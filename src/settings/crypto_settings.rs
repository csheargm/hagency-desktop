//! First-device recovery setup and recovery for another device.
use makepad_widgets::*;
use zeroize::Zeroizing;
use crate::matrix_context::SessionPin;
mod backend;
mod pending;
#[cfg(test)]
pub(crate) mod test_support;
use backend::{Operation, Progress, Snapshot, Update};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    mod.widgets.CryptoSettings = #(CryptoSettings::register_widget(vm)) {
        width: Fill, height: Fit
        flow: Down, spacing: 10
        margin: Inset{top: 16 bottom: 20}
        padding: 14
        show_bg: true
        draw_bg +: {color: mod.widgets.RINX_FIELD border_radius: 4.0}
        View {
            width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 10
            RinxLabel {
                width: Fill height: Fit
                text: #(crate::i18n::tr("Encryption and recovery")) i18n_text: "Encryption and recovery"
                draw_text.text_style: theme.font_bold {font_size: mod.widgets.RINX_TITLE_SIZE}
            }
            refresh := RobrixIconButton {
                height: SETTINGS_BUTTON_HEIGHT
                draw_icon.svg: crate_resource("self://resources/icons/article_refresh.svg")
                icon_walk: Walk{width: 18 height: 18}
            }
        }
        RinxLabel {
            width: Fill height: Fit flow: Flow.Right{wrap: true}
            text: #(crate::i18n::tr("Verification checks device identity. Recovery protects the keys needed to read encrypted history on another device.")) i18n_text: "Verification checks device identity. Recovery protects the keys needed to read encrypted history on another device."
        }
        status := RinxLabel {width: Fill height: Fit flow: Flow.Right{wrap: true}}
        notice := RinxLabel {width: Fill height: Fit flow: Flow.Right{wrap: true} visible: false}
        setup := RinxButton {
            width: Fit height: SETTINGS_BUTTON_HEIGHT visible: false
            text: #(crate::i18n::tr("Set up recovery")) i18n_text: "Set up recovery"
        }
        pending_setup := View {
            width: Fill height: Fit flow: Down spacing: 10 visible: false
            RinxLabel {
                width: Fill height: Fit flow: Flow.Right{wrap: true}
                text: #(crate::i18n::tr("Save the recovery key privately before enabling recovery. Anyone with this key can recover your encrypted messages. Keep a copy outside this device.")) i18n_text: "Save the recovery key privately before enabling recovery. Anyone with this key can recover your encrypted messages. Keep a copy outside this device."
            }
            View {
                width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 10 align: Align{y: 0.5}
                save := RinxButton {width: Fit height: SETTINGS_BUTTON_HEIGHT text: #(crate::i18n::tr("Save recovery key…")) i18n_text: "Save recovery key…"}
                load_pending := RinxButton {width: Fit height: SETTINGS_BUTTON_HEIGHT text: #(crate::i18n::tr("Load saved key to resume…")) i18n_text: "Load saved key to resume…"}
                discard := RinxButton {width: Fit height: SETTINGS_BUTTON_HEIGHT text: #(crate::i18n::tr("Discard unfinished setup")) i18n_text: "Discard unfinished setup"}
                enable := RinxButton {width: Fit height: SETTINGS_BUTTON_HEIGHT text: #(crate::i18n::tr("I saved my key — enable recovery")) i18n_text: "I saved my key — enable recovery"}
            }
        }
        existing_keys := RinxLabel {
            width: Fill height: Fit flow: Flow.Right{wrap: true} visible: false
            text: #(crate::i18n::tr("An existing identity or backup needs its original keys. Verify from an existing device or use a client that holds those keys.")) i18n_text: "An existing identity or backup needs its original keys. Verify from an existing device or use a client that holds those keys."
        }
        restore_section := View {
            width: Fill height: Fit flow: Down spacing: 10 visible: false
            RinxLabel {
                width: Fill height: Fit flow: Flow.Right{wrap: true}
                text: #(crate::i18n::tr("Restore your existing recovery key to recover identity and backup keys. This does not reset your identity or create a replacement backup.")) i18n_text: "Restore your existing recovery key to recover identity and backup keys. This does not reset your identity or create a replacement backup."
            }
            key_input := RobrixTextInput {
                width: Fill height: Fit is_password: true
                autocapitalize: None autocorrect: Disabled content_type: Password
                empty_text: #(crate::i18n::tr("Recovery key or security phrase")) i18n_empty_text: "Recovery key or security phrase"
            }
            View {
                width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 10 align: Align{y: 0.5}
                restore := RinxButton {width: Fit height: SETTINGS_BUTTON_HEIGHT text: #(crate::i18n::tr("Restore with key")) i18n_text: "Restore with key"}
                restore_file := RinxButton {width: Fit height: SETTINGS_BUTTON_HEIGHT text: #(crate::i18n::tr("Restore from key file…")) i18n_text: "Restore from key file…"}
                upload := RinxButton {width: Fit height: SETTINGS_BUTTON_HEIGHT text: #(crate::i18n::tr("Back up pending room keys")) i18n_text: "Back up pending room keys"}
            }
        }
        reset_section := View {
            width: Fill height: Fit flow: Down spacing: 10 visible: false
            reset_lead := RinxLabel {
                width: Fill height: Fit flow: Flow.Right{wrap: true}
                text: #(crate::i18n::tr("If the original keys are lost, you can start over with a new encryption identity.")) i18n_text: "If the original keys are lost, you can start over with a new encryption identity."
            }
            reset_open := RinxButton {
                width: Fit height: SETTINGS_BUTTON_HEIGHT
                text: #(crate::i18n::tr("Reset encryption identity…")) i18n_text: "Reset encryption identity…"
            }
            reset_confirm := RoundedView {
                width: Fill height: Fit flow: Down spacing: 10 visible: false
                padding: Inset{left: 12 right: 12 top: 10 bottom: 12}
                show_bg: true
                draw_bg +: {
                    color: mod.widgets.RINX_DANGER_BG
                    border_color: mod.widgets.RINX_DANGER_FG
                    border_size: 1.0
                    border_radius: 4.0
                }
                RinxLabel {
                    width: Fill height: Fit flow: Flow.Right{wrap: true}
                    draw_text +: {color: mod.widgets.RINX_DANGER_FG}
                    text: #(crate::i18n::tr("A reset gives this account a new encryption identity, held by this device, and deletes the room key backup on the server. Encrypted messages whose keys exist only in that backup, or on devices you no longer have, can never be read again. Your other sessions become unverified, and people who verified you will see that your identity changed. This cannot be undone.")) i18n_text: "A reset gives this account a new encryption identity, held by this device, and deletes the room key backup on the server. Encrypted messages whose keys exist only in that backup, or on devices you no longer have, can never be read again. Your other sessions become unverified, and people who verified you will see that your identity changed. This cannot be undone."
                }
                RinxLabel {
                    width: Fill height: Fit flow: Flow.Right{wrap: true}
                    text: #(crate::i18n::tr("The server may ask for your account password to confirm.")) i18n_text: "The server may ask for your account password to confirm."
                }
                reset_password := RobrixTextInput {
                    width: Fill height: Fit is_password: true
                    autocapitalize: None autocorrect: Disabled content_type: Password
                    empty_text: #(crate::i18n::tr("Account password")) i18n_empty_text: "Account password"
                }
                View {
                    width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: 10 align: Align{y: 0.5}
                    reset_now := RinxButton {
                        width: Fit height: SETTINGS_BUTTON_HEIGHT
                        draw_bg +: {border_color: mod.widgets.RINX_DANGER_FG border_color_hover: mod.widgets.RINX_DANGER_FG border_color_down: mod.widgets.RINX_DANGER_FG}
                        draw_text +: {color: mod.widgets.RINX_DANGER_FG color_hover: mod.widgets.RINX_DANGER_FG color_down: mod.widgets.RINX_DANGER_FG color_focus: mod.widgets.RINX_DANGER_FG}
                        text: #(crate::i18n::tr("Reset now — this cannot be undone")) i18n_text: "Reset now — this cannot be undone"
                    }
                    reset_cancel := RinxButton {width: Fit height: SETTINGS_BUTTON_HEIGHT text: #(crate::i18n::tr("Cancel")) i18n_text: "Cancel"}
                }
            }
        }
    }
}
#[derive(Script, ScriptHook, Widget)]
pub struct CryptoSettings {
    #[deref]
    view: View,
    #[rust]
    pin: Option<SessionPin>,
    #[rust]
    request: u64,
    #[rust]
    busy: bool,
    #[rust]
    state: Option<Snapshot>,
    #[rust]
    notice: String,
    /// The reset warning is open: the reset itself is a second, separate press.
    #[rust]
    confirming_reset: bool,
}
impl Widget for CryptoSettings {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.pin.as_ref().is_some_and(|p| !p.is_current()) {
            self.clear(cx);
        }
        if matches!(event, Event::ScriptReapply) {
            self.sync(cx);
        }
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else {
            return;
        };
        for action in actions {
            if let Some(update) = action.downcast_ref::<Update>() {
                if self.pin.as_ref() != Some(&update.pin)
                    || !update.pin.is_current()
                    || update.request != self.request
                {
                    continue;
                }
                self.busy = false;
                match &update.result {
                    Ok(state) => {
                        self.state = Some(state.clone());
                        self.notice = update
                            .notice
                            .map(crate::i18n::tr)
                            .unwrap_or_default()
                            .to_owned();
                    }
                    Err(error) => {
                        self.state = None;
                        self.notice = format!(
                            "{}: {}",
                            crate::i18n::tr("Could not confirm encryption settings"),
                            crate::i18n::tr(error)
                        );
                    }
                }
                self.sync(cx);
            } else if let Some(progress) = action.downcast_ref::<Progress>() {
                if self.busy
                    && self.pin.as_ref() == Some(&progress.pin)
                    && progress.pin.is_current()
                    && progress.request == self.request
                {
                    self.notice = crate::i18n::tr(progress.notice).to_owned();
                    self.sync(cx);
                }
            }
        }
        if self.busy {
            return;
        }
        let can_reset = self.state.as_ref().is_some_and(Snapshot::can_reset);
        if self.view.button(cx, ids!(reset_open)).clicked(actions) && can_reset {
            self.confirming_reset = true;
            self.sync(cx);
            return;
        }
        if self.view.button(cx, ids!(reset_cancel)).clicked(actions) {
            self.close_reset(cx);
            self.sync(cx);
            return;
        }
        let op = if self.view.button(cx, ids!(refresh)).clicked(actions) {
            Some(Operation::Refresh)
        } else if self.view.button(cx, ids!(setup)).clicked(actions)
            && self.state.as_ref().is_some_and(Snapshot::can_prepare)
        {
            Some(Operation::Prepare)
        } else if self.view.button(cx, ids!(discard)).clicked(actions)
            && self
                .state
                .as_ref()
                .is_some_and(|s| s.pending && !s.applying)
        {
            Some(Operation::Discard)
        } else if self.view.button(cx, ids!(save)).clicked(actions) {
            Some(Operation::Save)
        } else if self.view.button(cx, ids!(load_pending)).clicked(actions) {
            Some(Operation::LoadPending)
        } else if self.view.button(cx, ids!(enable)).clicked(actions)
            && self
                .state
                .as_ref()
                .is_some_and(|s| s.pending && s.key_saved)
        {
            Some(Operation::Enable)
        } else if self.view.button(cx, ids!(restore_file)).clicked(actions) {
            Some(Operation::RestoreFile)
        } else if self.view.button(cx, ids!(restore)).clicked(actions) {
            let input = self.view.text_input(cx, ids!(key_input));
            let key = Zeroizing::new(input.text());
            input.set_text(cx, "");
            Some(Operation::Restore(key))
        } else if self.view.button(cx, ids!(upload)).clicked(actions) {
            Some(Operation::Upload)
        } else if self.view.button(cx, ids!(reset_now)).clicked(actions)
            && self.confirming_reset
            && can_reset
        {
            let input = self.view.text_input(cx, ids!(reset_password));
            let password = Zeroizing::new(input.text());
            self.close_reset(cx);
            Some(Operation::Reset(password))
        } else {
            None
        };
        if let Some(op) = op {
            self.start(cx, op);
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}
impl CryptoSettings {
    fn clear(&mut self, cx: &mut Cx) {
        self.pin = None;
        self.state = None;
        self.busy = false;
        self.notice.clear();
        self.view.text_input(cx, ids!(key_input)).set_text(cx, "");
        self.close_reset(cx);
        self.sync(cx);
    }
    /// Closes the reset warning and forgets a typed password.
    fn close_reset(&mut self, cx: &mut Cx) {
        self.confirming_reset = false;
        self.view
            .text_input(cx, ids!(reset_password))
            .set_text(cx, "");
    }
    fn refresh(&mut self, cx: &mut Cx) {
        let Some(client) = crate::sliding_sync::get_client() else {
            self.clear(cx);
            return;
        };
        let Some(pin) = SessionPin::capture(&client) else {
            return;
        };
        if self.pin.as_ref() != Some(&pin) {
            self.clear(cx);
            self.pin = Some(pin);
        }
        if !self.busy {
            self.start(cx, Operation::Refresh);
        }
    }
    fn start(&mut self, cx: &mut Cx, op: Operation) {
        let Some(pin) = self.pin.clone().filter(SessionPin::is_current) else {
            return;
        };
        self.request = self.request.wrapping_add(1);
        self.busy = true;
        self.notice.clear();
        backend::launch(pin, self.request, op);
        self.sync(cx);
    }
    fn sync(&mut self, cx: &mut Cx) {
        let yes = crate::i18n::tr("Ready");
        let no = crate::i18n::tr("Not set up");
        let status = if self.busy {
            crate::i18n::tr("Checking encryption and recovery…").to_owned()
        } else if let Some(s) = &self.state {
            format!(
                "{}: {} · {}: {} · {}: {}",
                crate::i18n::tr("Device verification"),
                if s.verified {
                    yes
                } else {
                    crate::i18n::tr("Not verified")
                },
                crate::i18n::tr("Key recovery"),
                if s.pending {
                    crate::i18n::tr("Setup not confirmed")
                } else if s.default_key.is_some() {
                    if s.private_keys {
                        crate::i18n::tr("Identity keys available on this device")
                    } else {
                        crate::i18n::tr("Configured; restore this device")
                    }
                } else {
                    no
                },
                crate::i18n::tr("Room key backup"),
                if s.backup_connected {
                    crate::i18n::tr("Connected")
                } else if s.backup {
                    crate::i18n::tr("Restore required")
                } else {
                    no
                }
            )
        } else {
            crate::i18n::tr("Refresh to check your encryption settings.").to_owned()
        };
        self.view.label(cx, ids!(status)).set_text(cx, &status);
        self.view.label(cx, ids!(notice)).set_text(cx, &self.notice);
        self.view
            .widget(cx, ids!(notice))
            .set_visible(cx, !self.notice.is_empty());
        let s = self.state.as_ref();
        self.view.widget(cx, ids!(setup)).set_visible(
            cx,
            backend::file_picker_supported() && s.is_some_and(Snapshot::can_prepare),
        );
        self.view.widget(cx, ids!(existing_keys)).set_visible(
            cx,
            s.is_some_and(|s| !s.pending && s.default_key.is_none() && !s.can_prepare()),
        );
        self.view
            .widget(cx, ids!(pending_setup))
            .set_visible(cx, s.is_some_and(|s| s.pending));
        self.view
            .widget(cx, ids!(save))
            .set_visible(cx, s.is_some_and(|s| s.pending && s.key_loaded));
        self.view
            .widget(cx, ids!(load_pending))
            .set_visible(cx, s.is_some_and(|s| s.pending && !s.key_loaded));
        self.view
            .widget(cx, ids!(discard))
            .set_visible(cx, s.is_some_and(|s| s.pending && !s.applying));
        self.view.button(cx, ids!(enable)).set_enabled(
            cx,
            !self.busy && s.is_some_and(|s| s.pending && s.key_saved),
        );
        self.view
            .widget(cx, ids!(restore_section))
            .set_visible(cx, s.is_some_and(|s| s.default_key.is_some() && !s.pending));
        self.view
            .widget(cx, ids!(restore_file))
            .set_visible(cx, backend::file_picker_supported());
        self.view
            .widget(cx, ids!(upload))
            .set_visible(cx, s.is_some_and(|s| s.backup_connected));
        let can_reset = s.is_some_and(Snapshot::can_reset);
        if !can_reset && self.confirming_reset {
            self.close_reset(cx);
        }
        self.view
            .widget(cx, ids!(reset_section))
            .set_visible(cx, can_reset);
        self.view
            .widget(cx, ids!(reset_lead))
            .set_visible(cx, can_reset && !self.confirming_reset);
        self.view
            .widget(cx, ids!(reset_open))
            .set_visible(cx, can_reset && !self.confirming_reset);
        self.view
            .widget(cx, ids!(reset_confirm))
            .set_visible(cx, can_reset && self.confirming_reset);
        self.view
            .text_input(cx, ids!(reset_password))
            .set_disabled(cx, self.busy);
        for id in [
            ids!(reset_open),
            ids!(reset_now),
            ids!(reset_cancel),
            ids!(refresh),
            ids!(setup),
            ids!(save),
            ids!(discard),
            ids!(load_pending),
            ids!(restore),
            ids!(restore_file),
            ids!(upload),
        ] {
            self.view.button(cx, id).set_enabled(cx, !self.busy);
        }
        self.view
            .text_input(cx, ids!(key_input))
            .set_disabled(cx, self.busy);
        self.view.redraw(cx);
    }
}
impl CryptoSettingsRef {
    pub(crate) fn refresh(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.refresh(cx);
        }
    }
    pub(crate) fn clear(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.clear(cx);
        }
    }
}
pub(crate) fn clear_session() {
    backend::clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_status_distinguishes_account_setup_device_keys_and_backup_connection() {
        const NAME: &str = "settings::crypto_settings::tests::recovery_status_distinguishes_account_setup_device_keys_and_backup_connection";
        if std::env::var("HAGENCY_CRYPTO_UI_TEST").as_deref() != Ok(NAME) {
            let profile = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env("HAGENCY_CRYPTO_UI_TEST", NAME)
                .env("HAGENCY_DESKTOP_DATA_DIR", profile.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm,{mod.widgets.CryptoSettings{}});
            CryptoSettings::script_from_value(vm, value)
        });
        panel.state = Some(Snapshot {
            identity: true,
            verified: true,
            default_key: Some("existing-account-key".into()),
            backup: true,
            ..Default::default()
        });
        panel.sync(&mut cx);
        let status = panel.view.label(&cx, ids!(status)).text();
        assert!(
            status.contains(crate::i18n::tr("Configured; restore this device")),
            "account setup and device verification do not establish local private keys: {status}"
        );
        assert!(status.contains(crate::i18n::tr("Restore required")));
        assert!(!panel.view.button(&cx, ids!(setup)).visible());
        assert!(panel.view.widget(&cx, ids!(restore_section)).visible());

        panel.state.as_mut().unwrap().private_keys = true;
        panel.sync(&mut cx);
        let status = panel.view.label(&cx, ids!(status)).text();
        assert!(status.contains(crate::i18n::tr("Identity keys available on this device")));
        assert!(
            status.contains(crate::i18n::tr("Restore required")),
            "identity keys alone do not establish a backup connection"
        );

        panel.state.as_mut().unwrap().backup_connected = true;
        panel.sync(&mut cx);
        let status = panel.view.label(&cx, ids!(status)).text();
        assert!(status.contains(crate::i18n::tr("Connected")));
        assert!(!status.contains(crate::i18n::tr("Restore required")));
    }
    #[test]
    fn recovery_controls_require_a_saved_key_and_reject_stale_results() {
        const NAME: &str = "settings::crypto_settings::tests::recovery_controls_require_a_saved_key_and_reject_stale_results";
        if std::env::var("HAGENCY_CRYPTO_UI_TEST").as_deref() != Ok(NAME) {
            let profile = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env("HAGENCY_CRYPTO_UI_TEST", NAME)
                .env("HAGENCY_DESKTOP_DATA_DIR", profile.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use crate::settings::crypto_settings::test_support::{Server, install_link_client};
            let server = Server::with_handler(false, std::sync::Arc::new(|_| None));
            let first = server.room().await.client();
            install_link_client(Some(first.clone()));
            let pin = SessionPin::capture(&first).unwrap();
            let mut cx = Cx::new(Box::new(|_, _| {}));
            let mut panel = cx.with_vm(|vm| {
                makepad_widgets::script_mod(vm);
                crate::app::register_widgets(vm);
                let value = script_eval!(vm,{mod.widgets.CryptoSettings{}});
                CryptoSettings::script_from_value(vm, value)
            });
            panel.pin = Some(pin.clone());
            panel.request = 2;
            panel.busy = true;
            let deliver = |panel: &mut CryptoSettings,
                           cx: &mut Cx,
                           pin: SessionPin,
                           request: u64,
                           state: Snapshot| {
                let actions = cx.capture_actions(|cx| {
                    cx.action(Update {
                        pin,
                        request,
                        result: Ok(state),
                        notice: None,
                    })
                });
                panel.handle_event(cx, &Event::Actions(actions), &mut Scope::empty());
            };
            deliver(&mut panel, &mut cx, pin.clone(), 1, Snapshot::default());
            assert!(
                panel.busy,
                "older refresh cannot unlock a mutation in flight"
            );
            deliver(
                &mut panel,
                &mut cx,
                pin.clone(),
                2,
                Snapshot {
                    pending: true,
                    key_loaded: true,
                    ..Default::default()
                },
            );
            assert!(!panel.busy);
            assert!(
                !panel
                    .view
                    .button(&cx, ids!(enable))
                    .borrow()
                    .unwrap()
                    .enabled()
            );
            assert!(panel.view.button(&cx, ids!(discard)).visible());
            panel.state.as_mut().unwrap().applying = true;
            panel.sync(&mut cx);
            assert!(!panel.view.button(&cx, ids!(discard)).visible());
            panel.state.as_mut().unwrap().key_saved = true;
            panel.sync(&mut cx);
            assert!(
                panel
                    .view
                    .button(&cx, ids!(enable))
                    .borrow()
                    .unwrap()
                    .enabled()
            );
            panel
                .view
                .text_input(&cx, ids!(key_input))
                .set_text(&mut cx, "synthetic-private-key");
            let second = server.room().await.client();
            install_link_client(Some(second));
            deliver(
                &mut panel,
                &mut cx,
                pin,
                2,
                Snapshot {
                    default_key: Some("wrong-old-key".into()),
                    ..Default::default()
                },
            );
            assert!(panel.pin.is_none());
            assert!(panel.state.is_none());
            assert!(
                panel
                    .view
                    .text_input(&cx, ids!(key_input))
                    .text()
                    .is_empty()
            );
            install_link_client(None);
        });
    }
    #[test]
    fn reset_is_offered_only_while_this_device_cannot_use_the_existing_keys() {
        // A new account has nothing to reset.
        let mut state = Snapshot::default();
        assert!(!state.can_reset());
        // An identity made elsewhere, whose keys this device does not hold.
        state.identity = true;
        assert!(state.can_reset());
        state.private_keys = true;
        assert!(state.can_reset(), "keys that do not verify this device are not its identity");
        state.verified = true;
        assert!(!state.can_reset(), "a working identity is never offered a reset");
        // A backup this device cannot read or extend.
        state.backup = true;
        assert!(state.can_reset());
        state.backup_connected = true;
        assert!(!state.can_reset());
        // Recovery configured elsewhere and its key not restored here.
        let lost_key = Snapshot {
            identity: true,
            default_key: Some("existing".into()),
            ..Default::default()
        };
        assert!(lost_key.can_reset());
        assert!(!lost_key.can_prepare(), "set-up still never replaces existing keys");
    }
    #[test]
    fn a_reset_needs_the_warning_and_a_second_press_and_cancel_forgets_the_password() {
        const NAME: &str = "settings::crypto_settings::tests::a_reset_needs_the_warning_and_a_second_press_and_cancel_forgets_the_password";
        if std::env::var("HAGENCY_CRYPTO_UI_TEST").as_deref() != Ok(NAME) {
            let profile = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env("HAGENCY_CRYPTO_UI_TEST", NAME)
                .env("HAGENCY_DESKTOP_DATA_DIR", profile.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm,{mod.widgets.CryptoSettings{}});
            CryptoSettings::script_from_value(vm, value)
        });
        let press = |panel: &mut CryptoSettings, cx: &mut Cx, button: &[LiveId; 1]| {
            let uid = panel.view.button(cx, button).widget_uid();
            let actions = cx.capture_actions(|cx| {
                cx.widget_action(uid, ButtonAction::Clicked(KeyModifiers::default()))
            });
            panel.handle_event(cx, &Event::Actions(actions), &mut Scope::empty());
        };
        // Working keys: the reset is not on the screen at all.
        panel.state = Some(Snapshot {
            identity: true,
            verified: true,
            private_keys: true,
            ..Default::default()
        });
        panel.sync(&mut cx);
        assert!(!panel.view.widget(&cx, ids!(reset_section)).visible());
        press(&mut panel, &mut cx, ids!(reset_open));
        assert!(!panel.confirming_reset);

        // An identity made on another device, its keys out of reach.
        panel.state = Some(Snapshot {
            identity: true,
            backup: true,
            ..Default::default()
        });
        panel.sync(&mut cx);
        assert!(panel.view.widget(&cx, ids!(reset_section)).visible());
        assert!(panel.view.widget(&cx, ids!(existing_keys)).visible());
        assert!(panel.view.button(&cx, ids!(reset_open)).visible());
        assert!(!panel.view.widget(&cx, ids!(reset_confirm)).visible());
        // The destructive button does nothing while its warning is closed.
        press(&mut panel, &mut cx, ids!(reset_now));
        assert!(!panel.busy && panel.request == 0);

        press(&mut panel, &mut cx, ids!(reset_open));
        assert!(panel.confirming_reset && !panel.busy && panel.request == 0);
        assert!(panel.view.widget(&cx, ids!(reset_confirm)).visible());
        assert!(!panel.view.button(&cx, ids!(reset_open)).visible());
        panel
            .view
            .text_input(&cx, ids!(reset_password))
            .set_text(&mut cx, "synthetic-account-password");
        press(&mut panel, &mut cx, ids!(reset_cancel));
        assert!(!panel.confirming_reset && !panel.busy && panel.request == 0);
        assert!(!panel.view.widget(&cx, ids!(reset_confirm)).visible());
        assert!(panel.view.text_input(&cx, ids!(reset_password)).text().is_empty());

        // The keys arrive some other way while the warning is open: it closes with them.
        press(&mut panel, &mut cx, ids!(reset_open));
        panel
            .view
            .text_input(&cx, ids!(reset_password))
            .set_text(&mut cx, "synthetic-account-password");
        panel.state = Some(Snapshot {
            identity: true,
            verified: true,
            private_keys: true,
            backup: true,
            backup_connected: true,
            ..Default::default()
        });
        panel.sync(&mut cx);
        assert!(!panel.confirming_reset);
        assert!(!panel.view.widget(&cx, ids!(reset_section)).visible());
        assert!(panel.view.text_input(&cx, ids!(reset_password)).text().is_empty());
    }
    #[test]
    fn existing_identity_or_backup_never_offers_replacement_setup() {
        let mut state = Snapshot::default();
        assert!(state.can_prepare());
        state.identity = true;
        assert!(!state.can_prepare());
        state.private_keys = true;
        assert!(state.can_prepare());
        state.backup = true;
        assert!(!state.can_prepare());
        state.backup_connected = true;
        assert!(state.can_prepare());
        state.default_key = Some("existing".into());
        assert!(!state.can_prepare());
        state.default_key = None;
        state.pending = true;
        assert!(!state.can_prepare());
    }
}
