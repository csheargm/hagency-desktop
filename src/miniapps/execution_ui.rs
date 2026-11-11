//! Trusted Start/Resume panel for an existing approved native assignment.
use super::*;
#[cfg(not(feature = "shared-successors"))]
pub(super) struct ExecutionPrompt;
#[cfg(not(feature = "shared-successors"))]
impl MiniAppsPanel {
    pub(super) fn begin_execution_confirmation(
        &mut self,
        _: &mut Cx,
        _: &str,
        _: Value,
        _: (usize, u64),
    ) -> Result<(), String> {
        Err("shared_successors_disabled".into())
    }
    pub(super) fn present_execution_confirmation(&mut self, cx: &mut Cx) {
        self.view
            .view(cx, ids!(execution_confirmation))
            .set_visible(cx, false);
    }
    pub(super) fn poll_execution_confirmation(&mut self, _: &mut Cx) {}
    pub(super) fn close_execution_confirmation(&mut self, _: &mut Cx) {}
    pub(super) fn execution_details(&mut self, _: &mut Cx) {}
    pub(super) fn execution_primary(&mut self, _: &mut Cx) {}
    pub(super) fn execution_recover(&mut self, _: &mut Cx) {}
    pub(super) fn execution_key(&mut self, _: &mut Cx, _: &Event) -> bool {
        false
    }
}
#[cfg(feature = "shared-successors")]
pub(super) use enabled::ExecutionPrompt;
#[cfg(feature = "shared-successors")]
mod enabled {
    use super::*;
    use crate::host::hagency::execution::{ExecutionFlow, ExecutionReview, ExecutionAssessment};
    use ::hagency::native_owner::{SharedSuccessorBudgetReview, SharedSuccessorBlockReason};
    pub(crate) struct ExecutionPrompt {
        flow: ExecutionFlow,
        reply: (usize, u64),
        phase: Phase,
        receiver: Option<Receiver<Result<Outcome, String>>>,
        attempted: bool,
        expanded: bool,
    }
    enum Phase {
        Loading,
        Review(ExecutionReview),
        Blocked(SharedSuccessorBudgetReview),
        Progress(Value),
        Failed(String),
    }
    enum Outcome {
        Assessment(ExecutionAssessment),
        Progress(Value),
    }
    fn failure(code: &str) -> &'static str {
        if let Some(message)=crate::host::hagency::errors::message(code){return message;}
        match code {
            "shared_successor_preparation_changed" => {
                "This review expired or changed. Close it and review the same assignment again."
            }
            "shared_successor_capacity_unavailable" => {
                "The previous process or its usage is unresolved, or no budget remains. No new start was prepared."
            }
            "shared_successor_issuance_unavailable" | "shared_successors_disabled" => {
                "New starts are unavailable under the current server policy. Saved original starts can still be recovered when supported."
            }
            "management_scope_changed" | "management_access_required" => {
                "The account or session changed. Open a fresh review after signing in."
            }
            _ => {
                "The result is not confirmed. Recover the saved original start; do not create a replacement request."
            }
        }
    }
    fn safe_code(code: &str) -> &'static str {
        if let Some(code)=crate::host::hagency::errors::known_code(code){return code;}
        match code {
            "shared_successor_preparation_changed" => "shared_successor_preparation_changed",
            "shared_successor_capacity_unavailable" => "shared_successor_capacity_unavailable",
            "shared_successor_issuance_unavailable" => "shared_successor_issuance_unavailable",
            "shared_successors_disabled" => "shared_successors_disabled",
            "shared_successor_original_missing" => "shared_successor_original_missing",
            "management_scope_changed" => "management_scope_changed",
            "management_access_required" => "management_access_required",
            _ => "management_outcome_unknown",
        }
    }
    fn next_focus(current: Option<usize>, count: usize, reverse: bool) -> usize {
        match current {
            Some(i) => {
                if reverse {
                    (i + count - 1) % count
                } else {
                    (i + 1) % count
                }
            }
            None => {
                if reverse {
                    count - 1
                } else {
                    0
                }
            }
        }
    }
    fn progress_copy(v: &Value) -> &'static str {
        if v.pointer("/result/state").and_then(Value::as_str) == Some("no_saved_intent") {
            return "No original start is saved for this assignment. Resume did not create one.";
        }
        let p = v.get("progress").or_else(|| v.pointer("/result/progress"));
        match p.and_then(|p| p.get("state")).and_then(Value::as_str) {
            Some("running") => {
                "Active when checked."
            }
            Some("stopping") => {
                "Stopping when checked."
            }
            Some("recorded") => match p
                .and_then(|p| p.get("process_state"))
                .and_then(Value::as_str)
            {
                Some("stopped") => {
                    "Last recorded: stopped."
                }
                Some("qualified") => {
                    "Last recorded: qualified. Current activity unknown."
                }
                Some("unknown") => {
                    "Last recorded: process outcome unknown."
                }
                _ => "No final process observation yet.",
            },
            Some("historical_evidence_missing") => {
                "Original process evidence is incomplete. No replacement process is authorized by this result."
            }
            _ => {
                "No active process was confirmed by this check. The original request remains recoverable."
            }
        }
    }
    fn blocked_copy(r: &SharedSuccessorBudgetReview) -> &'static str {
        match r.reason {
            SharedSuccessorBlockReason::Exhausted => {
                "The allocated budget is exhausted. No new start was prepared."
            }
            SharedSuccessorBlockReason::UsageUnresolved => {
                "Usage is not fully resolved. Retained tokens remain unavailable; no new start was prepared."
            }
        }
    }
    fn blocked_details(r: &SharedSuccessorBudgetReview) -> String {
        let tr = crate::i18n::tr;
        let remaining = r
            .remaining_tokens
            .map(crate::utils::format_token_count)
            .unwrap_or_else(|| tr("Unknown").into());
        format!(
            "{}\n{}\n\n{}\n{} / {}\n\n{} · {}\n{} · {}\n{} · {}",
            tr("Agent"),
            r.description.agent_name,
            tr("Remaining / allocated tokens"),
            remaining,
            crate::utils::format_token_count(r.allocated_tokens),
            tr("Known consumed tokens"),
            crate::utils::format_token_count(r.known_consumed_tokens),
            tr("Retained tokens"),
            crate::utils::format_token_count(r.retained_tokens),
            tr("Known usage above allocation"),
            crate::utils::format_token_count(r.known_overrun_tokens)
        )
    }
    fn progress_details(v: &Value) -> String {
        let tr = crate::i18n::tr;
        format!(
            "{}\n{}\n\n{}\n{}\n\n{}\n{}",
            tr("Agent"),
            v.get("agentName").and_then(Value::as_str).unwrap_or("—"),
            tr("Project ID"),
            v.get("projectId").and_then(Value::as_str).unwrap_or("—"),
            tr("Last checked (UTC)"),
            v.get("observedAtUtc")
                .and_then(Value::as_str)
                .unwrap_or("—")
        )
    }
    fn has_details(phase: &Phase) -> bool {
        matches!(
            phase,
            Phase::Review(_) | Phase::Blocked(_) | Phase::Progress(_)
        )
    }
    impl ExecutionPrompt {
        fn can_start(&self) -> bool {
            self.receiver.is_none()
                && self.flow.active()
                && matches!(&self.phase,Phase::Review(r) if !r.expired())
                && !self.attempted
        }
        fn can_resume(&self) -> bool {
            self.receiver.is_none()
                && self.flow.can_resume()
                && !matches!(self.phase, Phase::Loading | Phase::Review(_))
        }
    }
    fn close_reply(
        phase: Phase,
        attempted: bool,
        pending: bool,
        active: bool,
        assignment: &str,
    ) -> Result<String, String> {
        if !active {
            return Err(if attempted {
                "management_outcome_unknown"
            } else {
                "management_scope_changed"
            }
            .into());
        }
        let v = match phase {
            Phase::Progress(v) if !pending => v,
            _ => {
                json!({"state":if attempted{"outcome_unknown"}else{"review_closed"},"assignmentId":assignment})
            }
        };
        Ok(v.to_string())
    }
    impl MiniAppsPanel {
        pub(in crate::octoscript_apps::ui) fn begin_execution_confirmation(
            &mut self,
            cx: &mut Cx,
            service: &str,
            args: Value,
            reply: (usize, u64),
        ) -> Result<(), String> {
            if self.execution_confirmation.is_some()
                || self.grant_confirmation.is_some()
                || self.adoption_confirmation.is_some()
                || self.provider_confirmation.is_some() || self.account_login.is_some()
                || self.reauthentication.is_some()
                || self.project_room_reply.is_some()
                || self.pending.len() >= 16
            {
                return Err("provider_confirmation_busy".into());
            }
            let host = self.hagency.as_ref().ok_or("management_session_required")?;
            let flow = host.execution_handoff(
                self.lease.clone().ok_or("management_session_required")?,
                service,
                args,
            )?;
            let resume = flow.is_resume();
            self.execution_confirmation = Some(ExecutionPrompt {
                flow: flow.clone(),
                reply,
                phase: Phase::Loading,
                receiver: None,
                attempted: resume,
                expanded: false,
            });
            self.execution_job(async move {
                if resume {
                    flow.resume().await.map(Outcome::Progress)
                } else {
                    flow.prepare().await.map(Outcome::Assessment)
                }
            });
            self.present_execution_confirmation(cx);
            cx.set_key_focus(
                self.view
                    .button(cx, ids!(execution_confirmation.buttons.cancel))
                    .area(),
            );
            Ok(())
        }
        fn execution_job(
            &mut self,
            task: impl std::future::Future<Output = Result<Outcome, String>> + Send + 'static,
        ) {
            let Some(p) = &mut self.execution_confirmation else {
                return;
            };
            let (tx, rx) = mpsc::sync_channel(1);
            p.receiver = Some(rx);
            p.phase = Phase::Loading;
            crate::sliding_sync::spawn_async_task(async move {
                let result = task.await;
                if let Err(e) = &result {
                    let code = safe_code(e);
                    makepad_widgets::warning!("Native shared start result unavailable: {code}");
                }
                let _ = tx.try_send(result);
                SignalToUI::set_ui_signal();
            });
        }
        pub(in crate::octoscript_apps::ui) fn present_execution_confirmation(
            &mut self,
            cx: &mut Cx,
        ) {
            self.view
                .view(cx, ids!(execution_confirmation))
                .set_visible(cx, self.execution_confirmation.is_some());
            let Some(p) = &self.execution_confirmation else {
                return;
            };
            let tr = crate::i18n::tr;
            self.view.view(cx, ids!(app_content)).set_visible(cx, false);
            self.view.view(cx, ids!(approval)).set_visible(cx, false);
            self.view
                .label(cx, ids!(execution_confirmation.sheet_scroll.owner))
                .set_text(cx, p.flow.account());
            let (status, details) = match &p.phase {
                Phase::Loading => (
                    tr(if p.attempted {
                        "Recovering the original start or submitting the confirmed start…"
                    } else {
                        "Checking the original assignment and remaining budget…"
                    })
                    .to_owned(),
                    String::new(),
                ),
                Phase::Review(r) => {
                    let r = r.review();
                    (tr("Start another process for this approved agent on this device. The server checks current policy and authority when submitted.").to_owned(),format!("{}\n{}\n\n{}\n{} / {}\n\n{}\n{}\n\n{} · {}",tr("Agent"),r.agent_name,tr("Remaining / allocated tokens"),crate::utils::format_token_count(r.remaining_tokens),crate::utils::format_token_count(r.allocated_tokens),tr("Project ID"),r.project_id,tr("Known consumed tokens"),crate::utils::format_token_count(r.known_consumed_tokens)))
                }
                Phase::Blocked(r) => (tr(blocked_copy(r)).to_owned(), blocked_details(r)),
                Phase::Progress(v) => (tr(progress_copy(v)).to_owned(), progress_details(v)),
                Phase::Failed(code) => (
                    tr(failure(code)).to_owned(),
                    format!(
                        "{}\n{}\n\n{}",
                        tr("Assignment ID"),
                        p.flow.assignment(),
                        safe_code(code)
                    ),
                ),
            };
            self.view
                .label(cx, ids!(execution_confirmation.sheet_scroll.status))
                .set_text(cx, &status);
            self.view
                .label(cx, ids!(execution_confirmation.sheet_scroll.details.copy))
                .set_text(cx, &details);
            self.view
                .view(cx, ids!(execution_confirmation.sheet_scroll.details))
                .set_visible(cx, !details.is_empty());
            let technical = match &p.phase {
                Phase::Review(review) => {
                    let r = review.review();
                    format!(
                        "{}\n{}\n\n{}\n{}\n\n{} · {}\n{} · {}",
                        tr("Assignment ID"),
                        r.assignment_id,
                        tr("Previous process command"),
                        r.predecessor_command_id,
                        tr("Next process attempt"),
                        r.process_attempt,
                        tr("Retained tokens"),
                        crate::utils::format_token_count(r.retained_tokens)
                    )
                }
                Phase::Blocked(r) => format!(
                    "{}\n{}\n\n{}\n{}",
                    tr("Project ID"),
                    r.description.project_id,
                    tr("Assignment ID"),
                    r.description.assignment_id
                ),
                Phase::Progress(_) => format!("{}\n{}", tr("Assignment ID"), p.flow.assignment()),
                _ => String::new(),
            };
            self.view
                .widget(cx, ids!(execution_confirmation.sheet_scroll.toggle_details))
                .set_visible(cx, !technical.is_empty());
            self.view
                .button(cx, ids!(execution_confirmation.sheet_scroll.toggle_details))
                .set_text(
                    cx,
                    tr(if p.expanded {
                        "Hide request details"
                    } else {
                        "Show request details"
                    }),
                );
            self.view
                .label(cx, ids!(execution_confirmation.sheet_scroll.technical))
                .set_text(cx, &technical);
            self.view
                .widget(cx, ids!(execution_confirmation.sheet_scroll.technical))
                .set_visible(cx, p.expanded && !technical.is_empty());
            self.view
                .widget(cx, ids!(execution_confirmation.buttons.primary))
                .set_visible(cx, p.can_start());
            self.view
                .widget(cx, ids!(execution_confirmation.buttons.recover))
                .set_visible(cx, p.can_resume());
            self.request_confirmation_redraw(cx);
        }
        pub(in crate::octoscript_apps::ui) fn poll_execution_confirmation(&mut self, cx: &mut Cx) {
            let Some(p) = &mut self.execution_confirmation else {
                return;
            };
            let result = p.receiver.as_ref().and_then(|rx| match rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("management_outcome_unknown".into()))
                }
                Err(_) => None,
            });
            let mut changed = false;
            if let Some(result) = result {
                p.receiver = None;
                p.phase = match result {
                    Ok(Outcome::Assessment(ExecutionAssessment::Ready(r))) => Phase::Review(r),
                    Ok(Outcome::Assessment(ExecutionAssessment::Blocked(r))) => Phase::Blocked(r),
                    Ok(Outcome::Progress(v)) => Phase::Progress(v),
                    Err(e) => Phase::Failed(safe_code(&e).into()),
                };
                changed = true;
            }
            if matches!(&p.phase,Phase::Review(r) if r.expired()) {
                p.phase = Phase::Failed("shared_successor_preparation_changed".into());
                changed = true;
            }
            let retired_code = if p.attempted {
                "management_outcome_unknown"
            } else {
                "management_scope_changed"
            };
            if !p.flow.active() && !matches!(&p.phase,Phase::Failed(c) if c==retired_code) {
                p.phase = Phase::Failed(retired_code.into());
                changed = true;
            }
            if changed {
                self.present_execution_confirmation(cx)
            }
        }
        pub(in crate::octoscript_apps::ui) fn execution_details(&mut self, cx: &mut Cx) {
            if let Some(p) = &mut self.execution_confirmation
                && has_details(&p.phase)
            {
                p.expanded = !p.expanded;
                self.present_execution_confirmation(cx);
            }
        }
        pub(in crate::octoscript_apps::ui) fn execution_primary(&mut self, cx: &mut Cx) {
            self.poll_execution_confirmation(cx);
            let Some(p) = &mut self.execution_confirmation else {
                return;
            };
            if !p.can_start() {
                return;
            }
            let flow = p.flow.clone();
            if let Phase::Review(r) = std::mem::replace(&mut p.phase, Phase::Loading) {
                p.attempted = true;
                self.execution_job(async move { flow.start(r).await.map(Outcome::Progress) });
                self.present_execution_confirmation(cx);
            }
        }
        pub(in crate::octoscript_apps::ui) fn execution_recover(&mut self, cx: &mut Cx) {
            let Some(p) = &mut self.execution_confirmation else {
                return;
            };
            if !p.can_resume() {
                return;
            }
            let flow = p.flow.clone();
            p.attempted = true;
            self.execution_job(async move { flow.resume().await.map(Outcome::Progress) });
            self.present_execution_confirmation(cx);
        }
        pub(in crate::octoscript_apps::ui) fn close_execution_confirmation(&mut self, cx: &mut Cx) {
            let Some(p) = self.execution_confirmation.take() else {
                return;
            };
            let result = close_reply(
                p.phase,
                p.attempted,
                p.receiver.is_some(),
                p.flow.active(),
                p.flow.assignment(),
            );
            splash_host_respond(
                cx,
                p.reply.0,
                p.reply.1,
                result.as_ref().map(String::as_str).map_err(String::as_str),
            );
            self.present_execution_confirmation(cx);
            self.view.view(cx, ids!(app_content)).set_visible(cx, true);
            self.reply_redraw = cx.new_next_frame();
            self.request_confirmation_redraw(cx);
        }
        pub(in crate::octoscript_apps::ui) fn execution_key(
            &mut self,
            cx: &mut Cx,
            event: &Event,
        ) -> bool {
            let Some(p) = &self.execution_confirmation else {
                return false;
            };
            let Event::KeyDown(k) = event else {
                return false;
            };
            if k.key_code == KeyCode::Escape {
                self.close_execution_confirmation(cx);
                return true;
            }
            let mut targets = vec![
                self.view
                    .button(cx, ids!(execution_confirmation.buttons.cancel))
                    .area(),
            ];
            if p.can_start() {
                targets.push(
                    self.view
                        .button(cx, ids!(execution_confirmation.buttons.primary))
                        .area(),
                )
            }
            if p.can_resume() {
                targets.push(
                    self.view
                        .button(cx, ids!(execution_confirmation.buttons.recover))
                        .area(),
                )
            }
            if has_details(&p.phase) {
                targets.push(
                    self.view
                        .button(cx, ids!(execution_confirmation.sheet_scroll.toggle_details))
                        .area(),
                );
            }
            if k.key_code == KeyCode::Tab {
                let current = targets.iter().position(|a| cx.has_key_focus(*a));
                cx.set_key_focus(targets[next_focus(current, targets.len(), k.modifiers.shift)]);
                return true;
            }
            if matches!(k.key_code, KeyCode::ReturnKey | KeyCode::Space) {
                if has_details(&p.phase)
                    && cx.has_key_focus(
                        self.view
                            .button(cx, ids!(execution_confirmation.sheet_scroll.toggle_details))
                            .area(),
                    )
                {
                    self.execution_details(cx);
                    return true;
                }

                if cx.has_key_focus(targets[0]) {
                    self.close_execution_confirmation(cx);
                    return true;
                }
                if p.can_start()
                    && cx.has_key_focus(
                        self.view
                            .button(cx, ids!(execution_confirmation.buttons.primary))
                            .area(),
                    )
                {
                    self.execution_primary(cx);
                    return true;
                }
                if p.can_resume()
                    && cx.has_key_focus(
                        self.view
                            .button(cx, ids!(execution_confirmation.buttons.recover))
                            .area(),
                    )
                {
                    self.execution_recover(cx);
                    return true;
                }
            }
            false
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn exhausted() -> SharedSuccessorBudgetReview {
            SharedSuccessorBudgetReview {
                description: ::hagency::native_owner::SharedSuccessorDescription {
                    agent_name: "Original approved agent".into(),
                    project_id: "project_original".into(),
                    assignment_id: "assignment_original".into(),
                },
                reason: SharedSuccessorBlockReason::Exhausted,
                allocated_tokens: 4096,
                known_consumed_tokens: 4813,
                retained_tokens: 4813,
                remaining_tokens: Some(0),
                known_overrun_tokens: 717,
                usage_complete: true,
            }
        }
        #[test]
        fn native_execution_budget_keeps_overrun_and_unknown_distinct_without_start_handle() {
            let r = exhausted();
            let text = blocked_details(&r);
            for actual in ["Original approved agent", "0 M / 0.004096 M", "Known consumed tokens · 0.004813 M", "Known usage above allocation · 0.000717 M"] {
                assert!(text.contains(actual), "{text}");
            }
            assert!(blocked_copy(&r).contains("exhausted"));
            let result: Value = serde_json::from_str(
                &close_reply(
                    Phase::Blocked(r.clone()),
                    false,
                    false,
                    true,
                    "assignment_original",
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(result["state"], "review_closed");
            let mut uncertain = r;
            uncertain.reason = SharedSuccessorBlockReason::UsageUnresolved;
            uncertain.remaining_tokens = None;
            uncertain.usage_complete = false;
            assert!(blocked_copy(&uncertain).contains("not fully resolved"));
            assert!(blocked_details(&uncertain).contains("Unknown / 0.004096 M"));
            assert!(!blocked_details(&uncertain).contains("0 / 4096"));
            for key in [
                blocked_copy(&uncertain),
                "Known usage above allocation",
                "Last checked (UTC)",
            ] {
                assert_ne!(
                    crate::i18n::text_for(crate::i18n::Language::Chinese, key),
                    key
                );
            }
        }
        #[test]
        fn native_execution_recovered_process_state_is_descriptive_and_keeps_agent_context() {
            let mut v = json!({"state":"recovered","agentName":"Original approved agent","projectId":"project_original","assignmentId":"assignment_original","observedAtUtc":"2026-10-16T12:00:00Z","result":{"state":"resumed","progress":{"state":"recorded","command_id":"original_command","observation_id":"original_observation","process_state":"stopped"}}});
            assert!(progress_copy(&v).starts_with("Last recorded: stopped"));
            let text = progress_details(&v);
            assert!(
                text.contains("Original approved agent") && text.contains("2026-10-16T12:00:00Z")
            );
            assert!(!text.contains("assignment_original"));
            v["result"]["progress"]["process_state"] = json!("qualified");
            assert!(progress_copy(&v).contains("Current activity unknown"));
            v["result"]["progress"]
                .as_object_mut()
                .unwrap()
                .remove("process_state");
            assert!(progress_copy(&v).contains("No final process observation"));
            assert_eq!(v["result"]["progress"]["command_id"], "original_command");
        }
        #[test]
        fn native_execution_keyboard_keeps_close_start_and_optional_details_reachable() {
            // Review focus order is Close, Start, details. Enter is deliberately
            // handled separately; moving focus never consumes the review.
            let review = ["close", "start", "details"];
            let mut at = None;
            let mut visited = Vec::new();
            for _ in 0..4 {
                let next = next_focus(at, review.len(), false);
                visited.push(review[next]);
                at = Some(next);
            }
            assert_eq!(visited, ["close", "start", "details", "close"]);
            assert_eq!(review[next_focus(Some(0), 3, true)], "details");
            // Loading has only Close; failure/recovery adds the original Resume.
            assert_eq!(next_focus(Some(0), 1, true), 0);
            assert_eq!(next_focus(Some(1), 2, false), 0);
        }
        #[test]
        fn native_execution_close_retains_unknown_and_never_creates_a_start() {
            let parse = |s: String| serde_json::from_str::<Value>(&s).unwrap();
            assert_eq!(
                parse(close_reply(Phase::Loading, false, true, true, "original").unwrap())["state"],
                "review_closed"
            );
            assert_eq!(
                parse(close_reply(Phase::Loading, true, true, true, "original").unwrap())["state"],
                "outcome_unknown"
            );
            assert_eq!(
                close_reply(Phase::Loading, true, true, false, "original").unwrap_err(),
                "management_outcome_unknown"
            );
            assert_eq!(
                parse(
                    close_reply(
                        Phase::Progress(
                            json!({"state":"recovered","result":{"state":"no_saved_intent"}})
                        ),
                        true,
                        false,
                        true,
                        "original"
                    )
                    .unwrap()
                )["result"]["state"],
                "no_saved_intent"
            );
            assert_eq!(
                safe_code("secret provider stderr"),
                "management_outcome_unknown"
            );
            assert!(
                progress_copy(&json!({"result":{"state":"no_saved_intent"}}))
                    .contains("did not create")
            );
            assert!(
                progress_copy(&json!({"progress":{"state":"running"}})).contains("when checked")
            );
        }
    }
}
