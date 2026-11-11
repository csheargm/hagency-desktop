//! Native provider authorization. No URL, credential or path reaches Splash.
use super::*;
use crate::host::hagency::accounts::AccountLoginFlow;
use ::hagency::native_owner::AccountLoginStatus;
use ::hagency::daemon::bridge::AccountLoginAuthorization;
enum Reply {
    Status(Result<AccountLoginStatus, String>),
    Authorization(Result<AccountLoginAuthorization, String>),
}
pub(super) struct AccountLoginPrompt {
    flow: Arc<AccountLoginFlow>,
    reply: (usize, u64),
    state: String,
    result: Option<AccountLoginStatus>,
    receiver: Option<Receiver<Reply>>,
    next_poll: Instant,
    started: bool,
    cancel_requested: bool,
    close_requested: bool,
    failed: bool,
}
fn pending(state: &str) -> bool {
    matches!(state, "starting" | "awaiting_authorization" | "stopping")
}
fn words(state: &str) -> (&'static str, &'static str) {
    match state {
        "review" => (
            "Sign in to this account",
            "Hagency will open the provider’s secure sign-in page. Credentials stay with Codex on this device. Signing in does not allocate resources.",
        ),
        "starting" => (
            "Starting sign-in…",
            "Keep this window open while the provider prepares authorization.",
        ),
        "awaiting_authorization" => (
            "Finish sign-in with the provider",
            "Open the secure sign-in page. This window will update when Codex confirms the result.",
        ),
        "stopping" => (
            "Finishing the original request…",
            "The original login process is being stopped. No second login is started.",
        ),
        "authenticated" => (
            "Account signed in",
            "Codex confirmed this account’s login. Resource enrollment is the next step.",
        ),
        "cancelled" => (
            "Sign-in cancelled",
            "No resource was allocated. Cancellation does not sign out a provider account that was already authenticated.",
        ),
        "expired" => (
            "Sign-in expired",
            "The original login attempt has ended. You can review a new sign-in request.",
        ),
        "superseded" => (
            "A newer request exists",
            "Return to the account and refresh to see its latest status.",
        ),
        _ => (
            "Sign-in result not confirmed",
            "Return to the account to recover the original request. No replacement login was started.",
        ),
    }
}
impl MiniAppsPanel {
    pub(super) fn begin_account_login(
        &mut self,
        cx: &mut Cx,
        args: Value,
        reply: (usize, u64),
    ) -> Result<(), String> {
        if self.account_login.is_some()
            || self.provider_confirmation.is_some()
            || self.reauthentication.is_some()
            || self.execution_confirmation.is_some()
            || self.grant_confirmation.is_some()
            || self.adoption_confirmation.is_some()
            || self.project_room_reply.is_some()
            || !self.pending.is_empty()
        {
            return Err("account_confirmation_busy".into());
        }
        let host = self.hagency.as_ref().ok_or("management_session_required")?;
        let flow = Arc::new(host.account_login_flow(
            self.lease.clone().ok_or("management_session_required")?,
            args,
        )?);
        self.account_login = Some(AccountLoginPrompt {
            flow,
            reply,
            state: "review".into(),
            result: None,
            receiver: None,
            next_poll: Instant::now(),
            started: false,
            cancel_requested: false,
            close_requested: false,
            failed: false,
        });
        self.present_account_login(cx);
        self.request_confirmation_redraw(cx);
        Ok(())
    }
    pub(super) fn present_account_login(&mut self, cx: &mut Cx) {
        self.view
            .view(cx, ids!(account_login_panel))
            .set_visible(cx, self.account_login.is_some());
        let Some(p) = &self.account_login else {
            return;
        };
        self.view.view(cx, ids!(app_content)).set_visible(cx, false);
        self.view.view(cx, ids!(approval)).set_visible(cx, false);
        self.view
            .label(cx, ids!(account_login_panel.sheet_scroll.account))
            .set_text(cx, p.flow.account());
        let (title, body) = words(&p.state);
        self.view
            .label(cx, ids!(account_login_panel.sheet_scroll.status))
            .set_text(cx, crate::i18n::tr(title));
        self.view
            .label(cx, ids!(account_login_panel.sheet_scroll.explanation))
            .set_text(cx, crate::i18n::tr(body));
        self.view
            .label(cx, ids!(account_login_panel.sheet_scroll.namespace))
            .set_text(cx, p.flow.namespace());
        let review = p.state == "review";
        let open = p.state == "awaiting_authorization";
        let primary = self
            .view
            .button(cx, ids!(account_login_panel.buttons.primary));
        primary.set_visible(cx, review || open);
        primary.set_enabled(cx, p.receiver.is_none() && p.flow.active());
        primary.set_text(
            cx,
            crate::i18n::tr(if review {
                "Start sign-in"
            } else {
                "Open sign-in page"
            }),
        );
        self.view
            .button(cx, ids!(account_login_panel.buttons.cancel))
            .set_text(
                cx,
                crate::i18n::tr(if review || pending(&p.state) {
                    "Cancel"
                } else {
                    "Done"
                }),
            );
    }
    fn account_login_work(&mut self, cx: &mut Cx, action: &str) {
        let Some(p) = &mut self.account_login else {
            return;
        };
        if p.receiver.is_some() {
            return;
        }
        let flow = p.flow.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        p.receiver = Some(rx);
        let action = action.to_owned();
        if action == "start" {
            p.started = true;
            p.state = "starting".into();
        }
        if action == "cancel" {
            p.cancel_requested = false;
            p.state = "stopping".into();
        }
        crate::sliding_sync::spawn_async_task(async move {
            let result = match action.as_str() {
                "start" => Reply::Status(flow.start().await),
                "cancel" => Reply::Status(flow.cancel().await),
                "open" => Reply::Authorization(flow.authorization().await),
                _ => Reply::Status(flow.status().await),
            };
            let _ = tx.try_send(result);
            SignalToUI::set_ui_signal();
        });
        self.present_account_login(cx);
        self.request_confirmation_redraw(cx);
    }
    pub(super) fn close_account_login(&mut self, cx: &mut Cx) {
        let Some(p) = &mut self.account_login else {
            return;
        };
        if p.started && (pending(&p.state) || p.receiver.is_some()) {
            p.cancel_requested = true;
            p.close_requested = true;
            self.poll_account_login(cx);
            return;
        }
        self.finish_account_login(cx);
    }
    fn finish_account_login(&mut self, cx: &mut Cx) {
        let Some(p) = self.account_login.take() else {
            return;
        };
        let result = if p.failed {
            Err("management_outcome_unknown".to_owned())
        } else {
            let mut v =
                serde_json::to_value(p.result.unwrap_or_else(|| p.flow.cancelled_before_start()))
                    .unwrap();
            v["recorded"] = json!(p.started);
            Ok(v.to_string())
        };
        splash_host_respond(
            cx,
            p.reply.0,
            p.reply.1,
            result.as_deref().map_err(|e| e.as_str()),
        );
        self.present_account_login(cx);
        self.show_approval(cx);
        self.view.view(cx, ids!(app_content)).set_visible(cx, true);
        self.reply_redraw = cx.new_next_frame();
        self.request_confirmation_redraw(cx);
    }
    pub(super) fn poll_account_login(&mut self, cx: &mut Cx) {
        let Some(p) = &mut self.account_login else {
            return;
        };
        let event = p.receiver.as_ref().and_then(|r| match r.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Reply::Status(Err("management_outcome_unknown".into())))
            }
            Err(mpsc::TryRecvError::Empty) => None,
        });
        let changed = event.is_some();
        if let Some(event) = event {
            p.receiver = None;
            p.next_poll = Instant::now() + Duration::from_millis(600);
            match event {
                Reply::Status(Ok(status)) => {
                    p.state = status.state.clone();
                    p.result = Some(status);
                }
                Reply::Authorization(Ok(url)) => {
                    if p.flow.active() && !p.cancel_requested {
                        let _ = robius_open::Uri::new(&url.into_url()).open();
                    }
                }
                _ => {
                    p.failed = true;
                    p.state = "uncertain".into();
                }
            }
        }
        let finish = p.close_requested && p.receiver.is_none() && !pending(&p.state);
        let cancel = p.cancel_requested && p.receiver.is_none() && pending(&p.state);
        let poll = p.receiver.is_none() && pending(&p.state) && Instant::now() >= p.next_poll;
        if finish {
            self.finish_account_login(cx);
        } else if cancel {
            self.account_login_work(cx, "cancel");
        } else if poll {
            self.account_login_work(cx, "status");
        } else if changed {
            self.present_account_login(cx);
            self.request_confirmation_redraw(cx);
        }
    }
    pub(super) fn handle_account_login_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let Some(p) = &self.account_login else {
            return;
        };
        let state = p.state.clone();
        if self
            .view
            .button(cx, ids!(account_login_panel.buttons.primary))
            .clicked(actions)
        {
            self.account_login_work(cx, if state == "review" { "start" } else { "open" });
        } else if self
            .view
            .button(cx, ids!(account_login_panel.buttons.cancel))
            .clicked(actions)
        {
            self.close_account_login(cx);
        }
    }
    pub(super) fn account_login_key(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if self.account_login.is_none() {
            return false;
        }
        let Event::KeyDown(key) = event else {
            return false;
        };
        if key.key_code == KeyCode::Escape {
            self.close_account_login(cx);
            return true;
        }
        if key.key_code == KeyCode::Tab {
            let cancel = self
                .view
                .button(cx, ids!(account_login_panel.buttons.cancel))
                .area();
            let primary = self
                .view
                .button(cx, ids!(account_login_panel.buttons.primary));
            cx.set_key_focus(if cx.has_key_focus(cancel) && primary.visible() {
                primary.area()
            } else {
                cancel
            });
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;
    #[test]
    fn native_hagency_account_login_controls_stay_reachable_through_review_progress_result() {
        for (width, height, language) in [
            (360., 640., crate::i18n::Language::English),
            (360., 640., crate::i18n::Language::Chinese),
            (840., 700., crate::i18n::Language::English),
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
            for path in [
                ids!(catalog),
                ids!(import_form),
                ids!(hagency_review),
                ids!(app_content),
                ids!(notice),
                ids!(approval),
            ] {
                panel.view.widget(&cx, path).set_visible(&mut cx, false);
            }
            panel
                .view
                .widget(&cx, ids!(account_login_panel))
                .set_visible(&mut cx, true);
            panel
                .view
                .label(&cx, ids!(account_login_panel.sheet_scroll.account))
                .set_text(&mut cx, "@original-owner:matrix.example");
            panel
                .view
                .label(&cx, ids!(account_login_panel.sheet_scroll.namespace))
                .set_text(&mut cx, "account_0123456789abcdef0123456789abcdef");
            let pass = DrawPass::new(&mut cx);
            pass.set_size(&mut cx, dvec2(width, height));
            let mut list = DrawList2d::new(&mut cx);
            for state in [
                "review",
                "starting",
                "awaiting_authorization",
                "stopping",
                "authenticated",
                "uncertain",
            ] {
                let (title, body) = words(state);
                panel
                    .view
                    .label(&cx, ids!(account_login_panel.sheet_scroll.status))
                    .set_text(&mut cx, crate::i18n::text_for(language, title));
                panel
                    .view
                    .label(&cx, ids!(account_login_panel.sheet_scroll.explanation))
                    .set_text(&mut cx, crate::i18n::text_for(language, body));
                let primary = state == "review" || state == "awaiting_authorization";
                panel
                    .view
                    .button(&cx, ids!(account_login_panel.buttons.primary))
                    .set_visible(&mut cx, primary);
                panel
                    .view
                    .button(&cx, ids!(account_login_panel.buttons.primary))
                    .set_text(
                        &mut cx,
                        crate::i18n::text_for(
                            language,
                            if state == "review" {
                                "Start sign-in"
                            } else {
                                "Open sign-in page"
                            },
                        ),
                    );
                panel.request_confirmation_redraw(&mut cx);
                let event = std::mem::take(&mut cx.new_draw_event);
                assert!(event.will_redraw());
                let mut draw = CxDraw::new(&mut cx, &event);
                let mut c = Cx2d::new(&mut draw);
                c.begin_pass(&pass, None);
                if list.begin(&mut c, Walk::default()).is_redrawing() {
                    c.begin_root_turtle(dvec2(width, height), Layout::flow_overlay());
                    panel.draw_all(&mut c, &mut Scope::empty());
                    c.end_pass_sized_turtle();
                    list.end(&mut c);
                }
                c.end_pass(&pass);
                drop(c);
                drop(draw);
                for path in [
                    ids!(account_login_panel.buttons.cancel),
                    ids!(account_login_panel.buttons.primary),
                ] {
                    let button = panel.view.button(&cx, path);
                    if !button.visible() {
                        continue;
                    }
                    let rect = button.area().rect(&cx);
                    assert!(rect.size.x > 0. && rect.size.y > 0.);
                    assert!(
                        rect.pos.y >= 0. && rect.pos.y + rect.size.y <= height,
                        "{state}: {rect:?}"
                    );
                    assert!(
                        rect.pos.x >= 0. && rect.pos.x + rect.size.x <= width,
                        "{state}: {rect:?}"
                    );
                }
            }
        }
    }
}
