//! A failed built-in script must return to native review, not a blank app.
//! The script budget remains the framework's. Retrying is a new explicit Run;
//! neither this guard nor teardown replays any saved management command.
use super::*;

const STARTUP_FAILED: &str =
    "Hagency could not finish opening. Select Run to try again. Saved requests are preserved.";

/// Whether the shipped Hagency root installed into `card`. The offline visual
/// review applies the same check, so it cannot pass a bundle the desktop refuses.
pub fn hagency_root_installed(card: &SplashRef) -> bool {
    let Some(card) = card.borrow() else {
        return false;
    };
    // These four sections are unconditional in the shipped root. They need not
    // be direct children: the root groups them in a shell under its alert
    // layer. This checks installation, not geometry: the body can be hidden
    // while Run is applying its source, and readiness/data arrive
    // asynchronously afterward.
    fn mark(id: LiveId, child: WidgetRef, required: &[LiveId; 4], present: &mut [bool; 4]) {
        if child.is_empty() {
            return;
        }
        for (index, expected) in required.iter().enumerate() {
            if id == *expected {
                present[index] = true;
            }
        }
        child.children(&mut |id, child| mark(id, child, required, present));
    }
    let required = [id!(heading), id!(navigation), id!(notice), id!(content)];
    let mut present = [false; 4];
    card.view.children(&mut |id, child| mark(id, child, &required, &mut present));
    present.into_iter().all(|value| value)
}

impl MiniAppsPanel {
    pub(super) fn install_script_source(
        &mut self,
        cx: &mut Cx,
        source: &str,
    ) -> Result<(), String> {
        let card = self.view.splash(cx, ids!(card));
        card.set_text(cx, source);
        if self.package.as_ref().and_then(Package::builtin_id)
            == Some(octosense_app_contract::hagency::APP_ID)
            && !hagency_root_installed(&card)
        {
            // Splash::set_text has no result. A parser/VM budget failure can
            // leave the source retained but install no tree. Revoke its lease,
            // stop timers and discard pending replies before showing controls.
            log!("[HAGENCY] miniapp_startup_failed: required root was not installed");
            self.stop(cx);
            self.view.view(cx, ids!(app_content)).set_visible(cx, false);
            let message = crate::i18n::tr(STARTUP_FAILED).to_owned();
            self.notice(cx, &message);
            self.present_review_controls(cx);
            self.request_confirmation_redraw(cx);
            return Err(message);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "heading := View{}\nnavigation := View{}\nnotice := Label{text: \"Loading\"}\ncontent := View{}";

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("rinx-startup-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> (Cx, MiniAppsPanel, Directory) {
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
        let directory = Directory::new();
        panel.package = Some(
            Package::load_builtin(octosense_app_contract::hagency::APP_ID, directory.path())
                .unwrap(),
        );
        panel.open = true;
        panel.showing_hub = false;
        panel.showing_catalog = false;
        (cx, panel, directory)
    }

    fn lease() -> Lease {
        rinx_miniapp_core::SessionAuthority::default().issue(
            InstanceId {
                app: octosense_app_contract::hagency::APP_ID.into(),
                account: "@owner:example.org".into(),
                room: None,
                generation: 1,
            },
            Default::default(),
            Default::default(),
            Instant::now() + Duration::from_secs(60),
        )
    }

    #[test]
    fn native_hagency_locale_hook_preserves_heap_draft_reply_and_rebake_layout() {
        let (mut cx, mut panel, _directory) = fixture();
        panel.lease = Some(lease());
        let card = panel.view.splash(&cx, ids!(card));
        card.set_host_caps(&mut cx, vec!["fixture.pending".into()]);
        panel.install_script_source(&mut cx, r#"
width: Fill height: Fill flow: Down spacing: 16
let changes = 0
heading := View{}
navigation := View{}
notice := Label{text: ""}
content := View{
    status := Label{text: "en:0"}
    draft := TextInput{text: "User draft 中文 {count}"}
    reply := Label{text: "waiting"}
}
let on_app_locale_changed = fn(locale){
    changes += 1
    ui.content.status.set_text(locale + ":" + changes.to_string())
}
host.request("fixture.pending", {}, fn(result){ ui.content.reply.set_text("original reply") })
"#).unwrap();
        let heap = card.isolate_heap_key(&mut cx).unwrap();
        let original = take_splash_host_requests_for(&[heap]);
        assert_eq!(original.len(), 1);
        cx.handle_script_tasks(); // JIT PORT: upstream drains script tasks through Cx
        let content = card.borrow().unwrap().view.children.iter()
            .find(|(id, _)| *id == id!(content)).unwrap().1.clone();
        let mut draft = WidgetRef::empty();
        content.children(&mut |id, child| {
            if id == id!(draft) { draft = child.clone(); }
        });
        assert!(!draft.is_empty(), "the actual input widget must exist");
        assert!(draft.borrow::<TextInput>().is_some(), "the draft is the native TextInput");
        draft.set_text(&mut cx, "User draft 中文 {count}");
        assert_eq!(draft.text(), "User draft 中文 {count}");
        let draft_uid = draft.widget_uid();
        assert!(matches!(card.borrow().unwrap().view.layout.flow, Flow::Down));
        for apply in [Apply::Rebake, Apply::ScriptReapply, Apply::Rebake] {
            cx.with_vm(|vm| {
                let value = vm.eval(script! {use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
                panel.script_apply(vm, &apply, &mut Scope::empty(), value);
                assert!(vm.take_errors().is_empty());
            });
            assert!(matches!(card.borrow().unwrap().view.layout.flow, Flow::Down),
                "host reapply must retain the running script's vertical root layout");
            assert_eq!(card.borrow().unwrap().view.layout.spacing, 16.0);
            assert_eq!(card.isolate_heap_key(&mut cx), Some(heap));
            assert_eq!(draft.widget_uid(), draft_uid);
            assert_eq!(draft.text(), "User draft 中文 {count}");
            assert!(take_splash_host_requests_for(&[heap]).is_empty());
        }
        panel.refresh_script_language(&mut cx, crate::i18n::Language::Chinese);
        panel.refresh_script_language(&mut cx, crate::i18n::Language::Chinese);
        cx.handle_script_tasks(); // JIT PORT: upstream drains script tasks through Cx
        assert_eq!(card.borrow().unwrap().view.label(&cx, ids!(content.status)).text(), "zh-CN:1");
        assert_eq!(card.isolate_heap_key(&mut cx), Some(heap));
        assert_eq!(draft.widget_uid(), draft_uid);
        assert_eq!(draft.text(), "User draft 中文 {count}");
        assert!(take_splash_host_requests_for(&[heap]).is_empty());
        splash_host_respond(&mut cx, heap, original[0].req_id, Ok("{}"));
        cx.handle_script_tasks(); // JIT PORT: upstream drains script tasks through Cx
        assert_eq!(card.borrow().unwrap().view.label(&cx, ids!(content.reply)).text(), "original reply");
        panel.refresh_script_language(&mut cx, crate::i18n::Language::English);
        cx.handle_script_tasks(); // JIT PORT: upstream drains script tasks through Cx
        assert_eq!(card.borrow().unwrap().view.label(&cx, ids!(content.status)).text(), "en:2");
        assert_eq!(draft.text(), "User draft 中文 {count}");
        assert!(take_splash_host_requests_for(&[heap]).is_empty());
        card.set_text(&mut cx, "");
    }

    #[test]
    fn native_hagency_refresh_visibility_retains_heap_input_and_pending_reply() {
        let (mut cx, mut panel, _directory) = fixture();
        panel.lease = Some(lease());
        let card = panel.view.splash(&cx, ids!(card));
        card.set_host_caps(&mut cx, vec!["fixture.pending".into()]);
        panel.install_script_source(&mut cx, r#"
width: Fill height: Fill flow: Down
let changes=0
heading := View{}
navigation := View{}
notice := Label{text:""}
content := View{
    status := Label{text:"initial"}
    draft := TextInput{text:"draft 中文"}
    reply := Label{text:"waiting"}
}
let on_app_visibility_changed=fn(state){
    changes+=1
    ui.content.status.set_text(state+":"+changes.to_string())
}
host.request("fixture.pending",{},fn(result){ui.content.reply.set_text("original reply")})
"#).unwrap();
        let heap=card.isolate_heap_key(&mut cx).unwrap();
        let pending=take_splash_host_requests_for(&[heap]);
        assert_eq!(pending.len(),1);
        cx.handle_script_tasks(); // JIT PORT: upstream drains script tasks through Cx
        let content=card.borrow().unwrap().view.children.iter()
            .find(|(id,_)| *id==id!(content)).unwrap().1.clone();
        let mut draft=WidgetRef::empty();
        content.children(&mut |id,child| {
            if id==id!(draft) { draft=child.clone(); }
        });
        assert!(draft.borrow::<TextInput>().is_some(),"the actual native input must exist");
        let uid=draft.widget_uid();
        draft.set_text(&mut cx,"retained typed 中文");
        assert_eq!(draft.text(),"retained typed 中文");
        panel.notify_script_visibility(&mut cx,false);
        panel.notify_script_visibility(&mut cx,true);
        cx.handle_script_tasks(); // JIT PORT: upstream drains script tasks through Cx
        assert_eq!(card.borrow().unwrap().view.label(&cx,ids!(content.status)).text(),"visible:2");
        assert_eq!(card.isolate_heap_key(&mut cx),Some(heap));
        assert_eq!(draft.widget_uid(),uid);
        assert_eq!(draft.text(),"retained typed 中文");
        assert!(take_splash_host_requests_for(&[heap]).is_empty(),"visibility cannot replay startup");
        splash_host_respond(&mut cx,heap,pending[0].req_id,Ok("{}"));
        cx.handle_script_tasks(); // JIT PORT: upstream drains script tasks through Cx
        assert_eq!(card.borrow().unwrap().view.label(&cx,ids!(content.reply)).text(),"original reply");
        card.set_text(&mut cx,"");
    }

    #[test]
    fn native_hagency_refresh_missing_visibility_hook_is_optional() {
        let (mut cx,mut panel,_directory)=fixture();
        panel.lease=Some(lease());
        let card=panel.view.splash(&cx,ids!(card));
        panel.install_script_source(&mut cx,ROOT).unwrap();
        assert!(!card.call_script_fn_with_strings(&mut cx,id!(on_app_visibility_changed),&["visible"]));
        panel.notify_script_visibility(&mut cx,false);
        panel.notify_script_visibility(&mut cx,true);
        assert!(hagency_root_installed(&card),"an absent optional hook is harmless");
        card.set_text(&mut cx,"");
    }

    #[test]
    fn native_hagency_startup_same_splash_stop_and_explicit_reopen_installs_root() {
        let (mut cx, mut panel, _directory) = fixture();
        let card = panel.view.splash(&cx, ids!(card));
        let widget = card.widget_uid();
        for _ in 0..3 {
            panel.lease = Some(lease());
            panel.install_script_source(&mut cx, ROOT).unwrap();
            assert!(hagency_root_installed(&card));
            assert_eq!(panel.view.splash(&cx, ids!(card)).widget_uid(), widget);
            panel.stop(&mut cx);
            assert!(!hagency_root_installed(&card));
            assert!(card.isolate_heap_key(&mut cx).is_none());
        }
    }

    #[test]
    fn shipped_hagency_bundle_defines_each_helper_once() {
        // A later `A.name = fn` silently replaces an earlier one. A second
        // `A.local_name` once broke every resource title in the bundle.
        let source = include_str!("../../apps/hagency/bundle/main.splash");
        let mut seen = std::collections::HashSet::new();
        let duplicates: Vec<&str> = source
            .lines()
            .filter_map(|line| {
                let name = line
                    .strip_prefix("A.")?
                    .split(|c: char| c == '=' || c.is_whitespace())
                    .next()?;
                let rest = line[2 + name.len()..].trim_start().strip_prefix('=')?;
                rest.trim_start().starts_with("fn").then_some(name)
            })
            .filter(|name| !seen.insert(*name))
            .collect();
        assert!(seen.len() > 100, "the helper scan found too few definitions");
        assert!(duplicates.is_empty(), "redefined bundle helpers: {duplicates:?}");
    }

    #[test]
    fn native_hagency_startup_finds_sections_grouped_under_an_alert_layer() {
        // The shipped root nests its sections in a shell so alerts can cover
        // the whole app. A direct-children check refused it at every Run.
        let (mut cx, mut panel, _directory) = fixture();
        let card = panel.view.splash(&cx, ids!(card));
        panel.lease = Some(lease());
        let shell = format!(
            "width: Fill height: Fill flow: Overlay\nshell := View{{\n{ROOT}\n}}\nalert_layer := View{{}}"
        );
        panel.install_script_source(&mut cx, &shell).unwrap();
        assert!(hagency_root_installed(&card));
        panel.stop(&mut cx);
        // Nesting does not relax the check: every section must still install.
        panel.lease = Some(lease());
        let partial = "shell := View{\nheading := View{}\nnavigation := View{}\nnotice := Label{text: \"\"}\n}";
        assert!(panel.install_script_source(&mut cx, partial).is_err());
        assert!(!hagency_root_installed(&card));
    }

    #[test]
    fn native_hagency_startup_failure_revokes_partial_instance_and_preserves_saved_requests() {
        for (source, bounded_loop) in [
            ("let broken = }".to_owned(), false),
            ("heading := View{}".to_owned(), false),
            // The jailed write immediately before the loop is observable in
            // the actual widget's sandbox. Parser/API failure before the loop
            // must not count as budget coverage. There are no timer or host
            // API calls whose rejection could substitute for loop execution.
            (
                format!(
                    "fs.write(\"/budget-entered\", \"entered\")\nloop {{}}\nfs.write(\"/budget-returned\", \"returned\")\n{ROOT}"
                ),
                true,
            ),
        ] {
            let (mut cx, mut panel, directory) = fixture();
            let saved = directory.path().join("original-command.json");
            let original = br#"{"commandId":"original","state":"unknown"}"#;
            std::fs::write(&saved, original).unwrap();
            let card = panel.view.splash(&cx, ids!(card));
            card.set_sandbox_dir(&mut cx, Some(directory.path().to_path_buf()));
            let original_lease = lease();
            assert!(original_lease.check("@owner:example.org").is_ok());
            panel.lease = Some(original_lease.clone());
            let timers = cx.script_data.timers.timers.len();
            if bounded_loop {
                // Splash::set_text exposes no error result. The supported
                // validator runs the identical body and returns the precise
                // VM diagnostics; require only the two actual budget bails,
                // not a syntax error, missing API, or generic isolate panic.
                let diagnostics =
                    makepad_widgets::splash::validate_splash_body(&mut cx, &source, false);
                assert!(!diagnostics.is_empty());
                assert!(
                    diagnostics.iter().all(|error| {
                        error.contains("script instruction limit exceeded")
                            || error.contains("script time budget exceeded")
                    }),
                    "unexpected loop diagnostics: {diagnostics:?}"
                );
            }
            assert!(panel.install_script_source(&mut cx, &source).is_err());
            if bounded_loop {
                assert_eq!(
                    std::fs::read(directory.path().join("budget-entered")).unwrap(),
                    b"entered",
                    "the actual installed script must reach the loop"
                );
                assert!(
                    !directory.path().join("budget-returned").exists(),
                    "the bounded loop must not continue into root construction"
                );
            }
            assert!(panel.lease.is_none());
            assert!(original_lease.check("@owner:example.org").is_err());
            assert!(panel.pending.is_empty());
            assert!(!hagency_root_installed(&card));
            assert!(card.text().is_empty());
            assert!(card.isolate_heap_key(&mut cx).is_none());
            assert_eq!(cx.script_data.timers.timers.len(), timers);
            assert!(
                makepad_widgets::splash_host::take_splash_host_requests().is_empty(),
                "failed startup cannot leave a queued host request for later dispatch"
            );
            assert_eq!(std::fs::read(&saved).unwrap(), original);
            assert!(!panel.view.view(&cx, ids!(app_content)).visible());
            assert!(panel.view.view(&cx, ids!(hagency_review)).visible());
            assert!(panel.view.button(&cx, ids!(hagency_run)).visible());
            assert!(panel.view.button(&cx, ids!(hagency_cancel)).visible());
            assert_eq!(panel.notice_text, crate::i18n::tr(STARTUP_FAILED));
            assert_ne!(panel.confirmation_redraw, NextFrame::default());
            // Recovery is explicit and reinstalls the same card. It does not
            // synthesize or execute an original saved management command.
            panel.lease = Some(lease());
            panel.install_script_source(&mut cx, ROOT).unwrap();
            assert!(hagency_root_installed(&card));
            assert_eq!(std::fs::read(&saved).unwrap(), original);
            panel.stop(&mut cx);
        }
    }

    #[test]
    fn native_hagency_startup_diagnostic_is_localized_and_imported_roots_are_unchanged() {
        assert_eq!(
            crate::i18n::text_for(crate::i18n::Language::English, STARTUP_FAILED),
            STARTUP_FAILED
        );
        let chinese = crate::i18n::text_for(crate::i18n::Language::Chinese, STARTUP_FAILED);
        assert_ne!(chinese, STARTUP_FAILED);
        assert!(chinese.contains("运行") && chinese.contains("请求"));
        let (mut cx, mut panel, _directory) = fixture();
        panel.package = None;
        panel
            .install_script_source(&mut cx, "only := Label{text: \"Other app\"}")
            .unwrap();
        assert!(!hagency_root_installed(&panel.view.splash(&cx, ids!(card))));
        panel.stop(&mut cx);
    }
}
