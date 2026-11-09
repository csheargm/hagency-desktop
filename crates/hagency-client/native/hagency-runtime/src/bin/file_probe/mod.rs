//! Offline Codex peer that consumes an uploaded binary and sends a derived file.
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

#[derive(Default)]
pub(super) struct Probe {
    step: usize,
    delivery: Option<String>,
    pending: Option<Value>,
}
impl Probe {
    pub(super) fn begin(&mut self) -> io::Result<()> {
        self.call("read_room_history", json!({"cursor":null}))
    }
    fn call(&mut self, tool: &str, arguments: Value) -> io::Result<()> {
        self.pending = Some(
            json!({"id":format!("file-item-{}",self.step),"type":"dynamicToolCall","tool":tool,"arguments":arguments,"status":"inProgress"}),
        );
        println!(
            "{}",
            json!({"method":"item/started","params":{"threadId":"text-thread","turnId":"text-turn","startedAtMs":1,
            "item":{"id":format!("file-item-{}",self.step),"type":"dynamicToolCall","tool":tool,"arguments":arguments,"status":"inProgress"}}})
        );
        println!(
            "{}",
            json!({"id":format!("file-{}",self.step),"method":"item/tool/call","params":{"threadId":"text-thread","turnId":"text-turn",
            "callId":format!("file-call-{}",self.step),"tool":tool,"arguments":arguments}})
        );
        io::stdout().flush()
    }
    pub(super) fn response(&mut self, root: &Path, response: &Value) -> io::Result<()> {
        if response["id"] != format!("file-{}", self.step) || response["result"]["success"] != true
        {
            return Err(io::Error::other("unexpected file callback"));
        }
        let value: Value = serde_json::from_str(
            response["result"]["contentItems"][0]["text"]
                .as_str()
                .ok_or_else(|| io::Error::other("file response missing"))?,
        )?;
        if self.step > 0 {
            let mut log = fs::OpenOptions::new().create(true).append(true)
                .open(root.join("file-responses.jsonl"))?;
            writeln!(log, "{value}")?;
        }
        let mut completed = self
            .pending
            .take()
            .ok_or_else(|| io::Error::other("no pending file item"))?;
        completed["status"] = "completed".into();
        println!(
            "{}",
            json!({"method":"item/completed","params":{"threadId":"text-thread","turnId":"text-turn","completedAtMs":2,"item":completed}})
        );
        let previous = self.step;
        self.step += 1;
        match previous {
            0 => {
                if !value["messages"].is_array() { return Err(io::Error::other("history unavailable")); }
                self.call("receive_file", json!({"event_id":"$foreign:test"}))
            }
            1 => {
                if value["error"] != "attachment_not_observed_in_this_room" {
                    return Err(io::Error::other("foreign attachment must fail"));
                }
                self.call("list_received_files", json!({}))
            }
            2 => {
                if value["files"][0]["event_id"] != "$attachment:test" {
                    return Err(io::Error::other("attachment inventory missing"));
                }
                self.call("receive_file", json!({"event_id":"$attachment:test"}))
            }
            3 => {
                let request: Value =
                    serde_json::from_slice(&fs::read(root.join("thread-request.json"))?)?;
                let workspace = Path::new(
                    request["cwd"]
                        .as_str()
                        .ok_or_else(|| io::Error::other("workspace missing"))?,
                );
                let path = value["path"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("received file path missing"))?;
                let mut bytes = fs::read(workspace.join(path))?;
                if bytes != b"original file\0\xff" {
                    return Err(io::Error::other("original binary was corrupted"));
                }
                bytes.extend_from_slice(b"\nprocessed by offline agent");
                fs::create_dir_all(workspace.join("output"))?;
                fs::write(workspace.join("output/result.bin"), bytes)?;
                self.call(
                    "send_file",
                    json!({"call_id":"file-result","path":"output/result.bin"}),
                )
            }
            4 => {
                if value["status"] != "delivered" {
                    return Err(io::Error::other("attachment was not delivered"));
                }
                self.delivery = Some(
                    value["delivery_id"]
                        .as_str()
                        .ok_or_else(|| io::Error::other("delivery id missing"))?
                        .into(),
                );
                self.call("get_file_delivery", json!({"delivery_id":self.delivery}))
            }
            5 => {
                if value["status"] != "delivered" {
                    return Err(io::Error::other("delivery receipt missing"));
                }
                self.call(
                    "send_file",
                    json!({"call_id":"file-result","path":"output/result.bin"}),
                )
            }
            6 => {
                if value["delivery_id"] != self.delivery.as_deref().unwrap()
                    || value["status"] != "delivered"
                {
                    return Err(io::Error::other("replay changed delivery"));
                }
                super::finish_text_turn("File received, processed and delivered")
            }
            _ => Err(io::Error::other("unexpected extra file operation")),
        }
    }
}
