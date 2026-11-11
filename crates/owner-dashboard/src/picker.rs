//! Searchable, inline filters. The parent scroll view owns the geometry: no
//! popup can extend beyond its window, and only one bounded page is mounted.
use crate::controls::OdListWidgetExt;
use crate::i18n::Locale;
use makepad_widgets::*;
use std::collections::{HashMap, HashSet};

pub const PAGE_SIZE: usize = 5;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let PickerButton = Button {
        width: Fill height: Fit{min: FitBound.Abs(OD_CONTROL)} margin: 0
        flow: Flow.Right{wrap: true} align: Align{x: 0.0 y: 0.5}
        padding: Inset{left: 12 right: 12 top: 12 bottom: 12}
        label_walk: Walk{width: Fill height: Fit}
        grab_key_focus: true
        draw_bg +: {
            color: OD_FIELD color_hover: OD_HOVER color_down: OD_PRESSED color_focus: OD_SELECTED color_disabled: OD_FIELD
            color_2: vec4(-1., -1., -1., -1.)
            border_radius: OD_RADIUS_SMALL border_size: 1.0
            border_color: OD_SEPARATOR border_color_hover: OD_SEPARATOR border_color_down: OD_ACCENT
            border_color_focus: OD_ACCENT border_color_disabled: OD_SEPARATOR
        }
        draw_text +: {
            color: OD_INK color_hover: OD_INK color_down: OD_INK color_focus: OD_INK color_disabled: OD_MUTED
            text_style: theme.font_regular{font_size: OD_SIZE_BODY}
        }
    }
    let PickerCaption = Label {
        width: Fill height: Fit padding: 0 flow: Flow.Right{wrap: true}
        draw_text +: {color: OD_MUTED text_style: theme.font_regular{font_size: OD_SIZE_CAPTION}}
    }
    mod.widgets.ODFilterPicker = #(OdFilterPicker::register_widget(vm)) {
        width: Fill height: Fit flow: Down spacing: 8
        picker_toggle := PickerButton {}
        picker_body := View {
            visible: false width: Fill height: Fit flow: Down spacing: 8
            picker_search := TextInput {
                width: Fill height: OD_CONTROL margin: 0
                padding: Inset{left: 12 right: 12 top: 10 bottom: 10}
                draw_bg +: {color: OD_SURFACE border_color: OD_SEPARATOR border_color_focus: OD_ACCENT border_radius: OD_RADIUS_SMALL}
                draw_text +: {color: OD_INK text_style: theme.font_regular{font_size: OD_SIZE_BODY}}
            }
            picker_clear := PickerButton {visible: false}
            picker_count := PickerCaption {}
            picker_choices := OdList {
                spacing: 4
                Choice := View {
                    width: Fill height: Fit flow: Down
                    // The actual Button supplies a complete accessible name;
                    // this marker exposes selected state to native adapters.
                    sel := View {visible: false width: 0 height: 0}
                    picker_choice := PickerButton {}
                }
            }
            picker_paging := View {
                width: Fill height: Fit flow: Right spacing: 8
                picker_previous := PickerButton {}
                picker_next := PickerButton {}
            }
            picker_close := PickerButton {}
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Choice {
    id: Option<String>,
    name: String,
    label: String,
}
impl Choice {
    fn key(&self) -> String { self.id.as_ref().map(|id| format!("id:{id}")).unwrap_or_else(|| "all".into()) }
}

#[derive(Default)]
struct PickerState {
    choices: Vec<Choice>,
    selected: Option<String>,
    selected_label: String,
    query: String,
    page: usize,
    active: Option<String>, // stable row key, including the explicit All row
    open: bool,
}
impl PickerState {
    fn update(&mut self, choices: Vec<(Option<String>, String)>, selected: &Option<String>) -> bool {
        let mut seen = HashSet::new();
        let choices: Vec<_> = choices.into_iter().filter(|(id, _)| seen.insert(id.clone())).collect();
        let mut counts = HashMap::new();
        for (_, name) in &choices { *counts.entry(name.trim().to_lowercase()).or_insert(0usize) += 1; }
        let choices: Vec<_> = choices.into_iter().map(|(id, name)| {
            let label = if counts.get(&name.trim().to_lowercase()).copied().unwrap_or(0) > 1 {
                id.as_ref().map(|id| format!("{name}\n{id}")).unwrap_or_else(|| name.clone())
            } else { name.clone() };
            Choice { id, name, label }
        }).collect();
        let changed = choices != self.choices || selected != &self.selected;
        if !changed { return false; }
        if let Some(choice) = choices.iter().find(|choice| &choice.id == selected) {
            self.selected_label = choice.label.clone();
        } else if selected != &self.selected || self.selected_label.is_empty() {
            self.selected_label = selected.clone().unwrap_or_default();
        }
        self.choices = choices;
        self.selected = selected.clone();
        self.reconcile();
        true
    }
    fn matching(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        self.choices.iter().enumerate().filter(|(_, choice)| query.is_empty()
            || choice.name.to_lowercase().contains(&query)
            || choice.id.as_deref().unwrap_or("").to_lowercase().contains(&query))
            .map(|(index, _)| index).collect()
    }
    fn reconcile(&mut self) {
        let matches = self.matching();
        self.page = self.page.min(matches.len().saturating_sub(1) / PAGE_SIZE);
        if !matches.iter().skip(self.page * PAGE_SIZE).take(PAGE_SIZE).any(|index| Some(self.choices[*index].key()) == self.active) {
            self.active = matches.get(self.page * PAGE_SIZE).map(|index| self.choices[*index].key());
        }
    }
    fn search(&mut self, query: String) {
        if self.query == query { return; }
        self.query = query;
        self.page = 0;
        self.active = None;
        self.reconcile();
    }
    fn page(&mut self, forward: bool) {
        let matches = self.matching();
        let last = matches.len().saturating_sub(1) / PAGE_SIZE;
        self.page = if forward { (self.page + 1).min(last) } else { self.page.saturating_sub(1) };
        self.active = matches.get(self.page * PAGE_SIZE).map(|index| self.choices[*index].key());
    }
    fn advance(&mut self, backward: bool, from_search: bool) -> Option<String> {
        let matches = self.matching();
        let current = matches.iter().position(|index| Some(self.choices[*index].key()) == self.active);
        let next = if from_search { if backward { matches.len().checked_sub(1)? } else { 0 } }
            else if backward { current.unwrap_or(0).saturating_sub(1) }
            else { current.map_or(0, |i| i + 1).min(matches.len().saturating_sub(1)) };
        let index = *matches.get(next)?;
        self.page = next / PAGE_SIZE;
        let key = self.choices[index].key();
        self.active = Some(key.clone());
        Some(key)
    }
    fn select(&mut self, key: &str) -> Option<Option<String>> {
        let choice = self.choices.iter().find(|choice| choice.key() == key)?;
        let selected = choice.id.clone();
        self.selected = selected.clone();
        self.selected_label = choice.label.clone();
        self.open = false;
        Some(selected)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum PickerAction {
    #[default]
    None,
    Changed(Option<String>),
}

enum FocusTarget { Toggle, Search, Choice(String) }

#[derive(Script, Widget)]
pub struct OdFilterPicker {
    #[deref] view: View,
    #[rust] state: PickerState,
    #[rust] label: String,
    #[rust] locale: Locale,
    #[rust] dirty: bool,
    #[rust] pending_focus: Option<FocusTarget>,
}
impl ScriptHook for OdFilterPicker {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, _apply: &Apply, _scope: &mut Scope, _value: ScriptValue) { self.dirty = true; }
}

impl OdFilterPicker {
    fn words(&self, en: &'static str, zh: &'static str) -> &'static str { if self.locale == Locale::ZhHans { zh } else { en } }
    fn mark(&mut self, cx: &mut Cx) { self.dirty = true; self.view.redraw(cx); }
    fn close(&mut self, cx: &mut Cx) {
        self.state.open = false;
        self.pending_focus = Some(FocusTarget::Toggle);
        self.mark(cx);
    }
    fn choose(&mut self, cx: &mut Cx, key: &str) {
        if let Some(selected) = self.state.select(key) {
            cx.widget_action(self.widget_uid(), PickerAction::Changed(selected));
            self.pending_focus = Some(FocusTarget::Toggle);
            self.mark(cx);
        }
    }
    fn rows(&self, cx: &Cx) -> Vec<(String, WidgetRef)> { self.view.od_list(cx, ids!(picker_choices)).rows() }
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.view.button(cx, ids!(picker_toggle)).clicked(actions) {
            self.state.open = !self.state.open;
            self.pending_focus = Some(if self.state.open { FocusTarget::Search } else { FocusTarget::Toggle });
            self.mark(cx);
        }
        if !self.state.open { return; }
        let input = self.view.text_input(cx, ids!(picker_search));
        if let Some(query) = input.changed(actions) { self.state.search(query); self.mark(cx); }
        if input.escaped(actions) || self.view.button(cx, ids!(picker_close)).clicked(actions) { self.close(cx); return; }
        if self.view.button(cx, ids!(picker_clear)).clicked(actions) {
            self.state.search(String::new());
            input.set_text(cx, "");
            self.pending_focus = Some(FocusTarget::Search);
            self.mark(cx);
        }
        if input.returned(actions).is_some() {
            if let Some(key) = self.state.active.clone() { self.choose(cx, &key); }
            return;
        }
        for (key, row) in self.rows(cx) {
            if row.button(cx, ids!(picker_choice)).clicked(actions) { self.choose(cx, &key); return; }
        }
        for (path, forward) in [(ids!(picker_previous), false), (ids!(picker_next), true)] {
            if self.view.button(cx, path).clicked(actions) {
                self.state.page(forward);
                self.pending_focus = self.state.active.clone().map(FocusTarget::Choice);
                self.mark(cx);
            }
        }
    }
    fn handle_key(&mut self, cx: &mut Cx, key: &KeyEvent) -> bool {
        if !self.state.open || key.modifiers.is_primary() || key.modifiers.alt { return false; }
        let focus = cx.key_focus();
        let search = self.view.widget(cx, ids!(picker_search)).area();
        let rows = self.rows(cx);
        let row = rows.iter().find(|(_, row)| row.widget(cx, ids!(picker_choice)).area() == focus);
        let ours = focus == search || row.is_some() || [ids!(picker_toggle), ids!(picker_clear), ids!(picker_previous), ids!(picker_next), ids!(picker_close)]
            .iter().any(|path| self.view.widget(cx, *path).area() == focus);
        if !ours { return false; }
        match key.key_code {
            KeyCode::Escape => { self.close(cx); true }
            KeyCode::ArrowDown | KeyCode::ArrowUp => {
                if let Some((id, _)) = row { self.state.active = Some(id.clone()); }
                if let Some(key) = self.state.advance(key.key_code == KeyCode::ArrowUp, focus == search) {
                    self.pending_focus = Some(FocusTarget::Choice(key));
                    self.mark(cx);
                }
                true
            }
            _ => false,
        }
    }
    fn sync(&mut self, cx: &mut Cx) {
        self.dirty = false;
        let absent = !self.state.choices.iter().any(|choice| choice.id == self.state.selected);
        let suffix = if absent { self.words(" · unavailable in this snapshot", " · 当前快照未包含") } else { "" };
        let arrow = if self.state.open { "▴" } else { "▾" };
        let text = format!("{} · {}{suffix}  {arrow}", self.label, self.state.selected_label);
        self.view.button(cx, ids!(picker_toggle)).set_text(cx, &text);
        self.view.view(cx, ids!(picker_body)).set_visible(cx, self.state.open);
        if !self.state.open { return; }
        let search = self.view.text_input(cx, ids!(picker_search));
        search.set_empty_text(cx, self.words("Search name or ID", "搜索名称或 ID").into());
        if search.text() != self.state.query { search.set_text(cx, &self.state.query); }
        self.view.button(cx, ids!(picker_clear)).set_text(cx, self.words("Clear search", "清除搜索"));
        self.view.widget(cx, ids!(picker_clear)).set_visible(cx, !self.state.query.is_empty());
        self.view.button(cx, ids!(picker_close)).set_text(cx, self.words("Done", "完成"));
        let matches = self.state.matching();
        let start = self.state.page * PAGE_SIZE;
        let end = (start + PAGE_SIZE).min(matches.len());
        let count = if matches.is_empty() { self.words("No matches. Try another name or ID.", "没有匹配结果，请尝试其他名称或 ID。").into() }
            else if self.locale == Locale::ZhHans { format!("第 {}–{} 项，共 {} 项", start + 1, end, matches.len()) }
            else { format!("{}–{} of {}", start + 1, end, matches.len()) };
        self.view.label(cx, ids!(picker_count)).set_text(cx, &count);
        let wanted: Vec<_> = matches[start..end].iter().map(|index| (self.state.choices[*index].key(), id!(Choice))).collect();
        let rows = self.view.od_list(cx, ids!(picker_choices)).sync(cx, &wanted);
        for (row, index) in rows.iter().zip(&matches[start..end]) {
            let choice = &self.state.choices[*index];
            let selected = choice.id == self.state.selected;
            row.widget(cx, ids!(sel)).set_visible(cx, selected);
            let text = if selected { format!("✓ {}", choice.label) } else { choice.label.clone() };
            row.button(cx, ids!(picker_choice)).set_text(cx, &text);
        }
        self.view.view(cx, ids!(picker_paging)).set_visible(cx, matches.len() > PAGE_SIZE);
        let previous = self.view.button(cx, ids!(picker_previous));
        previous.set_text(cx, self.words("Previous", "上一页"));
        previous.set_enabled(cx, self.state.page > 0);
        let next = self.view.button(cx, ids!(picker_next));
        next.set_text(cx, self.words("Next", "下一页"));
        next.set_enabled(cx, end < matches.len());
    }
}

impl Widget for OdFilterPicker {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::KeyDown(key) = event { if self.handle_key(cx, key) { return; } }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event { self.handle_actions(cx, actions); }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.dirty { self.sync(cx); }
        let step = self.view.draw_walk(cx, scope, walk);
        if !step.is_step() {
            if let Some(target) = self.pending_focus.take() {
                let area = match target {
                    FocusTarget::Toggle => self.view.widget(cx, ids!(picker_toggle)).area(),
                    FocusTarget::Search => self.view.widget(cx, ids!(picker_search)).area(),
                    FocusTarget::Choice(key) => self.rows(cx).into_iter().find(|(id, _)| id == &key)
                        .map(|(_, row)| row.widget(cx, ids!(picker_choice)).area()).unwrap_or(Area::Empty),
                };
                if area.is_valid(cx) { crate::keyboard::focus(cx, area); }
            }
        }
        step
    }
}

impl OdFilterPickerRef {
    /// Account changes revoke this UI scope as well as its data. Ordinary
    /// same-owner snapshot refreshes must use set_choices instead.
    pub fn reset(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.state = PickerState::default();
            inner.pending_focus = None;
            inner.view.text_input(cx, ids!(picker_search)).set_text(cx, "");
            inner.view.od_list(cx, ids!(picker_choices)).sync(cx, &[]);
            inner.view.label(cx, ids!(picker_count)).set_text(cx, "");
            inner.view.button(cx, ids!(picker_toggle)).set_text(cx, &inner.label);
            inner.view.view(cx, ids!(picker_body)).set_visible(cx, false);
            inner.mark(cx);
        }
    }
    pub fn set_choices(&self, cx: &mut Cx, choices: Vec<(Option<String>, String)>, selected: &Option<String>, label: String) {
        if let Some(mut inner) = self.borrow_mut() {
            let changed = inner.state.update(choices, selected);
            if changed || inner.label != label { inner.label = label; inner.mark(cx); }
        }
    }
    pub fn set_locale(&self, cx: &mut Cx, locale: Locale) {
        if let Some(mut inner) = self.borrow_mut() { if inner.locale != locale { inner.locale = locale; inner.mark(cx); } }
    }
    /// Outer None means no action. Some(None) is the explicit All choice.
    pub fn changed(&self, actions: &Actions) -> Option<Option<String>> {
        actions.filter_widget_actions_cast::<PickerAction>(self.widget_uid()).find_map(|action| match action {
            PickerAction::Changed(value) => Some(value), _ => None,
        })
    }
    pub fn is_open(&self) -> bool { self.borrow().is_some_and(|inner| inner.state.open) }
    pub fn close(&self, cx: &mut Cx) { if let Some(mut inner) = self.borrow_mut() { if inner.state.open { inner.close(cx); } } }
    /// Close another picker without moving focus away from the newly opened
    /// search field. Used by the containing filter group for exclusivity.
    pub fn collapse(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.pending_focus = None;
            if inner.state.open { inner.state.open = false; inner.mark(cx); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn choices() -> Vec<(Option<String>, String)> {
        std::iter::once((None, "All resources".into())).chain((0..61).map(|i| (Some(format!("pool-{i}")), format!("Resource {i}")))).collect()
    }
    #[test]
    fn paging_reaches_every_choice_and_selection_is_an_id() {
        let mut state = PickerState::default();
        state.update(choices(), &None);
        let mut seen = HashSet::new();
        loop {
            let matches = state.matching();
            let start = state.page * PAGE_SIZE;
            for i in &matches[start..(start + PAGE_SIZE).min(matches.len())] { seen.insert(state.choices[*i].key()); }
            if start + PAGE_SIZE >= matches.len() { break; }
            state.page(true);
        }
        assert_eq!(seen.len(), 62);
        assert_eq!(state.select("id:pool-60"), Some(Some("pool-60".into())));
        assert_eq!(state.select("all"), Some(None));
        assert_eq!(state.select("missing"), None);
    }
    #[test]
    fn refresh_preserves_query_page_active_id_and_missing_selection() {
        let mut state = PickerState::default();
        state.update(choices(), &Some("pool-42".into()));
        state.search("RESOURCE".into());
        state.page(true);
        let active = state.active.clone();
        let mut renamed = choices();
        renamed[1].1 = "Resource updated".into();
        state.update(renamed, &Some("pool-42".into()));
        assert_eq!(state.query, "RESOURCE");
        assert_eq!(state.page, 1);
        assert_eq!(state.active, active);
        state.update(vec![(None, "All resources".into())], &Some("pool-42".into()));
        assert_eq!(state.selected.as_deref(), Some("pool-42"));
        assert_eq!(state.selected_label, "Resource 42");
        assert_eq!(state.page, 0);
    }
    #[test]
    fn duplicate_names_get_full_ids_and_search_matches_name_or_id() {
        let mut state = PickerState::default();
        state.update(vec![(None,"All".into()), (Some("a".into()),"小黄".into()), (Some("b-long-id".into()),"小黄".into())], &None);
        assert_eq!(state.choices[1].label, "小黄\na");
        assert_eq!(state.choices[2].label, "小黄\nb-long-id");
        state.search("小黄".into());
        assert_eq!(state.matching(), vec![1, 2]);
        state.search("LONG-ID".into());
        assert_eq!(state.matching(), vec![2]);
        assert_eq!(state.advance(false, true).as_deref(), Some("id:b-long-id"));
        state.search("absent".into());
        assert!(state.advance(false, true).is_none());
    }
    #[test]
    fn keyboard_crosses_pages_without_skipping_matches() {
        let mut state = PickerState::default();
        state.update(choices(), &None);
        assert_eq!(state.advance(false, true).as_deref(), Some("all"));
        for _ in 0..5 { state.advance(false, false); }
        assert_eq!(state.active.as_deref(), Some("id:pool-4"));
        assert_eq!(state.page, 1);
        state.advance(true, false);
        assert_eq!(state.page, 0);
        assert_eq!(state.active.as_deref(), Some("id:pool-3"));
    }

    #[test]
    fn native_picker_wraps_full_names_and_refresh_keeps_search_widget() {
        use makepad_widgets::desktop_style::{self, DesktopStyle, StyleSheet};
        for dark in [false, true] {
            let mut cx = Cx::new(Box::new(|_, _| {}));
            let widget = cx.with_vm(|vm| {
                desktop_style::install(vm, StyleSheet::load(DesktopStyle::Macos.sheet(dark)) /* JIT PORT: sheets load by entry */);
                vm.bx.captured_errors = Some(Vec::new());
                makepad_widgets::script_mod(vm);
                crate::script_mod(vm);
                let value = script_eval!(vm, { mod.widgets.ODFilterPicker{} });
                let widget = WidgetRef::script_from_value(vm, value);
                assert!(vm.take_errors().is_empty(), "picker registration and construction");
                widget
            });
            let picker = widget.as_od_filter_picker();
            picker.set_choices(&mut cx, choices(), &None, "Resource".into());
            let search_uid;
            {
                let mut inner = picker.borrow_mut().unwrap();
                inner.state.open = true;
                inner.state.search("resource".into());
                inner.sync(&mut cx);
                search_uid = inner.view.widget(&cx, ids!(picker_search)).widget_uid();
                assert_eq!(inner.rows(&cx).len(), PAGE_SIZE);
                assert_eq!(inner.view.text_input(&cx, ids!(picker_search)).text(), "resource");
            }
            let long = "Resource with a complete descriptive name that must wrap instead of being clipped";
            let mut updated = choices();
            updated[1].1 = long.into();
            picker.set_choices(&mut cx, updated, &None, "Resource".into());
            {
                let mut inner = picker.borrow_mut().unwrap();
                inner.sync(&mut cx);
                assert_eq!(inner.view.widget(&cx, ids!(picker_search)).widget_uid(), search_uid);
                assert_eq!(inner.view.text_input(&cx, ids!(picker_search)).text(), "resource");
                let rows = inner.rows(&cx);
                let row = rows.iter().find(|(key, _)| key == "id:pool-0").unwrap().1.clone();
                let button = row.widget(&cx, ids!(picker_choice));
                assert_eq!(button.text(), long, "visible and accessible text is complete");
                assert!(matches!(button.walk(&mut cx).height, Size::Fit { min: Some(FitBound::Abs(height)), .. } if height >= 44.0));
            }
            let actions = cx.capture_actions(|cx| picker.borrow_mut().unwrap().choose(cx, "all"));
            assert_eq!(picker.changed(&actions), Some(None));
            assert_eq!(picker.changed(&ActionsBuf::new()), None);
            assert!(!picker.is_open());
            // A different authenticated owner must not inherit private search
            // terms, option names, selected IDs, or a pending focus transfer.
            {
                let mut inner = picker.borrow_mut().unwrap();
                inner.state.open = true;
                inner.state.search("private old-owner query".into());
                inner.pending_focus = Some(FocusTarget::Search);
            }
            picker.reset(&mut cx);
            {
                let inner = picker.borrow().unwrap();
                assert!(inner.state.query.is_empty() && inner.state.choices.is_empty());
                assert!(inner.state.selected.is_none() && inner.state.selected_label.is_empty());
                assert!(inner.state.active.is_none() && inner.pending_focus.is_none());
                assert!(!inner.state.open && inner.state.page == 0);
                assert!(inner.rows(&cx).is_empty());
                assert!(inner.view.text_input(&cx, ids!(picker_search)).text().is_empty());
                assert_eq!(inner.view.button(&cx, ids!(picker_toggle)).text(), "Resource");
            }
        }
    }
}
