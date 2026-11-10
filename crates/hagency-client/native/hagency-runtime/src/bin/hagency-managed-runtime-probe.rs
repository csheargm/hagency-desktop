//! Isolated supervised fixture: no credentials, model or production entrypoint.
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, Write},
    path::Path,
    time::Duration,
};
#[path = "history_probe/mod.rs"]
mod history_probe;
#[path = "file_probe/mod.rs"]
mod file_probe;
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    #[cfg(unix)]
    if args.first().is_some_and(|v| v == "guardian") {
        return hagency_platform::run_guardian();
    }
    let [mode, root, overrides @ ..] = args.as_slice() else {
        return Err(io::Error::other("offline fixture only"));
    };
    let mode = mode.to_string_lossy();
    let root = Path::new(root);
    let overrides = if overrides.starts_with(&[
        "app-server".into(), "--listen".into(), "stdio://".into(),
    ]) {
        &overrides[3..]
    } else {
        overrides
    };
    fs::write(root.join("process-id"), std::process::id().to_string())?;
    let mut quota = 0;
    let mut history = history_probe::Probe::default();
    let mut files = file_probe::Probe::default();
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("methods"))?;
    for line in io::stdin().lock().lines() {
        let v: Value = serde_json::from_str(&line?).map_err(io::Error::other)?;
        if mode.starts_with("text-workspace-files") && v.get("method").is_none() {
            files.response(root,&v)?;
            continue;
        }
        if mode.starts_with("text-room-history") && v.get("method").is_none() {
            history.response(root, &v)?;
            continue;
        }
        if mode.starts_with("text-question") && v.get("method").is_none() {
            // The host's one answer to the model's question: record it and
            // finish the turn with exactly what was answered.
            question_answered(root, &v)?;
            continue;
        }
        let Some(method) = v["method"].as_str() else {
            continue;
        };
        writeln!(log, "{method}")?;
        log.flush()?;
        if method == "initialized" {
            continue;
        }
        if (mode.starts_with("text-workspace")
            || mode.starts_with("text-question")
            || mode.starts_with("text-room-history"))
            && matches!(method, "thread/start" | "turn/start")
        {
            let name = if method == "thread/start" {
                "thread-request.json"
            } else {
                "turn-request.json"
            };
            fs::write(root.join(name), serde_json::to_vec(&v["params"])?)?;
        }
        if mode.starts_with("text-room-history")
            && method == "thread/start"
            && v["params"]["dynamicTools"][0]["name"] != "read_room_history"
        {
            return Err(io::Error::other(
                "room history tool was not registered by the execution driver",
            ));
        }
        if (mode == "delay" && method == "account/rateLimits/read")
            || (mode == "delay-config" && method == "config/read")
        {
            std::thread::sleep(Duration::from_secs(30));
        }
        if mode == "text-held-initialize" && method == "initialize" {
            let until = std::time::Instant::now() + Duration::from_secs(8);
            while !root.join("release-initialization").exists() && std::time::Instant::now() < until
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if mode == "text-held-qualify" && method == "account/rateLimits/read" {
            let until = std::time::Instant::now() + Duration::from_secs(8);
            while !root.join("release-qualification").exists() && std::time::Instant::now() < until
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if mode == "slow-quota" && method == "account/rateLimits/read" {
            std::thread::sleep(Duration::from_millis(2200));
        }
        if mode == "request" && method == "config/read" {
            println!(
                "{}",
                json!({"id":"bad-request","method":"item/commandExecution/requestApproval","params":{}})
            );
            io::stdout().flush()?;
        }
        if mode == "text-start-renew" && method == "turn/start" {
            std::thread::sleep(Duration::from_secs(12));
        }
        if mode == "text-start-interrupt-usage" && method == "turn/start" {
            std::thread::sleep(Duration::from_secs(7));
        }
        if mode == "text-native-session" && native_session_request(root, &v)? {
            continue;
        }
        let value = match method {
            "turn/interrupt" if mode.ends_with("interrupt-usage") => json!({}),
            "initialize" => {
                json!({"userAgent":"managed-offline-fixture/0.167.0","platformFamily":"unix","platformOs":"fixture","codexHome":"/fixture"})
            }
            "config/read" => {
                let mut config: Value =
                    serde_json::from_slice(&fs::read(root.join("config.json"))?)
                        .map_err(io::Error::other)?;
                for pair in overrides.as_chunks::<2>().0 {
                    if pair[0] != "-c" {
                        return Err(io::Error::other("fixture override"));
                    }
                    let setting = pair[1].to_string_lossy();
                    let (key, value) = setting
                        .split_once('=')
                        .ok_or(io::Error::other("fixture override"))?;
                    let keys: Vec<_> = key.split('.').collect();
                    match keys.as_slice() {
                        ["features", name] if matches!(value, "false" | "true") => {
                            config["features"][*name] = (value == "true").into()
                        }
                        ["mcp_servers", name, "enabled"] if value == "false" => {
                            config["mcp_servers"][*name]["enabled"] = false.into()
                        }
                        ["web_search" | "model_instructions_file" | "model_provider"
                            | "chatgpt_base_url" | "cli_auth_credentials_store"] => {
                            let value: String =
                                serde_json::from_str(value).map_err(io::Error::other)?;
                            config[key] = value.into();
                        }
                        _ => return Err(io::Error::other("fixture override")),
                    }
                }
                json!({"config":config})
            }
            "account/read" => {
                json!({"requiresOpenaiAuth":true,"account":{"type":"chatgpt","email":null,"planType":"plus"}})
            }
            "account/rateLimits/read" => {
                quota += 1;
                json!({"accountId":if mode=="changed-account"&&quota>1 {"fixture_changed"}else{"fixture_account"},"rateLimits":{}})
            }
            "thread/start" if mode.starts_with("text-workspace") => {
                let sandbox = match mode.as_ref() {
                    "text-workspace-standard" | "text-workspace-standard-usage" | "text-workspace-files-standard" => {
                        json!({"type":"workspaceWrite","writableRoots":[v["params"]["cwd"]],"networkAccess":false})
                    }
                    "text-workspace-yolo" | "text-workspace-yolo-usage" | "text-workspace-files-yolo" => {
                        json!({"type":"dangerFullAccess"})
                    }
                    _ => json!({"type":"readOnly","networkAccess":false}),
                };
                json!({"thread":{"id":"text-thread","cwd":v["params"]["cwd"],"status":{"type":"idle"},"turns":[]},"cwd":v["params"]["cwd"],"model":v["params"]["model"],"modelProvider":"openai","approvalPolicy":v["params"]["approvalPolicy"],"approvalsReviewer":"user","sandbox":sandbox})
            }
            "turn/start" if mode.starts_with("text-workspace") => {
                let thread: Value =
                    serde_json::from_slice(&fs::read(root.join("thread-request.json"))?)?;
                if v["params"]["cwd"] != thread["cwd"] {
                    return Err(io::Error::other("room workspace changed within turn"));
                }
                json!({"turn":{"id":"text-turn","items":[],"status":"inProgress"}})
            }
            "thread/start" if mode.starts_with("text") => {
                if v["params"]["sandbox"] != "read-only"
                    || v["params"]["approvalPolicy"] != "on-request"
                {
                    return Err(io::Error::other("fixture text policy"));
                }
                json!({"thread":{"id":"text-thread","cwd":v["params"]["cwd"],"status":{"type":"idle"},"turns":[]},"cwd":v["params"]["cwd"],"model":v["params"]["model"],"modelProvider":"openai","approvalPolicy":"on-request","approvalsReviewer":"user","sandbox":{"type":"readOnly","networkAccess":false}})
            }
            "turn/start" if mode.starts_with("text") => {
                if v["params"]["sandboxPolicy"]["type"] != "readOnly"
                    || v["params"]["sandboxPolicy"]["networkAccess"] != false
                {
                    return Err(io::Error::other("fixture turn policy"));
                }
                json!({"turn":{"id":"text-turn","items":[],"status":"inProgress"}})
            }
            _ => return Err(io::Error::other("fixture received non-metadata command")),
        };
        println!("{}", json!({"id":v["id"],"result":value}));
        if method == "turn/interrupt" && mode.ends_with("interrupt-usage") {
            let usage = json!({"totalTokens":27,"inputTokens":20,"cachedInputTokens":0,"cacheWriteInputTokens":0,"outputTokens":7,"reasoningOutputTokens":0});
            println!(
                "{}",
                json!({"method":"thread/tokenUsage/updated","params":{"threadId":"text-thread","turnId":"text-turn","tokenUsage":{"total":usage,"last":usage,"modelContextWindow":10000}}})
            );
            println!(
                "{}",
                json!({"method":"turn/completed","params":{"threadId":"text-thread","turn":{"id":"text-turn","status":"interrupted","items":[]}}})
            );
            io::stdout().flush()?;
            continue;
        }
        if method == "turn/start" && mode.starts_with("text") {
            io::stdout().flush()?;
            if mode.starts_with("text-workspace-files") {
                files.begin()?;
                continue;
            }
            if mode.starts_with("text-room-history") {
                history.begin(mode == "text-room-history-context-usage")?;
                continue;
            }
            if mode.starts_with("text-question") {
                println!(
                    "{}",
                    json!({"id":"question-1","method":"item/tool/requestUserInput","params":question_params(&mode)})
                );
                if mode == "text-question-withdrawn" {
                    // Codex withdraws the question before any answer and
                    // finishes the turn on its own.
                    finish_question_turn("Answer: withdrawn")?;
                }
                io::stdout().flush()?;
                continue;
            }
            if mode.ends_with("interrupt-usage") {
                continue;
            }
            if matches!(&*mode, "text-block" | "text-renew") {
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_secs(if mode == "text-renew" { 12 } else { 5 });
                while !root.join("release-text").exists() {
                    if std::time::Instant::now() >= deadline {
                        return Err(io::Error::other("fixture release missing"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            if mode == "text-web" {
                for (method, timestamp) in [
                    ("item/started", "startedAtMs"),
                    ("item/completed", "completedAtMs"),
                ] {
                    let mut params = json!({"threadId":"text-thread","turnId":"text-turn","item":{"type":"webSearch","id":"weather-search","query":"Beijing weather"}});
                    params[timestamp] = 1.into();
                    println!("{}", json!({"method":method,"params":params}));
                }
            }
            if mode == "text-context-workspace" {
                let cwd = Path::new(
                    v["params"]["cwd"]
                        .as_str()
                        .ok_or_else(|| io::Error::other("missing cwd"))?,
                );
                let marker = cwd.join("room-turn-count");
                let previous = fs::read_to_string(&marker)
                    .unwrap_or_default()
                    .parse::<u64>()
                    .unwrap_or(0);
                fs::write(marker, (previous + 1).to_string())?;
                let mut paths = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("room-paths"))?;
                writeln!(paths, "{}", serde_json::to_string(cwd)?)?;
            }
            let answer = if mode.starts_with("text-context") {
                let input = v["params"]["input"][0]["text"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("context input missing"))?;
                let mut inputs = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("inputs"))?;
                writeln!(
                    inputs,
                    "{}",
                    serde_json::to_string(input).map_err(io::Error::other)?
                )?;
                let (_, value) = input
                    .split_once('\n')
                    .ok_or_else(|| io::Error::other("context prefix missing"))?;
                let value: Value = serde_json::from_str(value).map_err(io::Error::other)?;
                let request = value["currentRequest"]["body"].as_str().unwrap_or_default();
                if let Some(name) = request.strip_prefix("Remember the name ") {
                    format!("The name is {name}")
                } else {
                    value["context"]["history"]
                        .as_array()
                        .and_then(|h| h.last())
                        .and_then(|p| p["delivery"]["body"].as_str())
                        .unwrap_or("No prior delivered name.")
                        .into()
                }
            } else if mode == "text-escaped" {
                "\n".repeat(32 * 1024)
            } else if mode == "text-items" {
                "## 调研结果\n\n**完成**\n\n".to_owned() + &"正文\n\n".repeat(1200)
            } else {
                "Offline answer.".into()
            };
            if mode.starts_with("text-tree-") {
                let usage = json!({"totalTokens":14,"inputTokens":10,"cachedInputTokens":0,"outputTokens":4,"reasoningOutputTokens":1});
                for _ in 0..2 {
                    println!("{}", json!({"method":"rawResponse/completed","params":{"threadId":"text-thread","turnId":"text-turn","responseId":"root-response","usage":usage}}));
                }
                println!("{}", json!({"method":"turn/started","params":{"threadId":"child-thread","turn":{"id":"child-turn","rootTurnId":"text-turn","status":"inProgress","items":[]}}}));
                println!("{}", json!({"method":"thread/tokenUsage/updated","params":{"threadId":"child-thread","turnId":"child-turn","tokenUsage":{"total":{"totalTokens":99999}}}}));
            }
            if matches!(
                &*mode,
                "text-workspace-standard-usage"
                    | "text-workspace-yolo-usage"
                    | "text-start-renew"
                    | "text-small-usage"
                    | "text-usage"
                    | "text-long"
                    | "text-overflow"
                    | "text-stream"
                    | "text-escaped"
                    | "text-items"
                    | "text-renew"
                    | "text-overrun"
                    | "text-context"
                    | "text-context-workspace"
            ) {
                let usage = if mode == "text-overrun" {
                    json!({"totalTokens":4813,"inputTokens":4713,"cachedInputTokens":0,"cacheWriteInputTokens":0,"outputTokens":100,"reasoningOutputTokens":0})
                } else if mode == "text-small-usage" {
                    json!({"totalTokens":37,"inputTokens":30,"cachedInputTokens":10,"cacheWriteInputTokens":0,"outputTokens":7,"reasoningOutputTokens":0})
                } else {
                    json!({"totalTokens":300,"inputTokens":200,"cachedInputTokens":100,"cacheWriteInputTokens":20,"outputTokens":100,"reasoningOutputTokens":0})
                };
                println!(
                    "{}",
                    json!({"method":"thread/tokenUsage/updated","params":{"threadId":"text-thread","turnId":"text-turn","tokenUsage":{"total":usage,"last":usage,"modelContextWindow":10000}}})
                );
            }
            if mode == "text-items" {
                println!(
                    "{}",
                    json!({"method":"item/started","params":{"threadId":"text-thread","turnId":"text-turn","startedAtMs":1,"item":{"type":"agentMessage","id":"progress","text":"","phase":"commentary"}}})
                );
                for delta in ["正在", "调研。"] {
                    println!(
                        "{}",
                        json!({"method":"item/agentMessage/delta","params":{"threadId":"text-thread","turnId":"text-turn","itemId":"progress","delta":delta}})
                    );
                }
                println!(
                    "{}",
                    json!({"method":"item/completed","params":{"threadId":"text-thread","turnId":"text-turn","completedAtMs":2,"item":{"type":"agentMessage","id":"progress","text":"正在调研。","phase":"commentary"}}})
                );
            }
            println!(
                "{}",
                json!({"method":"item/started","params":{"threadId":"text-thread","turnId":"text-turn","startedAtMs":1,"item":{"type":"agentMessage","id":"answer","text":"","phase":"final_answer"}}})
            );
            let long_prefix = if mode == "text-long" || mode == "text-overflow" {
                let count = if mode == "text-long" { 6000 } else { 12000 };
                for _ in 0..count {
                    println!(
                        "{}",
                        json!({"method":"item/agentMessage/delta","params":{"threadId":"text-thread","turnId":"text-turn","itemId":"answer","delta":"研"}})
                    );
                }
                "研".repeat(count)
            } else {
                String::new()
            };
            if mode == "text-stream" {
                // Continuous output has no five-second idle gap for a renewal.
                for _ in 0..60 {
                    println!(
                        "{}",
                        json!({"method":"item/agentMessage/delta","params":{"threadId":"text-thread","turnId":"text-turn","itemId":"answer","delta":"x"}})
                    );
                    io::stdout().flush()?;
                    std::thread::sleep(std::time::Duration::from_millis(600));
                }
            }
            println!(
                "{}",
                json!({"method":"item/agentMessage/delta","params":{"threadId":"text-thread","turnId":"text-turn","itemId":"answer","delta":answer}})
            );
            let answer = long_prefix
                + &if mode == "text-stream" {
                    "x".repeat(60) + &answer
                } else {
                    answer
                };
            println!(
                "{}",
                json!({"method":"item/completed","params":{"threadId":"text-thread","turnId":"text-turn","completedAtMs":2,"item":{"type":"agentMessage","id":"answer","text":answer,"phase":"final_answer"}}})
            );
            println!(
                "{}",
                json!({"method":"turn/completed","params":{"threadId":"text-thread","turn":{"id":"text-turn","status":"completed","items":[]}}})
            );
            if mode.starts_with("text-tree-") {
                if mode == "text-tree-eof" {
                    io::stdout().flush()?;
                    return Ok(());
                }
                if mode == "text-tree-measured" {
                    let usage = json!({"totalTokens":14,"inputTokens":10,"cachedInputTokens":0,"outputTokens":4,"reasoningOutputTokens":1});
                    println!("{}", json!({"method":"rawResponse/completed","params":{"threadId":"child-thread","turnId":"child-turn","responseId":"child-response","usage":usage}}));
                }
                println!("{}", json!({"method":"turn/completed","params":{"threadId":"child-thread","turn":{"id":"child-turn","rootTurnId":"text-turn","status":"completed","items":[]}}}));
            }
        }
        io::stdout().flush()?;
    }
    Ok(())
}

/// The model's question for the `text-question*` modes (Codex 0.166 shape;
/// a Default-mode question is not blocking).
fn question_params(mode: &str) -> Value {
    let database = json!({"id":"db","header":"Database","question":"Which database should the service use?","isOther":true,"isSecret":false,
        "options":[{"label":"Postgres (Recommended)","description":"Durable and familiar."},{"label":"SQLite","description":"Simplest to run."}]});
    let token = json!({"id":"token","header":"Token","question":"Paste the deploy token","isOther":false,"isSecret":true,"options":null});
    // Codex 0.166 asks an MCP tool approval this way when MCP elicitation is off.
    let approval = json!({"id":"mcp_tool_call_approval_call-1","header":"Approve app tool call?","question":"Allow the deploy tool to run?","isOther":false,"isSecret":false,
        "options":[{"label":"Allow","description":"Run the tool and continue."},{"label":"Cancel","description":"Cancel this tool call"}]});
    let questions = match mode {
        "text-question-secret" => json!([database, token]),
        "text-question-only-secret" => json!([token]),
        "text-question-mcp-approval" => json!([approval]),
        _ => json!([database]),
    };
    json!({"threadId":"text-thread","turnId":"text-turn","itemId":"ask-1","isBlocking":false,"autoResolutionMs":null,"questions":questions})
}

fn question_answered(root: &Path, response: &Value) -> io::Result<()> {
    if response["id"] != "question-1" {
        return Err(io::Error::other("answer for an unknown question"));
    }
    let result = &response["result"];
    fs::write(
        root.join("question-response.json"),
        serde_json::to_vec(result)?,
    )?;
    finish_question_turn(&format!(
        "Answer: {}",
        serde_json::to_string(&result["answers"])?
    ))
}

/// Codex's resolution of the question, then the rest of the turn.
fn finish_question_turn(answer: &str) -> io::Result<()> {
    println!(
        "{}",
        json!({"method":"serverRequest/resolved","params":{"threadId":"text-thread","requestId":"question-1"}})
    );
    finish_text_turn(answer)
}
fn finish_text_turn(answer: &str) -> io::Result<()> {
    let usage = json!({"totalTokens":300,"inputTokens":200,"cachedInputTokens":100,"cacheWriteInputTokens":20,"outputTokens":100,"reasoningOutputTokens":0});
    println!(
        "{}",
        json!({"method":"thread/tokenUsage/updated","params":{"threadId":"text-thread","turnId":"text-turn","tokenUsage":{"total":usage,"last":usage,"modelContextWindow":10000}}})
    );
    println!(
        "{}",
        json!({"method":"item/started","params":{"threadId":"text-thread","turnId":"text-turn","startedAtMs":1,"item":{"type":"agentMessage","id":"answer","text":"","phase":"final_answer"}}})
    );
    println!(
        "{}",
        json!({"method":"item/agentMessage/delta","params":{"threadId":"text-thread","turnId":"text-turn","itemId":"answer","delta":answer}})
    );
    println!(
        "{}",
        json!({"method":"item/completed","params":{"threadId":"text-thread","turnId":"text-turn","completedAtMs":2,"item":{"type":"agentMessage","id":"answer","text":answer,"phase":"final_answer"}}})
    );
    println!(
        "{}",
        json!({"method":"turn/completed","params":{"threadId":"text-thread","turn":{"id":"text-turn","status":"completed","items":[]}}})
    );
    io::stdout().flush()
}

// Offline native persistence fixture. Every process reads its own retained
// transcript and emits original per-response receipts alongside old cumulative
// context totals, so resumption cannot accidentally charge the history again.
fn native_session_request(root: &std::path::Path, v: &Value) -> io::Result<bool> {
    let method=v["method"].as_str().unwrap_or("");
    let params=&v["params"];
    match method {
        "thread/start" | "thread/resume" => {
            let id=if method=="thread/start" {
                if params["ephemeral"]!=false { return Err(io::Error::other("native session must persist")); }
                let count=fs::read_to_string(root.join("native-counter")).ok().and_then(|v|v.parse::<u64>().ok()).unwrap_or(0)+1;
                fs::write(root.join("native-counter"),count.to_string())?;
                let id=format!("native-thread-{count}");
                fs::write(root.join(format!("{id}.json")),b"[]")?;
                id
            } else {
                let id=params["threadId"].as_str().ok_or_else(||io::Error::other("resume id"))?;
                if !id.starts_with("native-thread-") || id.contains('/') { return Err(io::Error::other("resume identity")); }
                if let Ok(message) = fs::read_to_string(root.join("native-resume-error")) {
                    println!("{}",json!({"id":v["id"],"error":{"code":-32600,"message":message}}));
                    io::stdout().flush()?;
                    return Ok(true);
                }
                if !root.join(format!("{id}.json")).exists() {
                    println!("{}",json!({"id":v["id"],"error":{"code":-32600,"message":format!("no rollout found for thread id {id}")}}));
                    io::stdout().flush()?;
                    return Ok(true);
                }
                fs::read(root.join(format!("{id}.json")))?;
                id.to_owned()
            };
            fs::write(root.join("native-current"),&id)?;
            println!("{}",json!({"id":v["id"],"result":{"thread":{"id":id,"cwd":params["cwd"],"status":{"type":"idle"},"turns":[]},"cwd":params["cwd"],"model":params["model"],"modelProvider":"openai","approvalPolicy":params["approvalPolicy"],"approvalsReviewer":"user","sandbox":{"type":"readOnly","networkAccess":false}}}));
        }
        "turn/start" => {
            let id=fs::read_to_string(root.join("native-current"))?;
            if params["threadId"]!=id { return Err(io::Error::other("native thread changed")); }
            let path=root.join(format!("{id}.json"));
            let mut inputs:Vec<String>=serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?;
            let text=params["input"][0]["text"].as_str().ok_or_else(||io::Error::other("input"))?;
            let answer=format!("Previous native inputs: {}",inputs.join("; "));
            inputs.push(text.into());
            fs::write(path,serde_json::to_vec(&inputs)?)?;
            let turn=format!("native-turn-{}",inputs.len());
            println!("{}",json!({"id":v["id"],"result":{"turn":{"id":turn,"items":[],"status":"inProgress"}}}));
            let old=if inputs.len()==1 { json!({"totalTokens":11,"inputTokens":8,"cachedInputTokens":0,"outputTokens":3,"reasoningOutputTokens":0}) } else { json!({"totalTokens":9000,"inputTokens":8000,"cachedInputTokens":0,"outputTokens":1000,"reasoningOutputTokens":0}) };
            println!("{}",json!({"method":"thread/tokenUsage/updated","params":{"threadId":id,"turnId":turn,"tokenUsage":{"total":old,"last":old,"modelContextWindow":20000}}}));
            let actual=json!({"totalTokens":11,"inputTokens":8,"cachedInputTokens":0,"outputTokens":3,"reasoningOutputTokens":0});
            println!("{}",json!({"method":"rawResponse/completed","params":{"threadId":id,"turnId":turn,"responseId":format!("response-{turn}"),"usage":actual}}));
            println!("{}",json!({"method":"item/started","params":{"threadId":id,"turnId":turn,"startedAtMs":1,"item":{"id":"answer","type":"agentMessage","text":"","phase":"final_answer"}}}));
            println!("{}",json!({"method":"item/agentMessage/delta","params":{"threadId":id,"turnId":turn,"itemId":"answer","delta":answer}}));
            println!("{}",json!({"method":"item/completed","params":{"threadId":id,"turnId":turn,"completedAtMs":2,"item":{"id":"answer","type":"agentMessage","text":answer,"phase":"final_answer"}}}));
            println!("{}",json!({"method":"turn/completed","params":{"threadId":id,"turn":{"id":turn,"status":"completed","items":[]}}}));
        }
        _ => return Ok(false),
    }
    io::stdout().flush()?;
    Ok(true)
}
