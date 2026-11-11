//! Actual full shipped Splash construction: repeated named children must have
//! separate parents. This checks real widget instances, not screenshot pixels.
use super::*;

const OLD: &str = "agent_capability := choice{";
const FIXED: &str = "View{width:Fit height:Fit agent_capability := choice{";
const TAIL: &str = "on_click: || A.agent_field(\"capability\",capability)}}";
const FIXED_TAIL: &str = "on_click: || A.agent_field(\"capability\",capability)}}}";
const OPEN: &str = r#"
let capability_fixture_open=fn(){
    H.ready=true H.busy=false H.page="agent_wizard" H.command=nil H.agent_review=false
    H.session={account:"@manager:test" features:["agent-admission-v1"]}
    H.agent_project_id="project"
    H.agent_draft={projectId:"project" name:"Writer" description:"Draft" capability:"testing" tokens:"100"}
    H.agent_readiness={projectId:"project" projectName:"Original project" administratorMxid:"@admin:test" canRequest:true canViewRequests:true blockingReason:nil requestContext:{capabilities:["architect","coding","documentation","integration","review","testing"] periodKey:"2026-10" requestedTokenLimit:700}}
    A.save=fn(){return true}
    A.redraw()
}
"#;
fn count_capabilities(widget: &WidgetRef, values: &mut Vec<String>) {
    widget.children(&mut |id, child| {
        if id == id!(agent_capability) {
            values.push(child.text());
        }
        count_capabilities(&child, values);
    });
}
fn fixture(source: &str) -> (Cx, SplashRef) {
    // Test workers are distinct threads; the framework's isolate registrations
    // are thread-local, unlike the app's process-wide startup Once.
    // JIT PORT: as the app does at startup (miniapps/mod.rs).
    makepad_widgets::widget_async::set_splash_bail_on_uncaught_error(true);
    makepad_widgets::widget_async::register_splash_isolate_mod(crate::theme::script_mod);
    makepad_widgets::widget_async::register_splash_isolate_mod(crate::i18n::script_mod);
    makepad_widgets::widget_async::register_splash_isolate_mod(|vm| {
        crate::miniapps::design::script_mod(vm);
    });
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let card = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::i18n::install(vm);
        makepad_code_editor::script_mod(vm);
        crate::shared::script_mod(vm);
        crate::miniapps::script_mod(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = vm.eval(script! {use mod.prelude.widgets.* use mod.widgets.* Splash{}});
        let widget = WidgetRef::script_from_value(vm, value);
        widget.register_as_ui_root(vm);
        let card = widget.as_splash();
        assert!(vm.take_errors().is_empty());
        card
    });
    // As the panel mounts a system app; see operator_tasks_tests.
    card.set_time_budget(&mut cx, Some(SYSTEM_APP_TIME_BUDGET));
    card.set_text(&mut cx, &format!("{source}\n{OPEN}"));
    // The app normally seeds this tree during its first frame. Materialize the
    // real children before entering a callback that mutably borrows Splash.
    assert!(
        !cx.widget_tree()
            .find_within(card.widget_uid(), ids!(heading))
            .is_empty()
    );
    assert!(card.call_script_fn(&mut cx, id!(capability_fixture_open), &[]));
    // Script widget callbacks enqueue render work; drain the same public task
    // pump used by the native event loop before inspecting widget instances.
    cx.handle_script_tasks(); // JIT PORT: upstream drains script tasks through Cx
    (cx, card)
}
#[test]
fn native_hagency_all_six_capabilities_have_distinct_rendered_parents() {
    let source = include_str!("../../apps/hagency/bundle/main.splash");
    assert!(source.contains(FIXED));
    let (mut cx, card) = fixture(source);
    let mut values = Vec::new();
    card.borrow()
        .unwrap()
        .view
        .children(&mut |_, child| count_capabilities(&child, &mut values));
    assert_eq!(
        values,
        vec![
            "Architecture",
            "Coding",
            "Documentation",
            "Integration",
            "Review",
            "Testing"
        ]
    );
    card.set_text(&mut cx, "");
}
#[test]
fn native_hagency_repeated_sibling_id_reproduces_original_last_only_bug() {
    let source = include_str!("../../apps/hagency/bundle/main.splash")
        .replace(FIXED, OLD)
        .replace(FIXED_TAIL, TAIL);
    assert!(source.contains(OLD));
    let (mut cx, card) = fixture(&source);
    let mut values = Vec::new();
    card.borrow()
        .unwrap()
        .view
        .children(&mut |_, child| count_capabilities(&child, &mut values));
    assert_eq!(values, vec!["Testing"]);
    card.set_text(&mut cx, "");
}
