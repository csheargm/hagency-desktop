//! Current-account confirmation is native. A mini app cannot choose an identity
//! or trigger account/logout changes by supplying an approval argument.
use super::*;
use crate::host::hagency::reauthentication::{ReauthenticationConsent, SERVICE};
pub(super) struct ReauthenticationPrompt {
    consent: ReauthenticationConsent,
    reply: (usize, u64),
}
impl MiniAppsPanel {
    pub(super) fn begin_reauthentication(
        &mut self,
        cx: &mut Cx,
        args: Value,
        reply: (usize, u64),
    ) -> Result<(), String> {
        if self.execution_confirmation.is_some() || self.reauthentication.is_some()
            || self.provider_confirmation.is_some() || self.account_login.is_some()
            || self.grant_confirmation.is_some()
            || self.adoption_confirmation.is_some()
            || self.project_room_reply.is_some()
            || !self.pending.is_empty()
        {
            return Err("account_confirmation_busy".into());
        }
        let host = self.hagency.as_ref().ok_or("management_session_required")?;
        let lease = self.lease.clone().ok_or("management_session_required")?;
        let consent = host.reauthentication_consent(lease, args)?;
        self.reauthentication = Some(ReauthenticationPrompt { consent, reply });
        self.present_reauthentication(cx);
        self.request_confirmation_redraw(cx);
        Ok(())
    }
    pub(super) fn present_reauthentication(&mut self, cx: &mut Cx) {
        let visible = self.reauthentication.is_some();
        self.view
            .view(cx, ids!(account_confirmation))
            .set_visible(cx, visible);
        if let Some(prompt) = &self.reauthentication {
            self.view
                .label(cx, ids!(account_confirmation.sheet_scroll.account))
                .set_text(cx, prompt.consent.account());
            self.view.view(cx, ids!(app_content)).set_visible(cx, false);
            self.view.view(cx, ids!(approval)).set_visible(cx, false);
        }
    }
    pub(super) fn finish_reauthentication(&mut self, cx: &mut Cx, approved: bool) {
        let Some(prompt) = self.reauthentication.take() else {
            return;
        };
        self.present_reauthentication(cx);
        self.show_approval(cx);
        self.view.view(cx, ids!(app_content)).set_visible(cx, true);
        let outcome = decide(approved, || prompt.consent.accept());
        match outcome {
            Ok(action) => {
                let response = if action.is_some() {
                    r#"{"state":"signing_in"}"#
                } else {
                    r#"{"state":"cancelled"}"#
                };
                splash_host_respond(cx, prompt.reply.0, prompt.reply.1, Ok(response));
                if let Some(action) = action {
                    cx.action(action);
                }
            }
            Err(error) => {
                splash_host_respond(cx, prompt.reply.0, prompt.reply.1, Err(&error));
            }
        }
        self.reply_redraw = cx.new_next_frame();
        self.request_confirmation_redraw(cx);
    }
    pub(super) fn reauthentication_key(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if self.reauthentication.is_none() {
            return false;
        }
        let Event::KeyDown(key) = event else {
            return false;
        };
        if key.key_code == KeyCode::Escape {
            self.finish_reauthentication(cx, false);
            return true;
        }
        if key.key_code == KeyCode::Tab {
            let cancel = self
                .view
                .button(cx, ids!(account_confirmation.buttons.cancel))
                .area();
            let confirm = self
                .view
                .button(cx, ids!(account_confirmation.buttons.confirm))
                .area();
            cx.set_key_focus(if cx.has_key_focus(cancel) {
                confirm
            } else {
                cancel
            });
            return true;
        }
        false
    }
}
fn decide(
    approved: bool,
    confirm: impl FnOnce() -> Result<crate::accounts::AccountAction, String>,
) -> Result<Option<crate::accounts::AccountAction>, String> {
    if approved {
        confirm().map(Some)
    } else {
        Ok(None)
    }
}
pub(super) fn is_service(service: &str) -> bool {
    service == SERVICE
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;
    #[test]
    fn native_reauthentication_cancel_never_invokes_account_transition() {
        assert!(
            decide(false, || panic!("cancel must not invoke sign-in"))
                .unwrap()
                .is_none()
        );
        let action = decide(true, || Ok(crate::accounts::AccountAction::Reauthenticate)).unwrap();
        assert!(matches!(
            action,
            Some(crate::accounts::AccountAction::Reauthenticate)
        ));
        assert!(decide(true, || Err("management_scope_changed".into())).is_err());
    }
    #[test]
    fn native_reauthentication_controls_have_reachable_bounds_after_hidden_transition() {
        for (width, height, language) in [
            (360.0, 640.0, crate::i18n::Language::English),
            (360.0, 640.0, crate::i18n::Language::Chinese),
            (840.0, 700.0, crate::i18n::Language::English),
        ] {
            let mut cx = Cx::new(Box::new(|_, _| {}));
            let mut panel = cx.with_vm(|vm| {
                makepad_widgets::script_mod(vm);
                crate::i18n::install(vm);
                makepad_code_editor::script_mod(vm);
                crate::shared::script_mod(vm);
                vm.bx.captured_errors = Some(Vec::new());
                crate::miniapps::script_mod(vm);
                let v =
                    vm.eval(script! {use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
                let panel = MiniAppsPanel::script_from_value(vm, v);
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
                .label(&cx, ids!(account_confirmation.sheet_scroll.account))
                .set_text(&mut cx, "@original-owner:matrix.example");
            panel
                .view
                .button(&cx, ids!(account_confirmation.buttons.confirm))
                .set_text(&mut cx, crate::i18n::text_for(language, "Sign in again"));
            let pass = DrawPass::new(&mut cx);
            pass.set_size(&mut cx, dvec2(width, height));
            let mut list = DrawList2d::new(&mut cx);
            for visible in [false, true, false, true] {
                panel
                    .view
                    .widget(&cx, ids!(account_confirmation))
                    .set_visible(&mut cx, visible);
                panel.request_confirmation_redraw(&mut cx);
                let event = std::mem::take(&mut cx.new_draw_event);
                assert!(event.will_redraw());
                let mut draw = CxDraw::new(&mut cx, &event);
                let mut cx2d = Cx2d::new(&mut draw);
                cx2d.begin_pass(&pass, None);
                if list.begin(&mut cx2d, Walk::default()).is_redrawing() {
                    cx2d.begin_root_turtle(dvec2(width, height), Layout::flow_overlay());
                    panel.draw_all(&mut cx2d, &mut Scope::empty());
                    cx2d.end_pass_sized_turtle();
                    list.end(&mut cx2d);
                }
                cx2d.end_pass(&pass);
                drop(cx2d);
                drop(draw);
                if visible {
                    for path in [
                        ids!(account_confirmation.buttons.cancel),
                        ids!(account_confirmation.buttons.confirm),
                    ] {
                        let rect = panel.view.button(&cx, path).area().rect(&cx);
                        assert!(rect.size.x > 0.0 && rect.size.y > 0.0);
                        assert!(rect.pos.y >= 0.0 && rect.pos.y + rect.size.y <= height);
                        assert!(rect.pos.x >= 0.0 && rect.pos.x + rect.size.x <= width);
                    }
                }
            }
        }
    }
}
