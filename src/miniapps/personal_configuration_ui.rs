//! Personal model and rules inputs are native widgets, outside the script heap.
use super::*;
use crate::host::hagency::personal_configuration::PersonalConsent;

pub(super) struct PersonalPrompt {
    account: String,
    expires: Instant,
    receiver: Option<Receiver<Result<PersonalConsent, String>>>,
    consent: Option<PersonalConsent>,
    failure: Option<String>,
    tools_text: String,
    selected_directory: usize,
}
impl PersonalPrompt {
    pub(super) fn begin(consent: PersonalConsent) -> Self {
        let account = consent.account().to_owned();
        let (tx, rx) = mpsc::sync_channel(1);
        crate::sliding_sync::spawn_async_task(async move {
            let result = consent.load().await;
            let _ = tx.try_send(result);
            SignalToUI::set_ui_signal();
        });
        Self {
            account,
            expires: Instant::now() + Duration::from_secs(120),
            receiver: Some(rx),
            consent: None,
            failure: None,
            tools_text: String::new(),
            selected_directory: 0,
        }
    }
    pub(super) fn poll(&mut self) -> bool {
        let Some(rx) = &self.receiver else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(v) => v,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(_) => Err("management_unavailable".into()),
        };
        self.receiver = None;
        match result {
            Ok(consent) => {
                self.tools_text = consent.tools().join("\n");
                self.consent = Some(consent);
                self.failure = None;
            }
            Err(code) => {
                self.failure = Some(code);
            }
        }
        true
    }
    pub(super) fn account(&self) -> &str {
        &self.account
    }
    pub(super) fn expired(&self) -> bool {
        Instant::now() >= self.expires
            || self.consent.as_ref().is_some_and(PersonalConsent::expired)
    }
    pub(super) fn ready(&self) -> bool {
        self.receiver.is_none()
            && self.consent.as_ref().is_some_and(PersonalConsent::ready)
            && !self.expired()
    }
    pub(super) fn is_rules(&self) -> bool {
        self.consent.as_ref().is_some_and(PersonalConsent::is_rules)
    }
    pub(super) fn is_model(&self) -> bool {
        self.consent.as_ref().is_some_and(PersonalConsent::is_model)
    }
    pub(super) fn can_pick(&self) -> bool {
        self.receiver.is_none()
            && !self.expired()
            && personal_folder_picker::supported()
            && self
                .consent
                .as_ref()
                .is_some_and(PersonalConsent::can_choose_directory)
    }
    pub(super) fn words(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        self.consent
            .as_ref()
            .map(PersonalConsent::words)
            .unwrap_or((
                "Review personal-agent settings",
                "Reading the current execution-device configuration.",
                "Credentials and directory paths remain native.",
                "Save settings",
            ))
    }
    pub(super) fn details(&self) -> String {
        let mut text = self
            .consent
            .as_ref()
            .map(PersonalConsent::details)
            .unwrap_or_else(|| {
                crate::i18n::tr("Reading current personal-agent configuration…").into()
            });
        if let Some(code) = &self.failure {
            let message = crate::host::hagency::errors::message(&code).unwrap_or_else(|| match code.as_str() {
                "personal_execution_device_required" => {
                    "Open these settings on the agent’s execution device to choose folders or edit custom rules."
                }
                "personal_directory_picker_unavailable" => {
                    "Folder selection is unavailable on this platform. Keep the current workspace or use the default."
                }
                "personal_agent_settings_changed" | "local_revision_conflict" => {
                    "The agent settings changed. Close this review and read the current settings."
                }
                _ => {
                    "The personal-agent configuration could not be verified. Close this review or recover the original saved request."
                }
            });
            text.push_str("\n\n");
            text.push_str(crate::i18n::tr(message));
        }
        text
    }
    pub(super) fn choose_directory(&mut self) {
        if !self.can_pick() {
            return;
        }
        let Some(consent) = self.consent.take() else {
            return;
        };
        let (lease, account) = consent.picker_identity();
        let (tx, rx) = mpsc::sync_channel(1);
        self.receiver = Some(rx);
        self.failure = None;
        crate::sliding_sync::spawn_async_task(async move {
            let result = match personal_folder_picker::choose(lease, account).await {
                Ok(Some(path)) => consent.choose_directory(path).await,
                Ok(None) => Ok(consent),
                Err(error) => Err(error),
            };
            let _ = tx.try_send(result);
            SignalToUI::set_ui_signal();
        });
    }
    pub(super) fn restore_workspace(&mut self) {
        if !self.ready() || !self.is_model() {
            return;
        }
        let Some(consent) = self.consent.take() else {
            return;
        };
        let (tx, rx) = mpsc::sync_channel(1);
        self.receiver = Some(rx);
        self.failure = None;
        crate::sliding_sync::spawn_async_task(async move {
            let result = consent.preserve_workspace().await;
            let _ = tx.try_send(result);
            SignalToUI::set_ui_signal();
        });
    }
    pub(super) async fn accept(self) -> Result<Value, String> {
        if !self.ready() {
            return Err("personal_configuration_review_required".into());
        }
        self.consent
            .ok_or("personal_configuration_review_required")?
            .accept()
            .await
    }
}

impl MiniAppsPanel {
    pub(super) fn present_personal_controls(&mut self, cx: &mut Cx) {
        let prompt = self
            .provider_confirmation
            .as_ref()
            .and_then(|v| match &v.consent {
                resource_enrollment_ui::ProviderReview::Personal(v) => Some(v.as_ref()),
                _ => None,
            });
        self.view
            .widget(cx, ids!(provider_confirmation.sheet_scroll.personal))
            .set_visible(cx, prompt.is_some());
        let Some(prompt) = prompt else {
            return;
        };
        self.view
            .widget(cx, ids!(provider_confirmation.sheet_scroll.personal.rules))
            .set_visible(cx, prompt.is_rules());
        self.view
            .widget(cx, ids!(provider_confirmation.sheet_scroll.personal.workspace))
            .set_visible(cx, prompt.is_model());
        self.view
            .button(
                cx,
                ids!(provider_confirmation.sheet_scroll.personal.choose_directory),
            )
            .set_enabled(cx, prompt.can_pick());
        self.view
            .widget(
                cx,
                ids!(provider_confirmation.sheet_scroll.personal.choose_directory),
            )
            .set_visible(cx, prompt.is_model() || prompt.is_rules());
        let input = self
            .view
            .text_input(cx, ids!(provider_confirmation.sheet_scroll.personal.rules.tools));
        if input.text() != prompt.tools_text {
            input.set_text(cx, &prompt.tools_text);
        }
        let directories = prompt
            .consent
            .as_ref()
            .map(|v| v.directories())
            .unwrap_or(&[]);
        let labels = if directories.is_empty() {
            vec![crate::i18n::tr("No allowed directories").into()]
        } else {
            directories
                .iter()
                .map(|v| v.display().to_string())
                .collect()
        };
        self.view
            .drop_down(
                cx,
                ids!(provider_confirmation.sheet_scroll.personal.rules.directories),
            )
            .set_labels(cx, labels);
        self.view
            .drop_down(
                cx,
                ids!(provider_confirmation.sheet_scroll.personal.rules.directories),
            )
            .set_selected_item(
                cx,
                prompt
                    .selected_directory
                    .min(directories.len().saturating_sub(1)),
            );
        self.view
            .button(
                cx,
                ids!(provider_confirmation.sheet_scroll.personal.rules.remove_directory),
            )
            .set_enabled(
                cx,
                prompt.receiver.is_none() && !prompt.expired() && !directories.is_empty(),
            );
    }
    pub(super) fn handle_personal_controls(&mut self, cx: &mut Cx, actions: &Actions) {
        let Some(pending) = self.provider_confirmation.as_mut() else {
            return;
        };
        let resource_enrollment_ui::ProviderReview::Personal(prompt) = &mut pending.consent else {
            return;
        };
        let mut changed = false;
        if let Some(text) = self
            .view
            .text_input(cx, ids!(provider_confirmation.sheet_scroll.personal.rules.tools))
            .changed(actions)
        {
            if prompt.is_rules() && prompt.receiver.is_none() && !prompt.expired() {
                prompt.tools_text = text;
                if let Some(consent) = &mut prompt.consent {
                    consent.set_tools(&prompt.tools_text);
                }
                changed = true;
            }
        }
        if let Some(index) = self
            .view
            .drop_down(
                cx,
                ids!(provider_confirmation.sheet_scroll.personal.rules.directories),
            )
            .changed(actions)
        {
            prompt.selected_directory = index;
        }
        if self
            .view
            .button(
                cx,
                ids!(provider_confirmation.sheet_scroll.personal.rules.remove_directory),
            )
            .clicked(actions)
        {
            if let Some(consent) = &mut prompt.consent {
                consent.remove_directory(prompt.selected_directory);
                prompt.selected_directory = prompt
                    .selected_directory
                    .min(consent.directories().len().saturating_sub(1));
            }
            changed = true;
        }
        if self
            .view
            .button(
                cx,
                ids!(provider_confirmation.sheet_scroll.personal.choose_directory),
            )
            .clicked(actions)
        {
            prompt.choose_directory();
            changed = true;
        }
        if self
            .view
            .button(
                cx,
                ids!(provider_confirmation.sheet_scroll.personal.workspace.restore),
            )
            .clicked(actions)
        {
            prompt.restore_workspace();
            changed = true;
        }
        if changed {
            self.present_provider_confirmation(cx);
            self.request_confirmation_redraw(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::makepad_draw::draw_list_2d::DrawList2d;

    #[test]
    fn personal_rules_native_sheet_keeps_private_editor_and_footer_reachable() {
        for (width, height, language) in [
            (360., 640., Language::English),
            (360., 640., Language::Chinese),
            (840., 700., Language::English),
            (840., 700., Language::Chinese),
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
                .widget(&cx, ids!(provider_confirmation))
                .set_visible(&mut cx, true);
            panel
                .view
                .widget(&cx, ids!(provider_confirmation.sheet_scroll.personal))
                .set_visible(&mut cx, true);
            panel
                .view
                .widget(&cx, ids!(provider_confirmation.sheet_scroll.personal.workspace))
                .set_visible(&mut cx, false);
            panel
                .view
                .label(&cx, ids!(provider_confirmation.sheet_scroll.heading.title))
                .set_text(
                    &mut cx,
                    crate::i18n::text_for(language, "Edit personal-agent tool rules"),
                );
            panel
                .view
                .label(&cx, ids!(provider_confirmation.sheet_scroll.identity.runtime))
                .set_text(&mut cx, "agent_original\nPolicy revision: 3");
            let tools = panel
                .view
                .text_input(&cx, ids!(provider_confirmation.sheet_scroll.personal.rules.tools));
            tools.set_text(&mut cx, "read_file\nwrite_file");
            assert_eq!(tools.text(), "read_file\nwrite_file");
            panel
                .view
                .drop_down(
                    &cx,
                    ids!(provider_confirmation.sheet_scroll.personal.rules.directories),
                )
                .set_labels(&mut cx, vec!["/native-only/example".into()]);
            panel
                .view
                .button(&cx, ids!(provider_confirmation.buttons.connect))
                .set_text(
                    &mut cx,
                    crate::i18n::text_for(language, "Save custom rules"),
                );
            panel
                .view
                .button(&cx, ids!(provider_confirmation.buttons.cancel))
                .set_text(&mut cx, crate::i18n::text_for(language, "Cancel"));
            let pass = DrawPass::new(&mut cx);
            pass.set_size(&mut cx, dvec2(width, height));
            let mut list = DrawList2d::new(&mut cx);
            panel.request_confirmation_redraw(&mut cx);
            let event = std::mem::take(&mut cx.new_draw_event);
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
                ids!(provider_confirmation.buttons.cancel),
                ids!(provider_confirmation.buttons.connect),
            ] {
                let button = panel.view.button(&cx, path);
                let rect = button.area().rect(&cx);
                assert!(button.visible() && button.area().is_valid(&cx));
                assert!(
                    rect.size.x > 0.
                        && rect.size.y >= 40.
                        && rect.pos.y >= 0.
                        && rect.pos.y + rect.size.y <= height
                        && rect.pos.x >= 0.
                        && rect.pos.x + rect.size.x <= width,
                    "{rect:?}"
                );
            }
        }
    }
}
