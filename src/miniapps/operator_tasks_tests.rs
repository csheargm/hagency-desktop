//! Real full-bundle widget construction, not pixel or live-account acceptance.
use super::*;
const OPEN: &str = r#"
let task_fixture_open=fn(){
    H.ready=true H.busy=false H.page="operator_tasks" H.command=nil H.task_filters_open=true
    H.session={account:"@owner:test" features:[]}
    H.task_page={supported:true tasks:[{id:"task_a" title:"Original A" status:"created" priority:"p1" assignee:nil updatedAt:"old"},{id:"task_b" title:"Original B" status:"blocked" priority:"p2" assignee:"Agent" updatedAt:"later"}] nextOffset:nil}
    A.redraw()
}
"#;
fn fixture(source: &str) -> (Cx, SplashRef) {
    fixture_with_open(source, OPEN)
}
fn fixture_with_open(source: &str, open: &str) -> (Cx, SplashRef) {
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
        // The desktop mini-app host gives Splash a finite viewport. The default
        // Fit root has no established height in this offscreen draw harness,
        // which incorrectly clamps multiline inputs to zero through ancestors.
        let value = vm.eval(script! {use mod.prelude.widgets.* use mod.widgets.* Splash{width:Fill height:Fill}});
        let widget = WidgetRef::script_from_value(vm, value);
        widget.register_as_ui_root(vm);
        let card = widget.as_splash();
        assert!(vm.take_errors().is_empty());
        card
    });
    // The panel gives its system apps this budget; the full bundle does not
    // fit Makepad's default in an unoptimised test build.
    card.set_time_budget(&mut cx, Some(SYSTEM_APP_TIME_BUDGET));
    card.set_text(&mut cx, &format!("{source}\n{open}"));
    // The app normally seeds this tree during its first frame. Materialize the
    // real children before entering a callback that mutably borrows Splash.
    assert!(!cx
        .widget_tree()
        .find_within(card.widget_uid(), ids!(heading))
        .is_empty());
    assert!(card.call_script_fn(&mut cx, id!(task_fixture_open), &[]));
    // Script widget callbacks enqueue render work; drain the same public task
    // pump used by the native event loop before inspecting widget instances.
    cx.handle_script_tasks(); // JIT PORT: upstream drains script tasks through Cx
    (cx, card)
}

fn count(widget: &WidgetRef, id: LiveId) -> usize {
    let mut found = 0;
    widget.children(&mut |child_id, child| {
        if child_id == id {
            found += 1;
        }
        found += count(&child, id);
    });
    found
}
#[test]
fn operator_tasks_real_full_bundle_keeps_distinct_rows_and_all_status_controls() {
    let (mut cx, card) = fixture(include_str!("../../apps/hagency/bundle/main.splash"));
    let root = card.borrow().unwrap();
    let mut opens = 0;
    let mut statuses = 0;
    root.view.children(&mut |_, child| {
        opens += count(&child, id!(task_open));
        statuses += count(&child, id!(task_status));
    });
    assert_eq!(opens, 2);
    assert_eq!(statuses, 6);
    drop(root);
    card.set_text(&mut cx, "");
}

fn collect(widget: &WidgetRef, id: LiveId, result: &mut Vec<WidgetRef>) {
    widget.children(&mut |child_id, child| {
        if child_id == id {
            result.push(child.clone());
        }
        collect(&child, id, result);
    });
}
fn targets(card: &SplashRef, id: LiveId) -> Vec<WidgetRef> {
    let mut result = Vec::new();
    card.borrow()
        .unwrap()
        .view
        .children(&mut |child_id, child| {
            if child_id == id {
                result.push(child.clone());
            }
            collect(&child, id, &mut result);
        });
    result
}
fn draw_card(cx: &mut Cx, card: &SplashRef) {
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;
    let pass = DrawPass::new(cx);
    pass.set_size(cx, dvec2(1000., 800.));
    let mut list = DrawList2d::new(cx);
    let event = DrawEvent::default();
    let mut draw = CxDraw::new(cx, &event);
    let mut cx2d = Cx2d::new(&mut draw);
    cx2d.begin_pass(&pass, None);
    list.begin_always(&mut cx2d);
    cx2d.begin_root_turtle(dvec2(1000., 800.), Layout::flow_overlay());
    card.borrow_mut()
        .unwrap()
        .draw_all(&mut cx2d, &mut Scope::empty());
    cx2d.end_pass_sized_turtle();
    list.end(&mut cx2d);
    cx2d.end_pass(&pass);
}
const BOARD: &str = r#"
let task_fixture_open=fn(){
 H.ready=true H.busy=false H.page="operator_board" H.command=nil
 H.session={account:"@owner:test" features:[]}
 H.task_board={supported:true projectCount:0 agentCount:0 taskTotals:{created:0 accepted:0 in_progress:0 blocked:0 done:0} projects:[] nextAfter:nil}
 A.redraw()
}
"#;
#[test]
fn operator_tasks_board_counter_bounds_preserve_all_five_values() {
    for cn in [false, true] {
        let base = include_str!("../../apps/hagency/bundle/main.splash");
        let localized = if cn {
            base.replace("if mod.rinx_i18n.language() == \"zh-CN\"", "if true")
        } else {
            base.into()
        };
        let (mut cx, card) = fixture_with_open(&localized, BOARD);
        draw_card(&mut cx, &card);
        let counters = targets(&card, id!(board_counter));
        assert_eq!(counters.len(), 5);
        for counter in counters {
            let r = counter.area().rect(&cx);
            assert!(
                r.size.x > 20.
                    && r.size.y > 0.
                    && r.size.y < 70.
                    && r.pos.x >= 0.
                    && r.pos.x + r.size.x <= 1000.5
                    && r.pos.y + r.size.y <= 800.5,
                "counter {} {r:?}",
                counter.text()
            );
        }
        card.set_text(&mut cx, "");
    }
}
#[test]
fn operator_tasks_empty_results_and_create_are_in_initial_viewport() {
    let source = include_str!("../../apps/hagency/bundle/main.splash");
    let open = r#"
let task_fixture_open=fn(){H.ready=true H.busy=false H.page="operator_tasks" H.command=nil H.task_filters_open=false H.session={account:"@owner:test" features:[]} H.task_page={supported:true writeSupported:true writeOperations:["create"] tasks:[] nextOffset:nil} A.redraw()}
"#;
    let (mut cx, card) = fixture_with_open(source, open);
    draw_card(&mut cx, &card);
    assert!(targets(&card, id!(task_status)).is_empty());
    for id in [id!(task_create), id!(task_empty), id!(task_filters_toggle)] {
        let items = targets(&card, id);
        assert_eq!(items.len(), 1);
        let r = items[0].area().rect(&cx);
        assert!(
            r.size.x > 0. && r.size.y > 0. && r.pos.y + r.size.y <= 800.5,
            "{id:?} {r:?}"
        );
    }
    card.set_text(&mut cx, "");
}

#[test]
fn operator_tasks_original_fit_wrapper_reproduces_zero_width_counters() {
    let source = include_str!("../../apps/hagency/bundle/main.splash").replace(
        "board_counter := mod.widgets.HagencyCopy{width:Fit",
        "board_counter := mod.widgets.HagencyCopy{",
    );
    let (mut cx, card) = fixture_with_open(&source, BOARD);
    draw_card(&mut cx, &card);
    let counters = targets(&card, id!(board_counter));
    assert_eq!(counters.len(), 5);
    assert!(
        counters.iter().any(|v| v.area().rect(&cx).size.x < 1.),
        "original Fit/Fill layout must reproduce zero-width counters"
    );
    card.set_text(&mut cx, "");
}

#[test]
fn operator_tasks_editor_primary_action_precedes_optional_fields_in_viewport() {
    for operation in ["create", "comment"] {
        let open = format!(
            r#"
let task_fixture_open=fn(){{
 H.ready=true H.busy=false H.command=nil H.task_phase="" H.session={{account:"@owner:test" features:[]}}
 H.task_page={{supported:true writeSupported:true writeOperations:["create"] tasks:[] nextOffset:nil}}
 H.task_id="task_original" H.task_detail={{supported:true writeSupported:true writeOperations:["comment"] revision:7 task:{{id:"task_original" title:"A useful task"}}}}
 A.task_begin("{operation}","")
}}
"#
        );
        let (mut cx, card) =
            fixture_with_open(include_str!("../../apps/hagency/bundle/main.splash"), &open);
        draw_card(&mut cx, &card);
        let buttons = targets(&card, id!(task_prepare));
        assert_eq!(buttons.len(), 1);
        let r = buttons[0].area().rect(&cx);
        assert!(
            r.size.x > 0. && r.size.y > 0. && r.pos.y + r.size.y <= 800.5,
            "{operation} primary save bounds {r:?}"
        );
        assert!(targets(&card, id!(task_edit_priority)).is_empty());
        assert!(targets(&card, id!(task_parent_input)).is_empty());
        card.set_text(&mut cx, "");
    }
}

#[test]
fn readmission_form_constructs_real_text_inputs_and_freezes_during_submission() {
    for cn in [false, true] {
        for busy in [false, true] {
            let source = include_str!("../../apps/hagency/bundle/main.splash");
            let localized = if cn {
                source.replace("if mod.rinx_i18n.language() == \"zh-CN\"", "if true")
            } else {
                source.into()
            };
            let open = format!(
                r#"
let task_fixture_open=fn(){{
 H.ready=true H.busy={busy} H.command=nil H.page="connection_reoffer"
 H.session={{account:"@owner:test" administrator:"@admin:test" features:["resource-readmission-v1"]}}
 H.reoffer_draft={{resourceId:"original_resource" label:"Original resource" coordinatorMxid:"@coordinator:test" review:false resource:{{model:"test-model" framework:"codex"}}}}
 A.redraw()
}}
"#
            );
            let (mut cx, card) = fixture_with_open(&localized, &open);
            assert!(targets(&card, id!(reoffer_coordinator)).is_empty());
            for id in [id!(reoffer_label)] {
                let fields = targets(&card, id);
                assert_eq!(
                    fields.len(),
                    1,
                    "reoffer input failed real widget construction: {id:?}"
                );
                assert_eq!(fields[0].as_text_input().is_read_only(), busy);
            }
            assert_eq!(targets(&card, id!(reoffer_review)).len(), 1);
            card.set_text(&mut cx, "");
        }
    }
}

fn stopped_decision_fixture(busy: bool, pending: bool) -> String {
    format!(
        r#"
let task_fixture_open=fn(){{
 H.ready=true H.busy=false H.command=nil H.session={{account:"@owner:test" features:[]}}
 let row={{attempt:{{resourceId:"resource" engagementId:"engagement" dispatchId:"dispatch" agent:"Worker" projectId:"project" taskId:"task" fence:1 reason:"owned_runner_failure" inspectionAvailable:true sharedResourceAuthority:true}} taskTitle:"Inspect report" taskStatus:"blocked" revision:"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" resolvable:true canContinue:true entryCount:0 entries:[] nextEntry:nil}}
 A.stopped_review_received({{reviewCommandId:"review" inspectionId:"inspection" expiresAt:9999999999999 inspection:row}})
 A.choose_stopped("continue")
 H.stopped_note="Verified effects" H.stopped_instruction="Finish remaining work"
 H.busy={busy}
 if {pending} {{H.command={{service:"hagency.stopped.resolve" args:{{commandId:"resolve"}}}}}}
 A.redraw()
}}
"#
    )
}

#[test]
fn stopped_decision_constructs_real_inputs_and_freezes_when_busy_or_pending() {
    for cn in [false, true] {
        for (busy, pending) in [(false, false), (true, false), (false, true)] {
            let source = include_str!("../../apps/hagency/bundle/main.splash");
            let localized = if cn {
                source.replace("if mod.rinx_i18n.language() == \"zh-CN\"", "if true")
            } else {
                source.into()
            };
            let (mut cx, card) = fixture_with_open(&localized, &stopped_decision_fixture(busy, pending));
            for id in [id!(stopped_note), id!(stopped_instruction)] {
                let fields = targets(&card, id);
                assert_eq!(fields.len(), 1, "Recovery text input did not construct: {id:?}");
                assert_eq!(fields[0].as_text_input().is_read_only(), busy || pending);
            }
            assert_eq!(targets(&card, id!(stopped_confirm_review)).len(), 1);
            card.set_text(&mut cx, "");
        }
    }
}

#[test]
fn stopped_decision_original_enabled_property_reproduces_render_failure() {
    let source = include_str!("../../apps/hagency/bundle/main.splash").replace(
        "mod.widgets.HagencyInput{is_read_only:!A.stopped_can_decide()",
        "mod.widgets.HagencyInput{enabled:A.stopped_can_decide()",
    );
    let (mut cx, card) = fixture_with_open(&source, &stopped_decision_fixture(false, false));
    assert!(targets(&card, id!(stopped_note)).is_empty());
    assert!(targets(&card, id!(stopped_instruction)).is_empty());
    assert!(targets(&card, id!(stopped_confirm_review)).is_empty());
    card.set_text(&mut cx, "");
}

#[test]
fn recovery_ux_quarantined_workspace_prioritizes_inspection_over_waiting() {
    fn has_text(widget: &WidgetRef, expected: &str) -> bool {
        let mut found = widget.text() == expected;
        widget.children(&mut |_, child| found |= has_text(&child, expected));
        found
    }
    for cn in [false, true] {
        for quarantined in [false, true] {
            let source = include_str!("../../apps/hagency/bundle/main.splash");
            let localized = if cn {
                source.replace("if mod.rinx_i18n.language() == \"zh-CN\"", "if true")
            } else {
                source.into()
            };
            let block = if quarantined { "inspection_required" } else { "active_execution" };
            let open = format!(r#"
let task_fixture_open=fn(){{
 H.ready=true H.busy=false H.command=nil H.session={{account:"@owner:test" features:[]}}
 H.page="workspace" H.workspace_mode="detail" H.workspace_resource="resource" H.workspace_id="workspace"
 H.workspace_detail={{resourceId:"resource" workspaceId:"workspace" dirty:true activeLeases:1 quarantined:{quarantined} sharedResourceCount:1 releaseBlock:"{block}" lastReleasedAt:nil revision:"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}
 A.redraw()
}}
"#);
            let (mut cx, card) = fixture_with_open(&localized, &open);
            let inspection = if cn {
                "有执行结果尚未确认。请先检查已停止的任务，再解除工作区标记。"
            } else {
                "An execution outcome is unresolved. Inspect the stopped task before releasing this workspace."
            };
            let waiting = if cn {
                "任务仍占用此工作区，请等待任务结束后刷新。"
            } else {
                "A task still holds this workspace. Wait for it to finish, then refresh."
            };
            let mut inspected = false;
            let mut waited = false;
            card.borrow().unwrap().view.children(&mut |_, child| {
                inspected |= has_text(&child, inspection);
                waited |= has_text(&child, waiting);
            });
            assert_eq!(inspected, quarantined);
            assert_eq!(waited, !quarantined);
            assert_eq!(targets(&card, id!(workspace_stopped)).len(), usize::from(quarantined));
            assert!(targets(&card, id!(workspace_review)).is_empty());
            card.set_text(&mut cx, "");
        }
    }
}

#[test]
fn recovery_ux_continue_form_reduces_height_without_losing_multiline_inputs() {
    for cn in [false, true] {
        let source = include_str!("../../apps/hagency/bundle/main.splash");
        let localized = if cn {
            source.replace("if mod.rinx_i18n.language() == \"zh-CN\"", "if true")
        } else {
            source.into()
        };
        let original = localized.replace("height:64 is_multiline:true", "height:80 is_multiline:true");
        let mut bottoms = Vec::new();
        for version in [&original, &localized] {
            let (mut cx, card) = fixture_with_open(version, &stopped_decision_fixture(false, false));
            draw_card(&mut cx, &card);
            // The instruction stays a multiline field; the note is an optional single line
            // beside one-tap answers, so the form no longer requires typing a check.
            for (id, min_height) in [(id!(stopped_instruction), 60.), (id!(stopped_note), 30.)] {
                let fields = targets(&card, id);
                assert_eq!(fields.len(), 1);
                let rect = fields[0].area().rect(&cx);
                assert!(rect.size.x > 100. && rect.size.y >= min_height, "{id:?} {rect:?}");
                assert!(!fields[0].as_text_input().is_read_only());
            }
            assert!(!targets(&card, id!(stopped_check_pick)).is_empty(), "one-tap answers render");
            let actions = targets(&card, id!(stopped_confirm_review));
            assert_eq!(actions.len(), 1);
            let rect = actions[0].area().rect(&cx);
            assert!(rect.size.x > 0. && rect.size.y > 0., "{rect:?}");
            bottoms.push(rect.pos.y + rect.size.y);
            card.set_text(&mut cx, "");
        }
        assert!(bottoms[1] <= bottoms[0] - 15., "Expected 16px less form height: {bottoms:?}");
    }
}

/// The mini app and the native confirmation windows share one owner vocabulary
/// (每月上限, 每次申请上限, 这台电脑, Agent, token). Older technical words must not
/// come back into the bundle's Chinese text.
///
/// The capacity legend has three parts that add up to the monthly maximum: 已用 (used),
/// 已分出 (given to Agents, not used yet) and 剩余 (left). The maximum can't go below
/// 已用和已分出; the per-request limit can't go below what 正在运行的 Agent 已拿到. The
/// glossary defines exactly these terms, and the retired words (已承诺, 已分给 Agent，尚未用,
/// 已分给正在运行的 Agent, 所占的部分) stay out of every page.
#[test]
fn bundle_uses_the_owner_vocabulary() {
    let source = include_str!("../../apps/hagency/bundle/main.splash");
    for banned in [
        "每月总上限", "本设备", "这台设备", "智能体", "令牌", "授权意向", "加密权限详情", "接着原来的启动",
        // Other words for what Agents hold, and for the committed total.
        "已分到", "分到了额度", "已占用", "已计入", "承诺给 Agent", "保留已有额度", "已有额度",
        // Holds worded as reservations on pool, request and approval pages.
        "为它预留", "为这个 Agent 预留", "没有从资源池预留", "额度已预留", "已有本地预留", "容量预留", "容量已预留",
        "未能预留额度", "控制新的预留",
        // One way of confirming a change: "已保存：每月上限 1 M → 2 M", never these.
        "已保存。每月上限从", "已保存。每月上限现在是", "已修改。现在每次申请最多", "已记下：你知道了",
        // Restart jargon and the old verb for adding people.
        "完成上次的启动", "点「选上」",
        // Retired in round 7: the committed total, the second name for what Agents hold,
        // the catalogue and approval-room jargon, and the second Add on Who Can Request.
        "已承诺", "尚未用", "已分给正在运行的", "所占的部分", "对外展示", "审批聊天", "在目录中列出",
        "点「添加」会把对方", "审批对话（高级）",
        // Retired in round 8: a second verb for committing a maximum, a list that seemed to
        // expire, the per-request floor as a fragment or a "lower limit", raw server text on a
        // primary row, admin jargon, and a lock that sounded permanent.
        "保存每月上限", "本月底到期", "每次申请上限的下限", "最低约 ", "服务器原文（英文）：", "\"Connection Check\",\"连接诊断\"",
        "（电脑要开着）", "显示后，模型和每月上限不能修改", "点下方「保存更改」", "加入本月名单",
        // Retired in round 10: the catalogue as 目录 on the pool's pages (it's the pool's public
        // introduction), the daily amount in raw tokens, stopping Agents "under Requests", and a
        // suggestion pill that didn't say why.
        "在目录里显示", "目录里的角色", "目录介绍", "在目录里介绍", "每天 token 数", "在「申请」里停止",
        "A.t(\"Suggested \",\"建议 \")",
    ] {
        assert!(!source.contains(banned), "the bundle says {banned}");
    }
    // The page and the native confirmation windows say one thing one way: every shared term and
    // verb is the windows' own translation, word for word ("改成 X" commits a new value in both).
    let catalog: std::collections::BTreeMap<String, String> =
        serde_json::from_str(include_str!("../../resources/i18n/zh-CN.json")).unwrap();
    for english in [
        "Monthly maximum", "Per-request limit", "Held by running Agents", "Used and given out this month",
        "This month’s list only grows: people already on it can’t be removed.", "People who can request",
        "Add People Who Can Request", "Add to This Month’s List", "This pool", "Choose Again", "Publish Ahead",
        "Account limit", "Change to {amount}",
    ] {
        let chinese = catalog.get(english).unwrap_or_else(|| panic!("no translation for {english}")).replace("{amount}", "");
        assert!(source.contains(&chinese), "the windows say {chinese:?} for {english:?}, the bundle doesn't");
    }
    // The page's dates are on this device's clock and name its zone, as the windows do: the
    // session carries the offset and the zone's label ("北京时间", "UTC+8").
    assert!(
        source.contains("localUtcOffsetMinutes") && source.contains("localZoneLabel"),
        "the pool page doesn't name the zone of its dates"
    );
    // Manager-side project-reservation pages may still say 预留; the owner's pool, request,
    // approval, alert and device helpers may not.
    let owner = ["pool_", "reqs_", "dev_", "alert_", "who_", "health_", "stopped_", "onboarding_", "local_pool"];
    let mut helper: Option<&str> = None;
    for (index, line) in source.lines().enumerate() {
        if let Some(rest) = line.strip_prefix("A.") {
            let name = rest.split(|c: char| c == '=' || c.is_whitespace()).next().unwrap_or("");
            if rest[name.len()..].trim_start().strip_prefix('=').is_some_and(|r| r.trim_start().starts_with("fn")) {
                helper = Some(name);
            }
        } else if !line.is_empty() && !line.starts_with(char::is_whitespace) && !line.starts_with('}') {
            helper = None;
        }
        if let Some(name) = helper.filter(|name| owner.iter().any(|prefix| name.starts_with(prefix))) {
            // Interrupted-call reconciliation names its actual ledger reservation.
            assert!(name.starts_with("pool_call_") || !line.contains("预留"), "A.{name} says 预留 on line {}: {line}", index + 1);
        }
    }
    // The glossary is the source of these words, the secondary pages' too (one tap from those pages).
    for term in [
        r#"term:A.t("Used","已用")"#,
        r#"term:A.t("Given out","已分出")"#,
        r#"term:A.t("Left","剩余")"#,
        r#"term:A.t("Held by running Agents","正在运行的 Agent 已拿到")"#,
        r#"term:A.t("Workspace","工作区")"#,
        r#"term:A.t("Approval chat","审批对话")"#,
        r#"term:A.t("Public introduction","公开介绍")"#,
        "不能低于已用和已分出",
    ] {
        assert!(source.contains(term), "the glossary doesn't define {term}");
    }
    // The legend, floors and refusals quote the same words.
    for pattern in [
        "\"已保存：每月上限 \"", "\"已保存：每次申请上限 \"", "A.t(\"Not saved: \",\"没有保存：\")",
        // One sentence for the per-request floor, its estimate always 约.
        "不能低于正在运行的 Agent 已拿到的约 ", "（确认窗口里有准确数字）",
        "不能低于本月已用和已分出的 ", "（已用和已分出）", "\"已分出 \"", "\"已用 \"",
        // The save bar names the setting it saves; a suggestion says why.
        "\"每月上限改成 \"", "\"每次申请上限改成 \"", "每月上限的 1/4",
    ] {
        assert!(source.contains(pattern), "the bundle doesn't say {pattern}");
    }
}
