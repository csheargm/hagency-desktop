//! Offline provider peer for the complete native room-history execution path.
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

#[derive(Default)]
pub(super) struct Probe {
    index: usize,
    pending: bool,
    context: bool,
    blocked: usize,
    replayed: bool,
    sample: (u64, u64),
}
impl Probe {
    pub(super) fn begin(&mut self, context: bool) -> io::Result<()> {
        self.context = context;
        if context {
            let usage = json!({"totalTokens":300,"inputTokens":200,"cachedInputTokens":100,"cacheWriteInputTokens":20,"outputTokens":100,"reasoningOutputTokens":0});
            println!(
                "{}",
                json!({"method":"rawResponse/completed","params":{"threadId":"text-thread","turnId":"text-turn","responseId":"history-response","usage":usage}})
            );
            self.capacity(60_000, 900_000);
        }
        self.request(Value::Null)
    }
    fn capacity(&mut self, used: u64, total: u64) {
        self.sample = (used, total);
        let counters = |n| json!({"totalTokens":n,"inputTokens":n,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0});
        println!(
            "{}",
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"text-thread","turnId":"text-turn","tokenUsage":{"total":counters(total),"last":counters(used),"modelContextWindow":65_536}}})
        );
    }
    fn request(&mut self, cursor: Value) -> io::Result<()> {
        self.pending = true;
        println!(
            "{}",
            json!({"method":"item/started","params":{"threadId":"text-thread","turnId":"text-turn","startedAtMs":1,"item":{"id":format!("history-item-{}",self.index),"type":"dynamicToolCall","tool":"read_room_history","arguments":{"cursor":cursor},"status":"inProgress"}}})
        );
        println!(
            "{}",
            json!({"id":format!("history-{}",self.index),"method":"item/tool/call","params":{"threadId":"text-thread","turnId":"text-turn","callId":format!("history-call-{}",self.index),"tool":"read_room_history","arguments":{"cursor":cursor}}})
        );
        io::stdout().flush()
    }
    pub(super) fn response(&mut self, root: &Path, response: &Value) -> io::Result<()> {
        if !self.pending
            || response["id"] != format!("history-{}", self.index)
            || response["result"]["success"] != true
        {
            return Err(io::Error::other("unexpected history response"));
        }
        let text = response["result"]["contentItems"][0]["text"]
            .as_str()
            .ok_or_else(|| io::Error::other("history result missing"))?;
        if text.len() > 12 * 1024 {
            return Err(io::Error::other("history result exceeded its bound"));
        }
        let page: Value = serde_json::from_str(text)?;
        let mut pages = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("history-pages.jsonl"))?;
        writeln!(pages, "{page}")?;
        println!(
            "{}",
            json!({"method":"item/completed","params":{"threadId":"text-thread","turnId":"text-turn","completedAtMs":2,"item":{"id":format!("history-item-{}",self.index),"type":"dynamicToolCall","tool":"read_room_history","arguments":{"cursor":null},"status":"completed"}}})
        );
        self.pending = false;
        self.index += 1;
        if page["complete"] != true {
            if self.index >= (if self.context { 128 } else { 8 }) || !page["nextCursor"].is_string()
            {
                return Err(io::Error::other("history did not complete"));
            }
            if self.context && page["reason"] == "context_budget" {
                self.blocked += 1;
                if self.blocked > 1 && !self.replayed {
                    // Repeated telemetry must not restore input already read.
                    self.replayed = true;
                    self.capacity(self.sample.0, self.sample.1);
                } else {
                    // Simulate native compaction, not a second model start.
                    self.capacity(4_000, 900_000 + self.blocked as u64);
                }
            }
            return self.request(page["nextCursor"].clone());
        }
        if self.context && (self.blocked < 3 || !self.replayed) {
            return Err(io::Error::other(
                "context and duplicate capacity paths not exercised",
            ));
        }
        let answer = page["messages"]
            .as_array()
            .and_then(|messages| messages.iter().find_map(|message| message["body"].as_str()))
            .ok_or_else(|| io::Error::other("earlier history body missing"))?;
        let usage = json!({"totalTokens":300,"inputTokens":200,"cachedInputTokens":100,"cacheWriteInputTokens":20,"outputTokens":100,"reasoningOutputTokens":0});
        println!(
            "{}",
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":"text-thread","turnId":"text-turn","tokenUsage":{"total":usage,"last":usage,"modelContextWindow":131072}}})
        );
        println!(
            "{}",
            json!({"method":"item/started","params":{"threadId":"text-thread","turnId":"text-turn","startedAtMs":3,"item":{"type":"agentMessage","id":"answer","text":"","phase":"final_answer"}}})
        );
        println!(
            "{}",
            json!({"method":"item/agentMessage/delta","params":{"threadId":"text-thread","turnId":"text-turn","itemId":"answer","delta":answer}})
        );
        println!(
            "{}",
            json!({"method":"item/completed","params":{"threadId":"text-thread","turnId":"text-turn","completedAtMs":4,"item":{"type":"agentMessage","id":"answer","text":answer,"phase":"final_answer"}}})
        );
        println!(
            "{}",
            json!({"method":"turn/completed","params":{"threadId":"text-thread","turn":{"id":"text-turn","status":"completed","items":[]}}})
        );
        io::stdout().flush()
    }
}
