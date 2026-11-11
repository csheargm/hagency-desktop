//! Trusted current-profile setup. Only the native UI can confirm this review.
use super::*;

#[cfg(not(feature = "resource-adoption"))]
pub(super) struct AdoptionPrompt;

#[cfg(not(feature = "resource-adoption"))]
impl MiniAppsPanel {
    pub(in crate::octoscript_apps::ui) fn begin_adoption_confirmation(
        &mut self,
        _: &mut Cx,
        _: Value,
        _: (usize, u64),
    ) -> Result<(), String> {
        Err("onboarding_unavailable".into())
    }
    pub(in crate::octoscript_apps::ui) fn present_adoption_confirmation(&mut self, cx: &mut Cx) {
        self.view
            .view(cx, ids!(adoption_confirmation))
            .set_visible(cx, false);
    }
    pub(in crate::octoscript_apps::ui) fn poll_adoption_confirmation(&mut self, _: &mut Cx) {}
    pub(in crate::octoscript_apps::ui) fn close_adoption_confirmation(&mut self, _: &mut Cx) {}
    pub(in crate::octoscript_apps::ui) fn adoption_primary(&mut self, _: &mut Cx) {}
    pub(in crate::octoscript_apps::ui) fn adoption_recover(&mut self, _: &mut Cx) {}
    pub(in crate::octoscript_apps::ui) fn adoption_key(&mut self, _: &mut Cx, _: &Event) -> bool {
        false
    }
}

#[cfg(feature = "resource-adoption")]
pub(super) use enabled::AdoptionPrompt;

#[cfg(feature = "resource-adoption")]
mod enabled {
    use super::*;
    use crate::host::hagency::adoption::{
        DeviceSetupContext, DeviceSetupFlow, DeviceSetupProgress, DeviceSetupResult, DeviceSetupReview,
    };

    pub(crate) struct AdoptionPrompt {
        flow: DeviceSetupFlow,
        reply: (usize, u64),
        phase: Phase,
        receiver: Option<Receiver<Result<DeviceSetupResult, String>>>,
    }
    enum Phase {
        Loading,
        Review(Box<DeviceSetupReview>),
        Progress(DeviceSetupProgress),
        Failed(String),
    }
    impl AdoptionPrompt {
        fn idle(&self) -> bool {
            self.receiver.is_none() && self.flow.active()
        }
        fn can_confirm(&self) -> bool {
            self.idle()
                && match &self.phase {
                    Phase::Review(review) => !review.expired(),
                    _ => false,
                }
        }
        fn can_recover(&self) -> bool {
            self.idle()
                && match &self.phase {
                    Phase::Failed(_) => true,
                    Phase::Progress(p) => p.state != "active",
                    _ => false,
                }
        }
    }
    fn message(code: &str) -> &'static str {
        if let Some(message)=crate::host::hagency::errors::message(code){return message;}
        match code {
            "current_profile_review_expired" => "This device setup review expired. Reopen the review to continue.",
            "management_scope_changed" | "management_access_required" =>
                "The account or session changed. Open a fresh review after signing in.",
            "onboarding_unavailable" => "Device setup is unavailable in this build.",
            "current_profile_review_changed" => "Device setup changed. Reopen the review to continue.",
            _ => "Device setup is not confirmed. Check its current status before continuing.",
        }
    }
    fn context_text(context: &DeviceSetupContext) -> String {
        context_text_for(context, crate::i18n::language())
    }
    fn context_text_for(context: &DeviceSetupContext, language: crate::i18n::Language) -> String {
        let tr = |english| crate::i18n::text_for(language, english);
        let mut rows = vec![
            format!("{}\n{}", tr("Matrix server"), context.server),
            format!("{}\n{}", tr("Resource owner"), context.account),
        ];
        rows.push(tr("Initialize local management for this Matrix account. Then add a provider account, sign in, and choose a model.").into());
        rows.push(tr("Device setup creates no resource or shared quota. Account limits are reviewed when you register a resource.").into());
        if let Some(id) = &context.initialization_id {
            rows.push(format!("{}\n{}", tr("Original setup request"), id));
        }
        rows.join("\n\n")
    }
    fn close_reply(phase: Phase, in_flight: bool, active: bool) -> Result<String, String> {
        if !active {
            return Err("management_scope_changed".into());
        }
        if in_flight {
            return Ok(json!({"state":"outcome_unknown"}).to_string());
        }
        match phase {
            Phase::Progress(progress) => serde_json::to_string(&progress)
                .map_err(|_| "management_outcome_unknown".to_owned()),
            Phase::Failed(code) => Ok(json!({"state":"outcome_unknown","reason":code}).to_string()),
            _ => Ok(json!({"state":"review_closed"}).to_string()),
        }
    }
    impl MiniAppsPanel {
        pub(in crate::octoscript_apps::ui) fn begin_adoption_confirmation(
            &mut self,
            cx: &mut Cx,
            args: Value,
            reply: (usize, u64),
        ) -> Result<(), String> {
            if self.execution_confirmation.is_some()
                || self.adoption_confirmation.is_some()
                || self.grant_confirmation.is_some()
                || self.provider_confirmation.is_some()
                || self.account_login.is_some()
                || self.reauthentication.is_some()
                || self.project_room_reply.is_some()
                || self.pending.len() >= 16
            {
                return Err("provider_confirmation_busy".into());
            }
            let flow = self
                .hagency
                .as_ref()
                .ok_or("management_session_required")?
                .onboarding_handoff(
                    self.lease.clone().ok_or("management_session_required")?,
                    args,
                )?;
            self.adoption_confirmation = Some(AdoptionPrompt {
                flow: flow.clone(),
                reply,
                phase: Phase::Loading,
                receiver: None,
            });
            self.adoption_job(async move { flow.open().await });
            self.present_adoption_confirmation(cx);
            cx.set_key_focus(
                self.view
                    .button(cx, ids!(adoption_confirmation.buttons.cancel))
                    .area(),
            );
            Ok(())
        }
        fn adoption_job(
            &mut self,
            task: impl std::future::Future<Output = Result<DeviceSetupResult, String>> + Send + 'static,
        ) {
            let Some(prompt) = &mut self.adoption_confirmation else {
                return;
            };
            let (tx, receiver) = mpsc::sync_channel(1);
            prompt.receiver = Some(receiver);
            prompt.phase = Phase::Loading;
            crate::sliding_sync::spawn_async_task(async move {
                let result = task.await;
                let _ = tx.try_send(result);
                SignalToUI::set_ui_signal();
            });
        }
        pub(in crate::octoscript_apps::ui) fn present_adoption_confirmation(
            &mut self,
            cx: &mut Cx,
        ) {
            self.view
                .view(cx, ids!(adoption_confirmation))
                .set_visible(cx, self.adoption_confirmation.is_some());
            let Some(prompt) = &self.adoption_confirmation else {
                return;
            };
            self.view.view(cx, ids!(app_content)).set_visible(cx, false);
            self.view.view(cx, ids!(approval)).set_visible(cx, false);
            self.view
                .label(cx, ids!(adoption_confirmation.sheet_scroll.owner))
                .set_text(cx, prompt.flow.account());
            let tr = crate::i18n::tr;
            let heading = if matches!(&prompt.phase, Phase::Progress(p) if p.state == "active") {
                "Device setup completed"
            } else {
                "Review device setup"
            };
            self.view.label(cx, ids!(adoption_confirmation.sheet_scroll.heading.title)).set_text(cx, tr(heading));
            let (mut status, details, primary) = match &prompt.phase {
                Phase::Loading => (tr("Checking device setup…").to_owned(), String::new(), "Set up this device"),
                Phase::Review(review) => (
                    tr("Initialize local management for this Matrix account. Then add a provider account, sign in, and choose a model.").to_owned(),
                    context_text(review.context()), "Set up this device",
                ),
                Phase::Progress(p) => (
                    tr(if p.state == "active" {
                        "Device setup is ready. You can add an account and register resources. Shared capacity requires a separate review."
                    } else {
                        "Device setup is not confirmed. Check its current status before continuing."
                    }).to_owned(),
                    p.initialization_id.as_ref().map(|id| format!("{}\n{}", tr("Original setup request"), id)).unwrap_or_default(),
                    "Set up this device",
                ),
                Phase::Failed(code) => (tr(message(code)).to_owned(), String::new(), "Set up this device"),
            };
            if !prompt.flow.active() {
                status = tr(message("management_scope_changed")).into();
            }
            self.view
                .label(cx, ids!(adoption_confirmation.sheet_scroll.status))
                .set_text(cx, &status);
            self.view
                .label(cx, ids!(adoption_confirmation.sheet_scroll.details.copy))
                .set_text(cx, &details);
            self.view
                .view(cx, ids!(adoption_confirmation.sheet_scroll.details))
                .set_visible(cx, !details.is_empty());
            self.view
                .button(cx, ids!(adoption_confirmation.buttons.primary))
                .set_text(cx, tr(primary));
            self.view
                .widget(cx, ids!(adoption_confirmation.buttons.primary))
                .set_visible(cx, prompt.can_confirm());
            self.view
                .widget(cx, ids!(adoption_confirmation.buttons.recover))
                .set_visible(cx, prompt.can_recover());
            self.request_confirmation_redraw(cx);
        }
        pub(in crate::octoscript_apps::ui) fn poll_adoption_confirmation(&mut self, cx: &mut Cx) {
            let Some(prompt) = &mut self.adoption_confirmation else {
                return;
            };
            let result = prompt.receiver.as_ref().and_then(|rx| match rx.try_recv() {
                Ok(value) => Some(value),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("management_outcome_unknown".into()))
                }
                Err(_) => None,
            });
            let mut changed = false;
            if let Some(result) = result {
                prompt.receiver = None;
                prompt.phase = match result {
                    Ok(DeviceSetupResult::Review(r)) => Phase::Review(r),
                    Ok(DeviceSetupResult::Progress(p)) => Phase::Progress(p),
                    Err(e) => Phase::Failed(e),
                };
                changed = true;
            }
            if matches!(&prompt.phase,Phase::Review(r) if r.expired()) {
                prompt.phase = Phase::Failed("current_profile_review_expired".into());
                changed = true;
            }
            if !prompt.flow.active()
                && !matches!(&prompt.phase,Phase::Failed(code) if code=="management_scope_changed")
            {
                prompt.phase = Phase::Failed("management_scope_changed".into());
                changed = true;
            }
            if changed {
                self.present_adoption_confirmation(cx);
            }
        }
        pub(in crate::octoscript_apps::ui) fn adoption_primary(&mut self, cx: &mut Cx) {
            self.poll_adoption_confirmation(cx);
            let Some(prompt) = &mut self.adoption_confirmation else {
                return;
            };
            if !prompt.can_confirm() {
                return;
            }
            let flow = prompt.flow.clone();
            match std::mem::replace(&mut prompt.phase, Phase::Loading) {
                Phase::Review(review) => {
                    self.adoption_job(async move { flow.confirm(*review).await })
                }
                phase => {
                    prompt.phase = phase;
                    return;
                }
            }
            self.present_adoption_confirmation(cx);
        }
        pub(in crate::octoscript_apps::ui) fn adoption_recover(&mut self, cx: &mut Cx) {
            let Some(prompt) = &self.adoption_confirmation else {
                return;
            };
            if !prompt.can_recover() {
                return;
            }
            let flow = prompt.flow.clone();
            self.adoption_job(async move { flow.open().await });
            self.present_adoption_confirmation(cx);
        }
        pub(in crate::octoscript_apps::ui) fn close_adoption_confirmation(&mut self, cx: &mut Cx) {
            let Some(prompt) = self.adoption_confirmation.take() else {
                return;
            };
            let active = prompt.flow.active();
            let result = close_reply(prompt.phase, prompt.receiver.is_some(), active);
            splash_host_respond(
                cx,
                prompt.reply.0,
                prompt.reply.1,
                result.as_ref().map(String::as_str).map_err(String::as_str),
            );
            self.present_adoption_confirmation(cx);
            self.view.view(cx, ids!(app_content)).set_visible(cx, true);
            self.reply_redraw = cx.new_next_frame();
            self.view.redraw(cx);
        }
        pub(in crate::octoscript_apps::ui) fn adoption_key(
            &mut self,
            cx: &mut Cx,
            event: &Event,
        ) -> bool {
            let Some(prompt) = &self.adoption_confirmation else {
                return false;
            };
            let Event::KeyDown(key) = event else {
                return false;
            };
            if key.key_code == KeyCode::Escape {
                self.close_adoption_confirmation(cx);
                return true;
            }
            let cancel = self
                .view
                .button(cx, ids!(adoption_confirmation.buttons.cancel))
                .area();
            let primary = self
                .view
                .button(cx, ids!(adoption_confirmation.buttons.primary))
                .area();
            let recover = self
                .view
                .button(cx, ids!(adoption_confirmation.buttons.recover))
                .area();
            let mut targets = Vec::new();
            targets.push(cancel);
            if prompt.can_confirm() {
                targets.push(primary);
            }
            if prompt.can_recover() {
                targets.push(recover);
            }
            if key.key_code == KeyCode::Tab {
                let current = targets.iter().position(|area| cx.has_key_focus(*area));
                cx.set_key_focus(targets[next_focus(current, targets.len(), key.modifiers.shift)]);
                return true;
            }
            if matches!(key.key_code, KeyCode::ReturnKey | KeyCode::Space) {
                if cx.has_key_focus(cancel) {
                    self.close_adoption_confirmation(cx);
                    return true;
                }
                if prompt.can_confirm() && cx.has_key_focus(primary) {
                    self.adoption_primary(cx);
                    return true;
                }
                if prompt.can_recover() && cx.has_key_focus(recover) {
                    self.adoption_recover(cx);
                    return true;
                }
            }
            false
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn device_setup_preview_has_only_current_identity_and_no_quota() {
            let context = DeviceSetupContext {
                server: "https://matrix.test".into(),
                account: "@owner:test".into(),
                initialization_id: Some("current-setup".into()),
            };
            for language in [crate::i18n::Language::English, crate::i18n::Language::Chinese] {
                let text = context_text_for(&context, language);
                assert!(text.contains("https://matrix.test") && text.contains("@owner:test"));
                assert!(text.contains("current-setup"));
                assert!(!text.contains(crate::i18n::text_for(language, "Shared monthly account limit")));
            }
        }
        #[test]
        fn closing_never_turns_interruption_or_retirement_into_activation() {
            let progress = || Phase::Progress(DeviceSetupProgress {
                state: "active".into(), initialization_id: Some("current".into()), reason: None,
            });
            let read = |reply: String| serde_json::from_str::<Value>(&reply).unwrap();
            assert_eq!(read(close_reply(progress(), true, true).unwrap())["state"], "outcome_unknown");
            assert!(close_reply(progress(), false, false).is_err());
            assert_eq!(read(close_reply(Phase::Failed("interrupted".into()), false, true).unwrap())["state"], "outcome_unknown");
            let done = read(close_reply(progress(), false, true).unwrap());
            assert_eq!(done["state"], "active");
            assert_eq!(done["initializationId"], "current");
            assert!(done.get("migrationId").is_none());
            assert_eq!(read(close_reply(Phase::Loading, false, true).unwrap())["state"], "review_closed");
        }
    }
}

#[cfg(any(feature = "resource-adoption", test))]
fn next_focus(current: Option<usize>, count: usize, reverse: bool) -> usize {
    match current {
        None => {
            if reverse {
                count - 1
            } else {
                0
            }
        }
        Some(index) => {
            if reverse {
                (index + count - 1) % count
            } else {
                (index + 1) % count
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keyboard_reaches_close_and_confirmation_without_auto_confirm() {
        assert_eq!(next_focus(Some(0), 3, false), 1);
        assert_eq!(next_focus(Some(2), 3, false), 0);
        assert_eq!(next_focus(Some(0), 3, true), 2);
        assert_eq!(next_focus(None, 2, false), 0);
    }
    #[test]
    fn native_device_setup_footer_stays_visible_with_long_english_and_chinese_identity() {
        use makepad_widgets::makepad_draw::cx_draw::CxDraw;
        for (width, height, cn) in [
            (360.0, 640.0, false),
            (360.0, 640.0, true),
            (840.0, 700.0, false),
            (840.0, 700.0, true),
        ] {
            let mut cx = Cx::new(Box::new(|_, _| {}));
            let mut panel = cx.with_vm(|vm| {
                makepad_widgets::script_mod(vm);
                crate::i18n::install(vm);
                makepad_code_editor::script_mod(vm);
                crate::shared::script_mod(vm);
                vm.bx.captured_errors = Some(Vec::new());
                crate::miniapps::script_mod(vm);
                let value =
                    vm.eval(script! {use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
                let panel = MiniAppsPanel::script_from_value(vm, value);
                assert!(vm.take_errors().is_empty());
                panel
            });
            for id in [
                ids!(import_form),
                ids!(app_content),
                ids!(notice),
                ids!(approval),
                ids!(grant_confirmation),
            ] {
                panel.view.widget(&cx, id).set_visible(&mut cx, false);
            }
            panel
                .view
                .view(&cx, ids!(adoption_confirmation))
                .set_visible(&mut cx, true);
            let long = if cn {
                "当前账号与服务器\n设备初始化请求\n"
            } else {
                "Current account and server\nDevice initialization request\n"
            }
            .repeat(80);
            panel
                .view
                .label(&cx, ids!(adoption_confirmation.sheet_scroll.details.copy))
                .set_text(&mut cx, &long);
            if cn {
                for (id, text) in [
                    (ids!(adoption_confirmation.buttons.cancel), "关闭核对"),
                    (ids!(adoption_confirmation.buttons.primary), "设置此设备"),
                    (ids!(adoption_confirmation.buttons.recover), "重新检查"),
                ] {
                    panel.view.button(&cx, id).set_text(&mut cx, text);
                }
            }
            let pass = DrawPass::new(&mut cx);
            pass.set_size(&mut cx, dvec2(width, height));
            let mut list = DrawList2d::new(&mut cx);
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(&mut cx, &event);
            let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None);
            list.begin_always(&mut cx2d);
            cx2d.begin_root_turtle(dvec2(width, height), Layout::flow_overlay());
            panel.draw_all(&mut cx2d, &mut Scope::empty());
            cx2d.end_pass_sized_turtle();
            list.end(&mut cx2d);
            cx2d.end_pass(&pass);
            drop(cx2d);
            drop(draw);
            for id in [
                ids!(adoption_confirmation.buttons.cancel),
                ids!(adoption_confirmation.buttons.primary),
                ids!(adoption_confirmation.buttons.recover),
            ] {
                let r = panel.view.widget(&cx, id).area().rect(&cx);
                assert!(
                    r.size.x > 0.0
                        && r.size.y >= 40.0
                        && r.pos.x >= 0.0
                        && r.pos.y >= 0.0
                        && r.pos.x + r.size.x <= width + 0.5
                        && r.pos.y + r.size.y <= height + 0.5,
                    "clipped device setup control {id:?}: {r:?} at {width}x{height}"
                );
            }
        }
    }
}
