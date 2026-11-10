use std::borrow::Cow;

use makepad_widgets::*;
use matrix_sdk::{encryption::verification::Verification, ruma::events::key::verification::cancel::CancelCode};

use crate::verification::{VerificationAction, VerificationRequestActionState, VerificationUserResponse, ScopedVerificationAction, VerificationScope};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*


    mod.widgets.VerificationEmojiCell = View {
        width: Fit, height: Fit
        flow: Down
        align: Align{x: 0.5}
        padding: Inset{top: 6, right: 6, bottom: 6, left: 6}
        spacing: 4

        symbol := RinxLabel {
            width: 72, height: Fit
            align: Align{x: 0.5}
            draw_text +: {
                text_style: REGULAR_TEXT {font_size: (30 * mod.widgets.RINX_TEXT_SCALE)}, // style-size: glyph
                color: mod.widgets.RINX_INK
            }
        }
        description := RinxLabel {
            width: 72, height: Fit
            flow: Flow.Right{wrap: true}
            align: Align{x: 0.5}
            draw_text +: {
                text_style: REGULAR_TEXT {font_size: mod.widgets.RINX_META_SIZE},
                color: mod.widgets.RINX_INK
            }
        }
    }

    mod.widgets.VerificationModal = set_type_default() do #(VerificationModal::register_widget(vm)) {
        ..mod.widgets.SmallModal

        title := ModalTitle {
            text: #(crate::i18n::tr("Verification Request")) i18n_text: "Verification Request"
        }

        body := ModalBody {}

        // SAS V1 always produces exactly 7 emojis.
        emojis_view := View {
            width: Fill, height: Fit
            flow: Flow.Right{wrap: true}
            align: Align{x: 0.5}
            spacing: 10
            margin: Inset{top: 15, bottom: 5}
            visible: false

            emoji0 := mod.widgets.VerificationEmojiCell {}
            emoji1 := mod.widgets.VerificationEmojiCell {}
            emoji2 := mod.widgets.VerificationEmojiCell {}
            emoji3 := mod.widgets.VerificationEmojiCell {}
            emoji4 := mod.widgets.VerificationEmojiCell {}
            emoji5 := mod.widgets.VerificationEmojiCell {}
            emoji6 := mod.widgets.VerificationEmojiCell {}
        }

        buttons_view := ModalButtonsRow {
            margin: Inset{top: 30}

            cancel_button := RobrixNegativeIconButton {
                align: Align{x: 0.5, y: 0.5}
                padding: Inset{left: 15 right: 15 top: 0 bottom: 0},
                draw_icon.svg: (ICON_FORBIDDEN)
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -1} }
                text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"
            }

            accept_button := RobrixPositiveIconButton {
                align: Align{x: 0.5, y: 0.5}
                padding: Inset{left: 15 right: 15 top: 0 bottom: 0},
                draw_icon.svg: (ICON_CHECKMARK)
                icon_walk: Walk{width: 16, height: 16, margin: Inset{left: -2, right: -1} }
                text: #(crate::i18n::tr("Yes")) i18n_text: "Yes"
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct VerificationModal {
    #[deref] view: View,
    #[rust] state: Option<VerificationRequestActionState>,
    #[rust] active_scope: Option<VerificationScope>,
    /// Whether the modal is in a "final" state,
    /// meaning that the verification process has ended
    /// and that any further interaction with it should close the modal.
    #[rust(false)] is_final: bool,
    /// Whether the emoji/decimal comparison is currently being shown,
    /// which deetermines what the cancel button text shows.
    #[rust(false)] is_comparing_keys: bool,
}

/// Actions emitted by the `VerificationModal`.
#[derive(Clone, Copy, Debug)]
pub enum VerificationModalAction {
    Close,
}

impl Widget for VerificationModal {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.active_scope.as_ref().is_some_and(|scope| !scope.is_current()) {
            self.reset_state();
            cx.action(VerificationModalAction::Close);
            return;
        }
        self.view.handle_event(cx, event, scope);
        self.widget_match_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl WidgetMatchEvent for VerificationModal {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        let accept_button = self.button(cx, ids!(accept_button));
        let cancel_button = self.button(cx, ids!(cancel_button));

        let cancel_button_clicked = cancel_button.clicked(actions);
        let modal_dismissed = actions
            .iter()
            .any(|a| matches!(a.downcast_ref(), Some(ModalAction::Dismissed)));

        if cancel_button_clicked || modal_dismissed {
            if let Some(state) = self.state.as_ref() {
                // Clicking "no" should send a key mismatch response,
                // but dismissing the modal should just cancel the verification.
                let response = if self.is_comparing_keys && cancel_button_clicked {
                    VerificationUserResponse::Mismatch
                } else {
                    VerificationUserResponse::Cancel
                };
                let _ = state.response_sender.send(response);
            }
            self.reset_state();

            // If the modal was dismissed by clicking outside of it, we MUST NOT emit
            // a `VerificationModalAction::Close` action, as that would cause
            // an infinite action feedback loop.
            if !modal_dismissed {
                cx.action(VerificationModalAction::Close);
            }
        }

        if accept_button.clicked(actions) {
            if self.is_final {
                cx.action(VerificationModalAction::Close);
                self.reset_state();
            } else if let Some(state) = self.state.as_ref() {
                let _ = state.response_sender.send(VerificationUserResponse::Accept);
                self.is_comparing_keys = false;
                accept_button.set_enabled(cx, false);
            }
        }

        let mut needs_redraw = false;
        for action in actions {
            // `VerificationAction`s come from a background thread, so they are NOT widget actions.
            if let Some(scoped) = action.downcast_ref::<ScopedVerificationAction>() {
                if !scoped.scope.is_current() || self.active_scope.as_ref()!=Some(&scoped.scope) {continue;}
                let verification_action = &scoped.action;
                // `RequestReceived` is handled by the top-level app.
                if matches!(verification_action, VerificationAction::RequestReceived(_)) {
                    continue;
                }
                // The emoji grid is hidden by default and only shown during emoji verification.
                self.view.view(cx, ids!(emojis_view)).set_visible(cx, false);
                match verification_action {
                    VerificationAction::RequestCancelled(cancel_info) => {
                        let text: Cow<'static, str> = match cancel_info.cancel_code() {
                            CancelCode::Timeout => Cow::from(
                                "The verification process timed out.\n\n\
                                Each step must be completed fairly quickly. Try again."
                            ),
                            CancelCode::Accepted => Cow::from(
                                "You handled this request on a different device."
                            ),
                            CancelCode::MismatchedSas => Cow::from(
                                "The emoji did not match, so nothing was verified.\n\n\
                                If you're sure you compared them correctly, your connection may be compromised."
                            ),
                            CancelCode::User if cancel_info.cancelled_by_us() => Cow::from(
                                "Verification was cancelled on this device.\n\n\
                                You might have started another verification request jointly, \
                                or there was a different Matrix error. Please try again."
                            ),
                            CancelCode::User => Cow::from(
                                "Verification was cancelled on the other device."
                            ),
                            _ => Cow::from(format!(
                                "Verification was cancelled by {}: {}",
                                if cancel_info.cancelled_by_us() { "this device" } else { "another device" },
                                cancel_info.reason(),
                            )),
                        };
                        self.label(cx, ids!(body)).set_text(cx, &text);
                        self.transition_to_final_state(cx, &accept_button, &cancel_button);
                    }

                    VerificationAction::RequestAccepted => {
                        self.label(cx, ids!(body)).set_text(
                            cx,
                            "You successfully accepted the verification request.\n\n\
                            Waiting for the other device to agree on verification methods…"
                        );
                        self.transition_to_waiting_state(cx, &accept_button, &cancel_button);
                    }

                    VerificationAction::RequestAcceptError(error) => {
                        self.label(cx, ids!(body)).set_text(cx,
                            &format!(
                                "Error accepting verification request: {}\n\n\
                                Please try the verification process again.",
                                error,
                            ),
                        );
                        self.transition_to_final_state(cx, &accept_button, &cancel_button);
                    }

                    VerificationAction::RequestCancelError(error) => {
                        self.label(cx, ids!(body)).set_text(
                            cx,
                            &format!("Error cancelling verification request: {}.", error)
                        );
                        self.transition_to_final_state(cx, &accept_button, &cancel_button);
                    }

                    VerificationAction::RequestTransitionedToUnsupportedMethod(method) => {
                        self.label(cx, ids!(body)).set_text(
                            cx,
                            &format!(
                                "Verification request transitioned to unsupported method: {}\n\nPlease try the verification process again.",
                                match method {
                                    Verification::SasV1(_) => "Short Authentication String",
                                    // Verification::QrV1(_) => "QR Code",
                                    _other => "Unknown",
                                },
                            )
                        );
                        self.transition_to_final_state(cx, &accept_button, &cancel_button);
                    }

                    VerificationAction::SasAccepted(_accepted_protocols) => {
                        self.label(cx, ids!(body)).set_text(
                            cx,
                            "Both sides have accepted the same verification method(s).\n\n\
                            Waiting for both devices to exchange keys…"
                        );
                        self.transition_to_waiting_state(cx, &accept_button, &cancel_button);
                    }

                    VerificationAction::KeysExchanged { emojis, decimals } => {
                        if let Some(emoji_list) = emojis {
                            self.label(cx, ids!(body)).set_text(
                                cx,
                                "Keys have been exchanged. Please verify the following emoji:\n\n\
                                Do the emoji names match?",
                            );
                            let emoji_cell_paths: &[&[LiveId]] = ids_array!(
                                emoji0,
                                emoji1,
                                emoji2,
                                emoji3,
                                emoji4,
                                emoji5,
                                emoji6,
                            );
                            for (emoji, cell) in emoji_list.emojis.iter().zip(
                                self.view_set(cx, emoji_cell_paths).iter()
                            ) {
                                cell.label(cx, ids!(symbol)).set_text(cx, emoji.symbol);
                                cell.label(cx, ids!(description)).set_text(cx, emoji.description);
                            }
                            self.view.view(cx, ids!(emojis_view)).set_visible(cx, true);
                        } else {
                            let text = format!(
                                "Keys have been exchanged. Please verify the following numbers:\n\
                                \n   {}\n   {}\n   {}\n\n\
                                Do these number keys match?",
                                decimals.0, decimals.1, decimals.2,
                            );
                            self.label(cx, ids!(body)).set_text(cx, &text);
                        }
                        self.is_final = false;
                        self.is_comparing_keys = true;
                        accept_button.set_visible(cx, true);
                        accept_button.set_enabled(cx, true);
                        accept_button.set_text(cx, crate::i18n::tr("Yes"));
                        cancel_button.set_text(cx, "No");
                        cancel_button.set_enabled(cx, true);
                        cancel_button.set_visible(cx, true);
                    }

                    VerificationAction::SasConfirmed => {
                        self.label(cx, ids!(body)).set_text(
                            cx,
                            "You successfully confirmed the Short Auth String keys.\n\n\
                            Waiting for the other device to confirm…"
                        );
                        self.transition_to_waiting_state(cx, &accept_button, &cancel_button);
                    }

                    VerificationAction::SasConfirmationError(error) => {
                        self.label(cx, ids!(body)).set_text(
                            cx,
                            &format!("Error confirming keys: {}\n\nPlease retry the verification process.", error)
                        );
                        self.transition_to_final_state(cx, &accept_button, &cancel_button);
                    }

                    VerificationAction::RequestCompleted => {
                        self.label(cx, ids!(body)).set_text(
                            cx,
                            crate::i18n::tr("Verification completed. The compared keys matched. Encrypted history still requires its room keys."),
                        );
                        self.transition_to_final_state(cx, &accept_button, &cancel_button);
                    }

                    // Skipped above. Listed explicitly instead of a `_` catch-all so that a
                    // new action variant is a compile error here rather than a silent no-op.
                    VerificationAction::RequestReceived(_) => { }
                }
                // If we received a `VerificationAction`, we need to redraw the modal content.
                needs_redraw = true;
            }
        }

        if needs_redraw {
            self.redraw(cx);
        }
    }
}

impl VerificationModal {
    fn reset_state(&mut self) {
        self.state = None;
        self.active_scope = None;
        self.is_final = false;
        self.is_comparing_keys = false;
    }

    /// The verification flow is waiting on another device.
    fn transition_to_waiting_state(&mut self, cx: &mut Cx, accept_button: &ButtonRef, cancel_button: &ButtonRef) {
        self.is_final = false;
        self.is_comparing_keys = false;
        accept_button.set_enabled(cx, false);
        accept_button.set_text(cx, "Waiting…");
        cancel_button.set_text(cx, crate::i18n::tr("Cancel"));
        cancel_button.set_enabled(cx, true);
        cancel_button.set_visible(cx, true);
    }

    /// The verification flow is in its final state.
    fn transition_to_final_state(&mut self, cx: &mut Cx, accept_button: &ButtonRef, cancel_button: &ButtonRef) {
        accept_button.set_visible(cx, true);
        accept_button.set_enabled(cx, true);
        accept_button.set_text(cx, crate::i18n::tr("Okay"));
        cancel_button.set_visible(cx, false);
        self.is_final = true;
        self.is_comparing_keys = false;
    }

    fn initialize_with_data(
        &mut self,
        cx: &mut Cx,
        state: VerificationRequestActionState,
    ) {
        log!("Initializing verification modal with state: {:?}", state);
        let request = &state.request;
        // `we_started` means this is an outgoing request we just sent, so we don't need
        // to accept it, but rather just wait for another device to accept & respond.
        let we_started = request.we_started();
        let prompt_text = if we_started {
            Cow::from("We just sent a verification request to your other logged-in devices.\n\n\
                Accept the request on another device to continue…")
        } else if request.is_self_verification() {
            Cow::from("Do you wish to verify your own device?")
        } else if let Some(room_id) = request.room_id() {
            format!("Do you wish to verify user {} in room {}?",
                request.other_user_id(),
                room_id,
            ).into()
        } else {
            format!("Do you wish to verify user {}?",
                request.other_user_id()
            ).into()
        };
        self.label(cx, ids!(body)).set_text(cx, &prompt_text);
        self.view.view(cx, ids!(emojis_view)).set_visible(cx, false);

        let accept_button = self.button(cx, ids!(accept_button));
        let cancel_button = self.button(cx, ids!(cancel_button));
        accept_button.set_text(cx, crate::i18n::tr("Yes"));
        accept_button.set_enabled(cx, !we_started);
        accept_button.set_visible(cx, !we_started);
        cancel_button.set_text(cx, crate::i18n::tr("Cancel"));
        cancel_button.set_enabled(cx, true);
        cancel_button.set_visible(cx, true);

        self.active_scope=Some(state.scope.clone());
        self.state = Some(state);
        self.is_final = false;
        self.is_comparing_keys = false;
    }
}

impl VerificationModalRef {
    pub(crate) fn clear(&self,cx:&mut Cx) {
        if let Some(mut inner)=self.borrow_mut() {inner.reset_state();inner.view.redraw(cx);}
    }
    pub fn initialize_with_data(
        &self,
        cx: &mut Cx, 
        state: VerificationRequestActionState,
    ) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.initialize_with_data(cx, state);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::crypto_settings::test_support::{Server, install_link_client};

    #[test]
    fn queued_verification_results_require_the_exact_session_peer_and_flow() {
        const NAME: &str = "verification_modal::tests::queued_verification_results_require_the_exact_session_peer_and_flow";
        if std::env::var("HAGENCY_VERIFY_SCOPE_TEST").as_deref() != Ok(NAME) {
            let profile = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env("HAGENCY_VERIFY_SCOPE_TEST", NAME)
                .env("HAGENCY_DESKTOP_DATA_DIR", profile.path()).output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let server = Server::with_handler(false, std::sync::Arc::new(|_| None));
            let first = server.room().await.client();
            install_link_client(Some(first.clone()));
            let scope = VerificationScope {
                session: crate::matrix_context::SessionPin::capture(&first).unwrap(),
                peer: matrix_sdk::ruma::user_id!("@alice:example.org").to_owned(),
                flow_id: "second-flow".into(),
            };
            let mut cx = Cx::new(Box::new(|_, _| {}));
            let mut modal = cx.with_vm(|vm| {
                makepad_widgets::script_mod(vm);
                crate::app::register_widgets(vm);
                let value = script_eval!(vm, { mod.widgets.VerificationModal {} });
                VerificationModal::script_from_value(vm, value)
            });
            modal.active_scope = Some(scope.clone());
            let send = |modal: &mut VerificationModal, cx: &mut Cx, incoming: VerificationScope| {
                let actions = cx.capture_actions(|cx| cx.action(ScopedVerificationAction {
                    scope: incoming, action: VerificationAction::RequestCompleted,
                }));
                modal.handle_event(cx, &Event::Actions(actions), &mut Scope::empty());
            };
            let mut old_flow = scope.clone(); old_flow.flow_id = "first-flow".into();
            send(&mut modal, &mut cx, old_flow);
            assert!(!modal.is_final, "old transaction cannot finish a new modal");
            let mut wrong_peer = scope.clone(); wrong_peer.peer = matrix_sdk::ruma::user_id!("@bob:example.org").to_owned();
            send(&mut modal, &mut cx, wrong_peer);
            assert!(!modal.is_final, "peer is part of the transaction identity");
            send(&mut modal, &mut cx, scope.clone());
            assert!(modal.is_final, "current exact completion is rendered");
            modal.is_final = false;
            let second = server.room().await.client();
            assert_eq!(first.user_id(), second.user_id());
            assert_eq!(first.device_id(), second.device_id());
            install_link_client(Some(second.clone()));
            send(&mut modal, &mut cx, scope.clone());
            assert!(!modal.is_final, "same IDs after re-login do not authorize old SDK results");
            let current = VerificationScope {session: crate::matrix_context::SessionPin::capture(&second).unwrap(), ..scope};
            modal.active_scope = Some(current.clone());
            send(&mut modal, &mut cx, current);
            assert!(modal.is_final);
            modal.reset_state();
            assert!(modal.active_scope.is_none());
            install_link_client(None);
        });
    }
}
