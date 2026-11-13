//! Bridge the shared OUP loop to original durable call custody. It owns no
//! route, activity lease or Matrix decryption authority by itself.
use super::*;
use hagency_agent_local::{codex, runtime::octos_turn};
use hagency_runtime::{octos::session::Observation, owned::{OwnedOctosSession, SharedOctosPolicy, SharedOctosShell}};
use super::claude_run::{authority::Live, bridge};
use serde_json::{json, Value};
use tokio::sync::{oneshot, watch};

pub(super) struct Host {
    pending: Option<SharedCallCustody>,
    send: Option<oneshot::Sender<SharedCallCustody>>,
    live: Live,
    policy: SharedOctosPolicy,
    work: SharedWorkItem,
    shell: Option<SharedOctosShell>,
}
pub(super) struct Capture {
    receive: Option<oneshot::Receiver<SharedCallCustody>>,
    sink: Option<SharedCallSink>,
    receipt: Option<SharedWorkReceipt>,
    progress: watch::Sender<tokio::time::Instant>,
}
pub(super) fn capture(live: Live, policy: SharedOctosPolicy, pending: SharedCallCustody) -> (Host, Capture) {
    let work = pending.work().clone();
    let (send, receive) = oneshot::channel();
    (Host { pending:Some(pending), send:Some(send), live, policy, work, shell:None }, Capture {
        receive:Some(receive), sink:None, receipt:None, progress:watch::channel(tokio::time::Instant::now()).0,
    })
}
impl Host {
    async fn approve_shell(&self, id: &str, session: &str, args: Value) -> bool {
        if self.live.proof().await.is_err() { return false; }
        let allowed = match self.policy {
            SharedOctosPolicy::Yolo { .. } => true,
            SharedOctosPolicy::Standard { .. } => {
                let request = json!({"id":id,"method":"octos/can_use_tool",
                    "params":{"sessionId":session,"toolName":"shell","input":args},
                    "sessionAllowed":false,"managerCanDecide":false});
                // Leave time to send the result and recheck the live lease.
                tokio::time::timeout(Duration::from_secs(45), bridge::permission(&self.live, &self.work, request))
                    .await.is_ok_and(|result| result.unwrap_or(false))
            }
            _ => false,
        };
        allowed && self.live.proof().await.is_ok()
    }
}
fn output_limit(value: &mut Value, tokens: Option<u64>) -> std::result::Result<(), String> {
    let Some(tokens) = tokens else { return Ok(()); };
    if !(1..=16384).contains(&tokens) { return Err("invalid_output_limit".into()); }
    let limit = (tokens as usize).saturating_mul(4);
    if let Some(text) = value.get("output").and_then(Value::as_str).map(str::to_owned)
        && text.len() > limit {
        let mut boundary = limit;
        while !text.is_char_boundary(boundary) { boundary -= 1; }
        value["output"] = json!(&text[..boundary]);
        value["truncated"] = json!(true);
    }
    Ok(())
}
impl octos_turn::Host for Host {
    async fn start(&mut self, process: &mut OwnedOctosSession, turn: &str, input: &str) -> codex::Result<()> {
        let custody = self.pending.as_ref().ok_or(codex::Error::Unknown)?;
        if input != custody.model_input() { return Err(codex::Error::Protocol("shared Octos input differs")); }
        custody.start_octos_turn(process, turn).await.map_err(|_| codex::Error::Unknown)?;
        let custody = self.pending.take().ok_or(codex::Error::Unknown)?;
        if let Err(custody) = self.send.take().ok_or(codex::Error::Unknown)?.send(custody) {
            self.pending = Some(custody);
            return Err(codex::Error::Unknown);
        }
        Ok(())
    }
    async fn permission(&mut self, id: &str, session: &str, params: Value) -> bool {
        let Some(tool) = params["tool_name"].as_str() else { return false; };
        if params["session_id"] != session || params["approval_id"] != id
            || !matches!(tool, "shell" | "web_search" | "web_fetch")
            || matches!(tool, "web_search" | "web_fetch") && !self.policy.web()
            || tool == "shell" && !self.policy.tools()
            || self.live.proof().await.is_err()
        { return false; }
        let allow = match self.policy {
            SharedOctosPolicy::Text => false,
            SharedOctosPolicy::LiveWeb => matches!(tool, "web_search" | "web_fetch"),
            SharedOctosPolicy::Yolo { .. } => true,
            SharedOctosPolicy::Standard { .. } => {
                let request = json!({"id":id,"method":"octos/can_use_tool",
                    "params":{"sessionId":session,"toolName":tool,"input":params},
                    "sessionAllowed":false,"managerCanDecide":false});
                tokio::time::timeout(Duration::from_secs(100), bridge::permission(&self.live, &self.work, request))
                    .await.is_ok_and(|result| result.unwrap_or(false))
            }
        };
        allow && self.live.proof().await.is_ok()
    }
    fn bind_shell(&mut self, shell: SharedOctosShell) -> codex::Result<()> {
        if self.shell.replace(shell).is_some() { return Err(codex::Error::Unknown); }
        Ok(())
    }
    async fn shell(&mut self, session: &str, id: &str, args: Value) -> std::result::Result<Value, String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input { command: String, timeout_secs: Option<u64> }
        let input: Input = serde_json::from_value(args.clone()).map_err(|_| "invalid_shell_request")?;
        let timeout = input.timeout_secs.unwrap_or(30);
        if input.command.is_empty() || input.command.len() > 16_000 || !(1..=90).contains(&timeout)
            || self.live.proof().await.is_err() { return Err("shell_request_refused".into()); }
        if !self.approve_shell(id, session, args).await { return Err("shell_owner_denied".into()); }
        let shell = self.shell.as_ref().ok_or("shell_unavailable")?;
        let result = shell.run(&input.command, Duration::from_secs(timeout)).await.map_err(str::to_owned)?;
        serde_json::to_value(result).map_err(|_| "shell_output_unavailable".into())
    }
    async fn exec_command(&mut self, session: &str, id: &str, args: Value) -> std::result::Result<Value, String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            cmd: String, workdir: Option<String>, tty: Option<bool>, yield_time_ms: Option<u64>,
            timeout_secs: Option<u64>, max_output_tokens: Option<u64>,
        }
        let input: Input = serde_json::from_value(args.clone()).map_err(|_| "invalid_shell_request")?;
        let timeout = input.timeout_secs.unwrap_or(300);
        let yield_ms = input.yield_time_ms.unwrap_or(1000);
        if input.cmd.is_empty() || input.cmd.len() > 16_000 || !(1..=1800).contains(&timeout)
            || yield_ms > 30_000 || input.max_output_tokens.is_some_and(|n| !(1..=16384).contains(&n)) {
            return Err("shell_request_refused".into());
        }
        if !self.approve_shell(id, session, args).await { return Err("shell_owner_denied".into()); }
        let shell = self.shell.as_ref().ok_or("shell_unavailable")?;
        let result = shell.exec(&input.cmd, input.workdir.as_deref(), input.tty.unwrap_or(false),
            Duration::from_millis(yield_ms), Duration::from_secs(timeout)).await.map_err(str::to_owned)?;
        let mut value = serde_json::to_value(result).map_err(|_| "shell_output_unavailable")?;
        output_limit(&mut value, input.max_output_tokens)?;
        Ok(value)
    }
    async fn write_stdin(&mut self, _session: &str, _id: &str, args: Value) -> std::result::Result<Value, String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input { session_id: String, chars: Option<String>, yield_time_ms: Option<u64>, max_output_tokens: Option<u64> }
        let input: Input = serde_json::from_value(args).map_err(|_| "invalid_shell_request")?;
        let yield_ms = input.yield_time_ms.unwrap_or(1000);
        if yield_ms > 30_000 || input.max_output_tokens.is_some_and(|n| !(1..=16384).contains(&n))
            || self.live.proof().await.is_err() { return Err("shell_request_refused".into()); }
        let shell = self.shell.as_ref().ok_or("shell_unavailable")?;
        let result = shell.write_stdin(&input.session_id, input.chars.as_deref().unwrap_or(""), Duration::from_millis(yield_ms))
            .await.map_err(str::to_owned)?;
        let mut value = serde_json::to_value(result).map_err(|_| "shell_output_unavailable")?;
        output_limit(&mut value, input.max_output_tokens)?;
        Ok(value)
    }
    async fn apply_patch(&mut self, _session: &str, _id: &str, args: Value) -> std::result::Result<Value, String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input { patch: Option<String>, path: Option<String>, diff: Option<String> }
        let input: Input = serde_json::from_value(args.clone()).map_err(|_| "invalid_patch_request")?;
        if self.live.proof().await.is_err()
            || !(input.patch.is_some() && input.path.is_none() && input.diff.is_none()
                || input.patch.is_none() && input.path.is_some() && input.diff.is_some()) {
            return Err("invalid_patch_request".into());
        }
        let shell = self.shell.as_ref().ok_or("patch_unavailable")?;
        let result = shell.apply_patch(&args).await.map_err(str::to_owned)?;
        if self.live.proof().await.is_err() { return Err("patch_interrupted".into()); }
        Ok(result)
    }
}
impl octos_turn::Capture for Capture {
    async fn bind(&mut self, process: &OwnedOctosSession) -> codex::Result<()> {
        self.progress.send_replace(tokio::time::Instant::now());
        let custody = self.receive.take().ok_or(codex::Error::Unknown)?.await.map_err(|_| codex::Error::Unknown)?;
        self.sink = Some(custody.bind_started_octos_turn(process).await.map_err(|_| codex::Error::Unknown)?);
        Ok(())
    }
    async fn observe(&mut self, observation: &Observation) -> codex::Result<()> {
        self.progress.send_replace(tokio::time::Instant::now());
        if let Some(receipt) = self.sink.as_mut().ok_or(codex::Error::Unknown)?
            .observe_octos(observation).await.map_err(|_| codex::Error::Unknown)? {
            self.receipt = Some(receipt);
        }
        Ok(())
    }
    async fn complete(&mut self, process: &OwnedOctosSession, text: &str) -> codex::Result<()> {
        self.receipt = Some(self.sink.as_mut().ok_or(codex::Error::Unknown)?
            .finish_octos(process, text).await.map_err(|_| codex::Error::Unknown)?);
        Ok(())
    }
}
impl Capture {
    pub fn progress(&self) -> watch::Receiver<tokio::time::Instant> { self.progress.subscribe() }
    /// Retain the original failure or unknown usage before stopping the child.
    pub async fn finish(&mut self, host: &mut Host) -> Result<Option<SharedWorkReceipt>, NativeError> {
        if let Some(sink) = &mut self.sink {
            if !self.receipt.as_ref().is_some_and(|r| matches!(r.state.as_str(), "completed" | "failed")) {
                self.receipt = Some(sink.record_unknown().await.map_err(store)?);
            }
        } else {
            let custody = host.pending.take().or_else(|| self.receive.as_mut().and_then(|r| r.try_recv().ok()));
            if let Some(custody) = custody {
                self.receipt = Some(custody.finish_failed_start().await.map_err(store)?);
            }
        }
        if let Some(shell) = &host.shell {
            shell.shutdown().await.map_err(|_| failure(409, "shared_octos_shell_cleanup_unknown"))?;
        }
        Ok(self.receipt.clone())
    }
}
