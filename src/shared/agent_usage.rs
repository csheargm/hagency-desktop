//! Presentation only: retain the original Matrix body for copy/forward/retry.
//! Recognize only the exact native completion suffix from an Appservice puppet.
use std::collections::HashSet;
use makepad_widgets::*;
use super::navigation_bar_button::NavigationBarButtonWidgetExt;

#[derive(Debug, PartialEq)]
pub struct UsageInfo<'a> {
    pub answer: &'a str,
    seconds: u64,
    total: u64,
    input: u64,
    output: u64,
    cached: u64,
    reasoning: u64,
    requests: u64,
    returned: u64,
    budget: Option<(&'a str, &'a str, &'a str, &'a str)>,
}
fn number_before<'a>(text: &'a str, delimiter: &str) -> Option<(u64, &'a str)> {
    let (number, rest) = text.split_once(delimiter)?;
    if number.is_empty() || !number.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((number.parse().ok()?, rest))
}
// TODO(R4): agent_usage.rs is over budget; split the suffix parser from the
// usage widget along that seam.
pub fn parse_usage<'a>(sender: &str, body: &'a str, formatted: bool) -> Option<UsageInfo<'a>> {
    if !hagency_core::puppet::is_puppet_localpart(sender) || formatted {
        return None;
    }
    let (answer, suffix) = body.rsplit_once("\n\n✅ 本轮处理已结束 · 已运行 ")?;
    if answer.is_empty() {
        return None;
    }
    let (seconds, rest) = number_before(suffix, " 秒\n本次用量 ")?;
    let (total, rest) = number_before(rest, " Token（输入 ")?;
    let (input, rest) = number_before(rest, " / 输出 ")?;
    let (output, rest) = number_before(rest, "；缓存输入 ")?;
    let (cached, rest) = number_before(rest, "，推理输出 ")?;
    let (reasoning, rest) = number_before(rest, "）· 工具请求 ")?;
    let (requests, rest) = number_before(rest, " 次，已返回 ")?;
    let (returned, rest) = number_before(rest, " 次")?;
    if input.checked_add(output)? != total
        || cached > input
        || reasoning > output
        || returned > requests
    {
        return None;
    }
    let budget = if rest.is_empty() {
        None
    } else {
        let rest = rest.strip_prefix("\n\nAgent 配额（")?;
        let (period, rest) = rest.split_once("）：已使用 ")?;
        if !matches!(period, "累计" | "UTC 当日" | "UTC 当月") {
            return None;
        }
        let (spent, rest) = rest.split_once(" / ")?;
        spent.parse::<u64>().ok()?;
        let (limit, rest) = rest.split_once(" Token；待确认或运行中的预留 ")?;
        if !matches!(limit, "不限额" | "未设置") {
            limit.parse::<u64>().ok()?;
        }
        let held = rest.strip_suffix(" Token。")?;
        held.parse::<u64>().ok()?;
        Some((period, spent, limit, held))
    };
    Some(UsageInfo {
        answer,
        seconds,
        total,
        input,
        output,
        cached,
        reasoning,
        requests,
        returned,
        budget,
    })
}
impl UsageInfo<'_> {
    fn details(&self) -> String {
        let mut text = crate::i18n::format(
            "{seconds}s · {tokens} tokens · tools {returned}/{requests}",
            &[
                ("seconds", self.seconds.to_string()),
                ("tokens", self.total.to_string()),
                ("returned", self.returned.to_string()),
                ("requests", self.requests.to_string()),
            ],
        );
        text.push('\n');
        text.push_str(&crate::i18n::format(
            "Input {input} · output {output} · cached {cached} · reasoning {reasoning}",
            &[
                ("input", self.input.to_string()),
                ("output", self.output.to_string()),
                ("cached", self.cached.to_string()),
                ("reasoning", self.reasoning.to_string()),
            ],
        ));
        if let Some((period, spent, limit, held)) = self.budget {
            text.push('\n');
            text.push_str(&crate::i18n::format(
                "Budget ({period}) {spent}/{limit} · reserved {held}",
                &[
                    (
                        "period",
                        match period {
                            "累计" => crate::i18n::tr("Lifetime"),
                            "UTC 当日" => crate::i18n::tr("UTC day"),
                            _ => crate::i18n::tr("UTC month"),
                        }
                        .into(),
                    ),
                    ("spent", spent.into()),
                    (
                        "limit",
                        match limit {
                            "不限额" => crate::i18n::tr("Unlimited"),
                            "未设置" => crate::i18n::tr("Unset"),
                            _ => limit,
                        }
                        .into(),
                    ),
                    ("held", held.into()),
                ],
            ));
        }
        text
    }
}
#[derive(Default)]
struct ExpandedUsage(HashSet<String>);
pub fn clear_session(cx: &mut Cx) {
    cx.global::<ExpandedUsage>().0.clear();
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    mod.widgets.AgentUsage = #(AgentUsage::register_widget(vm)) {
        ..mod.widgets.View
        visible: false width: Fill height: Fit flow: Down spacing: 3
        show_toggle: true show_details: true
        toggle := NavigationBarButton {
            width: 24 height: 24 padding: 0
            tooltip_text: #(crate::i18n::tr("Show or hide response usage"))
            Icon {width: 14 height: 14 draw_icon +: {svg: mod.widgets.ICON_USAGE color: mod.widgets.RINX_MUTED}}
        }
        details := Label {
            visible: false width: Fill height: Fit padding: Inset{top: 1 bottom: 2}
            draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (9 * mod.widgets.RINX_TEXT_SCALE)}}
        }
    }
}
#[derive(Script, ScriptHook, Widget)]
pub struct AgentUsage {
    #[deref]
    view: View,
    #[live]
    show_toggle: bool,
    #[live]
    show_details: bool,
    #[rust]
    identity: String,
    #[rust]
    present: bool,
}
impl AgentUsage {
    fn toggle(&mut self, cx: &mut Cx) {
        let expanded = &mut cx.global::<ExpandedUsage>().0;
        if !expanded.remove(&self.identity) {
            if expanded.len() >= 2048 {
                expanded.clear();
            }
            expanded.insert(self.identity.clone());
        }
        self.render(cx);
        self.redraw(cx);
    }
    fn render(&mut self, cx: &mut Cx) {
        let expanded = cx.global::<ExpandedUsage>().0.contains(&self.identity);
        self.view.set_visible(
            cx,
            self.present && (self.show_toggle || self.show_details && expanded),
        );
        self.view
            .navigation_bar_button(cx, ids!(toggle))
            .set_visible(cx, self.show_toggle);
        self.view
            .label(cx, ids!(details))
            .set_visible(cx, self.show_details && expanded);
        self.view
            .navigation_bar_button(cx, ids!(toggle))
            .set_selected(cx, expanded);
    }
}
impl Widget for AgentUsage {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            if self
                .view
                .navigation_bar_button(cx, ids!(toggle))
                .clicked(actions)
            {
                self.toggle(cx);
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.render(cx);
        self.view.draw_walk(cx, scope, walk)
    }
}
impl AgentUsageRef {
    pub fn populate(&self, cx: &mut Cx, identity: String, usage: Option<&UsageInfo<'_>>) {
        let Some(mut inner) = self.borrow_mut() else {
            return;
        };
        inner.identity = identity;
        inner.present = usage.is_some();
        inner
            .view
            .label(cx, ids!(details))
            .set_text(cx, &usage.map_or_else(String::new, UsageInfo::details));
        inner.render(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const BODY: &str = "Answer\n\n✅ 本轮处理已结束 · 已运行 3 秒\n本次用量 50 Token（输入 30 / 输出 20；缓存输入 10，推理输出 5）· 工具请求 2 次，已返回 1 次";
    #[test]
    fn exact_puppet_suffix_only_leaves_original_answer() {
        let usage = parse_usage("_hagency_agt_01abc", BODY, false).unwrap();
        assert_eq!(usage.answer, "Answer");
        assert_eq!(usage.total, 50);
        assert!(parse_usage("alice", BODY, false).is_none());
        assert_eq!(parse_usage("_hagency_ann_helper", BODY, false).unwrap().answer, "Answer", "readable puppet IDs");
        assert!(parse_usage("_hagency_service", BODY, false).is_none());
        assert!(parse_usage("_hagency_agt_01abc", BODY, true).is_none());
        assert!(
            parse_usage(
                "_hagency_agt_01abc",
                &BODY.replace("50 Token", "65 Token"),
                false
            )
            .is_none()
        );
        assert!(
            parse_usage(
                "_hagency_agt_01abc",
                &format!("{BODY}\nextra user text"),
                false
            )
            .is_none()
        );
    }
    #[test]
    fn warnings_and_incomplete_statuses_stay_visible_in_body() {
        for body in [
            "⏸️ 暂停处理：Token 配额未设置或可用额度不足。",
            "⚠️ 处理失败",
            "✅ 本轮处理已结束",
            "\n\n✅ 本轮处理已结束 · 已运行 0 秒",
        ] {
            assert!(parse_usage("_hagency_agt_01abc", body, false).is_none());
        }
    }
    #[test]
    fn private_budget_and_repeated_footer_keep_last_suffix_only() {
        let body = format!(
            "{BODY}\n\nAgent 配额（累计）：已使用 70 / 100 Token；待确认或运行中的预留 10 Token。"
        );
        let usage = parse_usage("_hagency_agt_01abc", &body, false).unwrap();
        assert_eq!(usage.budget, Some(("累计", "70", "100", "10")));
        let body = format!(
            "{BODY}\n\nquoted text\n\n{}",
            BODY.split_once("\n\n").unwrap().1
        );
        assert!(
            parse_usage("_hagency_agt_01abc", &body, false)
                .unwrap()
                .answer
                .contains("quoted text")
        );
    }
    #[test]
    fn usage_widget_recycling_keeps_expansion_scoped_to_account_room_and_event() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut widget = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            vm.bx.captured_errors = Some(Vec::new());
            crate::app::register_widgets(vm);
            let value = script_eval!(vm,{mod.widgets.AgentUsage {}});
            let widget = AgentUsage::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty());
            widget
        });
        widget.present = true;
        widget.identity = "account-a|room|event-a".into();
        widget.render(&mut cx);
        assert!(!widget.view.label(&mut cx, ids!(details)).visible());
        widget.toggle(&mut cx);
        assert!(widget.view.label(&mut cx, ids!(details)).visible());
        widget.identity = "account-b|room|event-a".into();
        widget.render(&mut cx);
        assert!(!widget.view.label(&mut cx, ids!(details)).visible());
        widget.identity = "account-a|room|event-a".into();
        widget.render(&mut cx);
        assert!(widget.view.label(&mut cx, ids!(details)).visible());
        clear_session(&mut cx);
        widget.render(&mut cx);
        assert!(!widget.view.label(&mut cx, ids!(details)).visible());
    }
    #[test]
    fn name_row_toggle_and_footer_share_expansion_without_empty_footer() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let (mut header, mut footer) = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            vm.bx.captured_errors = Some(Vec::new());
            crate::app::register_widgets(vm);
            let value = script_eval!(vm,{mod.widgets.AgentUsage {show_details: false}});
            let header = AgentUsage::script_from_value(vm, value);
            let value = script_eval!(vm,{mod.widgets.AgentUsage {show_toggle: false}});
            let footer = AgentUsage::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty());
            (header, footer)
        });
        for widget in [&mut header, &mut footer] {
            widget.present = true;
            widget.identity = "account|room|event".into();
            widget.render(&mut cx);
        }
        assert!(header.view.visible());
        assert!(!footer.view.visible());
        header.toggle(&mut cx);
        footer.render(&mut cx);
        assert!(footer.view.visible());
        assert!(footer.view.label(&mut cx, ids!(details)).visible());
        assert!(!header.view.label(&mut cx, ids!(details)).visible());
        header.toggle(&mut cx);
        footer.render(&mut cx);
        assert!(!footer.view.visible());
        header.present = false;
        header.render(&mut cx);
        assert!(!header.view.visible());
    }
}
