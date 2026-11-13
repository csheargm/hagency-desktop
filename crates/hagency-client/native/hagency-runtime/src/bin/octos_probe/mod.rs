//! Explicit offline peer mode; never invokes Octos or a provider. It speaks
//! the OUP stdio frames ADR-193 relies on, in the order Octos main sends them.
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, Write},
    path::Path,
};

/// The kernel's continuation turn in `background` (a fixed UUID).
const CONTINUATION: &str = "0192f0c1-0000-7000-8000-00000000c0de";
const MODES: [&str; 17] = [
    "normal",
    "host-tools",
    "quiet",
    "duplicate",
    "error",
    "unknown-usage",
    "unknown-usage-with-totals",
    "background",
    "approval",
    "approval-answer",
    "approval-twice",
    "approval-cancel",
    "refuse-open",
    "malformed",
    "stall",
    "late-totals",
    "ignore-generic-tools",
];

fn emit(value: Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}
fn notify(method: &str, params: Value) -> io::Result<()> {
    emit(json!({"jsonrpc":"2.0","method":method,"params":params}))
}
fn answer(request: &Value, result: Value) -> io::Result<()> {
    emit(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
}
fn refuse(request: &Value) -> io::Result<()> {
    emit(json!({"jsonrpc":"2.0","id":request["id"],
        "error":{"code":-32602,"message":"offline refusal","data":{"kind":"offline"}}}))
}
/// The next request, with a string ID.
fn request(stdin: &mut io::StdinLock<'_>) -> io::Result<Value> {
    let mut line = String::new();
    if stdin.read_line(&mut line)? == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let request: Value = serde_json::from_str(&line)?;
    if request["jsonrpc"] != "2.0" || !request["id"].is_string() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(request)
}
/// The next request, which must be `method` with a string ID.
fn expect(stdin: &mut io::StdinLock<'_>, method: &str) -> io::Result<Value> {
    let request = request(stdin)?;
    if request["method"] != method {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(request)
}
/// Every request Hagency sent, one JSON line each, for the test to compare.
fn record(marker: &Path, request: &Value) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker.with_extension("requests"))?;
    serde_json::to_writer(&mut file, request)?;
    file.write_all(b"\n")
}
struct Stream {
    session: String,
    seq: u64,
}
impl Stream {
    fn envelope(&mut self, thread: &str, turn: &str, kind: &str, data: Value) -> io::Result<u64> {
        self.seq += 1;
        self.redeliver(thread, self.seq, turn, kind, data)?;
        Ok(self.seq)
    }
    fn redeliver(
        &self,
        thread: &str,
        seq: u64,
        turn: &str,
        kind: &str,
        data: Value,
    ) -> io::Result<u64> {
        notify(
            "projection/envelope",
            json!({"session_id":self.session,"thread_id":thread,"seq":seq,"turn_id":turn,
                "payload":{"type":kind,"data":data}}),
        )?;
        Ok(seq)
    }
    fn orchestration(&self, active: bool, agents: u32, continuations: u32) -> io::Result<()> {
        notify(
            "session/orchestration",
            json!({"session_id":self.session,"active":active,"running_agents":agents,
                "pending_continuations":continuations}),
        )
    }
    fn persisted(&mut self, turn: &str, text: &str) -> io::Result<u64> {
        let id = format!("message-{}", self.seq + 1);
        self.envelope(
            "main",
            turn,
            "assistant_persisted",
            json!({"text":text,"assistant_segment_id":id,
                "meta":{"message_id":id,"persisted_at":"2026-10-08T00:00:00Z"}}),
        )
    }
    fn tool(&mut self, turn: &str) -> io::Result<u64> {
        self.envelope(
            "main",
            turn,
            "tool_start",
            json!({"tool_call_id":"tool-1","name":"shell","arguments_preview":"ls"}),
        )
    }
    fn terminal(&mut self, turn: &str, data: Value) -> io::Result<u64> {
        self.envelope("main", turn, "turn_terminal", data)
    }
}
/// A shell approval as Octos main raises one, its typed details nested under
/// `command`.
fn shell_approval(stream: &Stream, id: &str, turn: &str, cwd: &Value) -> io::Result<()> {
    notify(
        "approval/requested",
        json!({"session_id":stream.session,"approval_id":id,"turn_id":turn,
            "tool_name":"shell","title":"Run a command","body":"rm -rf build",
            "approval_kind":"command","risk":"high",
            "typed_details":{"kind":"command",
                "command":{"command_line":"rm -rf build","cwd":cwd,"tool_call_id":"tool-1"}}}),
    )
}
/// The host's answer to one approval, recorded, then acknowledged as Octos
/// main does: its result, and the `approval/decided` every client sees.
fn answer_approval(
    stdin: &mut io::StdinLock<'_>,
    marker: &Path,
    stream: &Stream,
    id: &str,
) -> io::Result<bool> {
    let respond = expect(stdin, "approval/respond")?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker.with_extension("responses"))?;
    serde_json::to_writer(&mut file, &respond["params"])?;
    file.write_all(b"\n")?;
    if respond["params"]["approval_id"] != id || respond["params"]["session_id"] != stream.session {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let decision = respond["params"]["decision"].clone();
    answer(
        &respond,
        json!({"approval_id":id,"accepted":true,"status":"accepted","runtime_resumed":true}),
    )?;
    notify(
        "approval/decided",
        json!({"session_id":stream.session,"approval_id":id,"decision":decision}),
    )?;
    Ok(decision == "approve")
}
fn usage(input: u64, output: u64) -> Value {
    json!({"input_tokens":input,"output_tokens":output,"reasoning_tokens":2,
        "cache_read_tokens":20,"cache_write_tokens":30})
}
/// One `octos serve --stdio` session as Octos main runs it: hello, the
/// permission profile, `session/open` on the given workspace, one turn, then
/// the session totals once the host finds it idle.
pub(super) fn run(mode: &str, marker: &Path) -> io::Result<()> {
    if !MODES.contains(&mode) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    fs::write(marker.with_extension("entered"), b"offline")?;
    let mut stdin = io::stdin().lock();
    let hello = expect(&mut stdin, "client_hello")?;
    record(marker, &hello)?;
    if mode == "stall" {
        drop(stdin);
        return super::pulse(marker);
    }
    if mode == "malformed" {
        io::stdout().write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7}\n")?;
        io::stdout().flush()?;
        drop(stdin);
        return super::pulse(marker);
    }
    answer(
        &hello,
        json!({"type":"server_hello","transport":"stdio","client":hello["params"]["client"],
            "capabilities":{"version":{"protocol":"octos-ui/v1alpha1","schema_version":1,"jsonrpc":"2.0"},
                "capabilities_schema_version":1,"supported_methods":[],"supported_notifications":[]}}),
    )?;
    let set = expect(&mut stdin, "permission/profile/set")?;
    record(marker, &set)?;
    answer(
        &set,
        json!({"session_id":set["params"]["session_id"],"applied":true,
            "current":{"mode":set["params"]["update"]["mode"],"network":set["params"]["update"]["network"]}}),
    )?;
    let open = expect(&mut stdin, "session/open")?;
    record(marker, &open)?;
    if mode == "refuse-open" {
        refuse(&open)?;
        drop(stdin);
        return super::pulse(marker);
    }
    let params = &open["params"];
    // A writable session writes its workspace policy into a project that has
    // none, as Octos main does.
    if set["params"]["update"]["mode"] == "workspace_write" {
        let policy = Path::new(params["cwd"].as_str().ok_or(io::ErrorKind::InvalidInput)?)
            .join(".octos-workspace.toml");
        if !policy.exists() {
            fs::write(policy, "[workspace]\n")?;
        }
    }
    answer(
        &open,
        json!({"opened":{"session_id":params["session_id"],"active_profile_id":params["profile_id"],
            "workspace_root":params["cwd"]}}),
    )?;
    // Hagency's task tools, when registered on the session before its turn:
    // each is offered under its model name, as Octos main answers.
    let mut next = request(&mut stdin)?;
    if next["method"] == "peer/tools/register" {
        let register = next;
        record(marker, &register)?;
        let registered: Vec<Value> = register["params"]["tools"]
            .as_array()
            .ok_or(io::ErrorKind::InvalidInput)?
            .iter()
            .map(|tool| {
                json!({"name":tool["name"],"app":"hagency","risk":tool["risk"],
                    "model_name":tool["name"].as_str().unwrap_or_default().replace('.', "_")})
            })
            .collect();
        let generic = if mode == "ignore-generic-tools" { Value::Null }
            else { register["params"]["generic_tools"].clone() };
        answer(
            &register,
            json!({"session_id":register["params"]["session_id"],"version":1,"previous_version":0,
                "tools":registered,"generic_tools":generic,"applies":"next_turn"}),
        )?;
        next = request(&mut stdin)?;
    }
    if next["method"] != "turn/start" {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let start = next;
    record(marker, &start)?;
    fs::write(
        marker.with_extension("prompt"),
        start["params"]["input"][0]["text"]
            .as_str()
            .ok_or(io::ErrorKind::InvalidInput)?,
    )?;
    let turn = start["params"]["turn_id"]
        .as_str()
        .ok_or(io::ErrorKind::InvalidInput)?
        .to_owned();
    let mut stream = Stream {
        session: params["session_id"]
            .as_str()
            .ok_or(io::ErrorKind::InvalidInput)?
            .to_owned(),
        seq: 0,
    };
    answer(&start, json!({"accepted":true}))?;
    notify(
        "turn/started",
        json!({"session_id":stream.session,"turn_id":turn,"timestamp":"2026-10-08T00:00:00Z"}),
    )?;
    if mode != "quiet" {
        stream.orchestration(true, 0, 0)?;
    }
    // Two host tool calls: one the helper answers, one it refuses.
    if mode == "host-tools" {
        for (call, name, args) in [
            (
                "ptc-1",
                "hagency.get_task",
                json!({"task_id":"offline-task"}),
            ),
            ("ptc-2", "hagency.transition_task", json!({})),
        ] {
            notify(
                "peer/tool/call",
                json!({"peer":null,"session_id":stream.session,"turn_id":turn,"call_id":call,
                    "tool_call_id":call,"name":name,"app":"hagency","args":args,"risk":"read",
                    "confirm_required":false,"timeout_ms":60000,"tools_version":1}),
            )?;
            let result = expect(&mut stdin, "peer/tool/result")?;
            record(marker, &result)?;
            answer(&result, json!({"call_id":call,"accepted":true}))?;
        }
    }
    stream.envelope(
        "main",
        &turn,
        "user_message",
        json!({"text":"echo of the prompt"}),
    )?;
    stream.envelope(
        "main",
        &turn,
        "assistant_delta",
        json!({"text":"work","assistant_segment_id":"segment-1"}),
    )?;
    stream.persisted(&turn, "working")?;
    let tool = stream.tool(&turn)?;
    let mut approved = true;
    match mode {
        "approval" | "approval-cancel" => {
            shell_approval(&stream, "approval-1", &turn, &params["cwd"])?;
            if mode == "approval-cancel" {
                notify(
                    "approval/cancelled",
                    json!({"session_id":stream.session,"approval_id":"approval-1"}),
                )?;
                fs::write(marker.with_extension("cancelled"), b"offline")?;
            }
            drop(stdin);
            return super::pulse(marker);
        }
        // The same command twice: a task grant answers the second.
        "approval-answer" | "approval-twice" => {
            let count = if mode == "approval-twice" { 2 } else { 1 };
            for index in 1..=count {
                let id = format!("approval-{index}");
                shell_approval(&stream, &id, &turn, &params["cwd"])?;
                approved &= answer_approval(&mut stdin, marker, &stream, &id)?;
            }
            // The approved (or refused) command's own end, as Octos main
            // projects it once the tool returns.
            stream.envelope(
                "main",
                &turn,
                "tool_end",
                json!({"tool_call_id":"tool-1","status":if approved { "ok" } else { "error" }}),
            )?;
        }
        _ => {}
    }
    let reply = match mode {
        "background" => "spawned a helper",
        "approval-answer" | "approval-twice" if !approved => {
            "octos reply without the denied command"
        }
        "approval-answer" | "approval-twice" => "octos approved reply",
        _ => "octos fixture reply",
    };
    stream.persisted(&turn, reply)?;
    match mode {
        "error" => {
            stream.terminal(
                &turn,
                json!({"outcome":"errored","error":{"code":"provider_unavailable","message":"offline"},
                    "token_usage":usage(10, 7)}),
            )?;
        }
        "unknown-usage" | "unknown-usage-with-totals" => {
            stream.terminal(&turn, json!({"outcome":"completed"}))?;
        }
        _ => {
            stream.terminal(
                &turn,
                json!({"outcome":"completed","token_usage":usage(10, 7)}),
            )?;
        }
    }
    if mode == "duplicate" {
        // Redelivery repeats a pair the client already has; a second terminal
        // for the same turn cannot replace the first.
        stream.redeliver(
            "main",
            tool,
            &turn,
            "tool_start",
            json!({"tool_call_id":"tool-1","name":"shell"}),
        )?;
        stream.terminal(
            &turn,
            json!({"outcome":"errored","token_usage":usage(1000, 1000)}),
        )?;
    }
    if mode == "background" {
        // A sub-agent outlives the turn; its child stream is not a turn.
        stream.orchestration(true, 1, 0)?;
        stream.envelope(
            "child-1",
            "child-stream-1",
            "assistant_persisted",
            json!({"text":"child text","assistant_segment_id":"child","meta":{"message_id":"child","persisted_at":"2026-10-08T00:00:00Z"}}),
        )?;
        stream.envelope(
            "child-1",
            "child-stream-1",
            "turn_terminal",
            json!({"outcome":"completed","token_usage":usage(500, 500)}),
        )?;
        stream.orchestration(true, 0, 1)?;
        notify(
            "turn/started",
            json!({"session_id":stream.session,"turn_id":CONTINUATION,"timestamp":"2026-10-08T00:00:01Z"}),
        )?;
        stream.tool(CONTINUATION)?;
        stream.persisted(CONTINUATION, "octos background reply")?;
        stream.terminal(
            CONTINUATION,
            json!({"outcome":"completed","token_usage":usage(5, 3)}),
        )?;
    }
    if mode != "quiet" {
        stream.orchestration(false, 0, 0)?;
    }
    let status = expect(&mut stdin, "session/status/read")?;
    record(marker, &status)?;
    let totals = match mode {
        "unknown-usage" => json!({}),
        // Sub-agents' usage is in the session's totals only.
        "background" => json!({"input_tokens":915,"output_tokens":610,
            "cached_input_tokens":60,"cache_write_input_tokens":90}),
        // A ledger behind the turns' own reports.
        "late-totals" => json!({"input_tokens":1,"output_tokens":1,
            "cached_input_tokens":0,"cache_write_input_tokens":0}),
        _ => json!({"input_tokens":10,"output_tokens":7,
            "cached_input_tokens":20,"cache_write_input_tokens":30}),
    };
    // Before the answer: a host holding the totals finds the marker written.
    fs::write(marker.with_extension("idle"), b"offline")?;
    answer(
        &status,
        json!({"session_id":stream.session,"usage":totals,"health":{"status":"ok"}}),
    )?;
    // Alive after the session went idle and after stdin closes: only the
    // original guardian's stop or the bounded fixture fuse ends the pulse.
    drop(stdin);
    super::pulse(marker)
}
