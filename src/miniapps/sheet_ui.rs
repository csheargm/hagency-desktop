//! One family of native confirmation sheets. The grant window (publish,
//! change, add people, publish ahead) and the monthly-maximum window compute
//! a `SheetView` from native state only and write it through `apply_sheet`,
//! so every window has the same status icon, before → after hero, grouped
//! facts, people list, sharing period row, technical details disclosure and
//! full-width footer.
use super::*;

/// What the sheet's status icon and title say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SheetTone {
    /// A change waiting for the owner, shown with what it does.
    #[default]
    Proposal,
    /// Reading, saving or publishing: a spinner.
    Working,
    /// It worked: a green check.
    Done,
    /// It was refused and nothing changed: a red warning.
    Failed,
    /// The outcome isn't known yet, or the record no longer applies: amber.
    Unsure,
}
/// What a proposal does, for its icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SheetGlyph {
    #[default]
    Publish,
    Change,
    People,
    Renew,
    Maximum,
}
/// One big number in the hero, with a small tag under it ("Now", "After").
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Figure {
    pub value: String,
    pub tag: String,
}
impl Figure {
    pub(super) fn new(value: impl Into<String>, tag: impl Into<String>) -> Self {
        Self { value: value.into(), tag: tag.into() }
    }
}
/// One grouped row: a name, its value, and an optional note under it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Fact {
    pub label: String,
    pub value: String,
    pub note: String,
}
impl Fact {
    pub(super) fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self { label: label.into(), value: value.into(), note: String::new() }
    }
    pub(super) fn note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }
}
/// One person on a list: `@name`, whether this window adds them, and whether
/// the owner may still take them off before confirming.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Person {
    pub id: String,
    pub name: String,
    pub added: bool,
    pub removable: bool,
}
/// Everything one rendering of a sheet shows, already in the owner's language.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SheetView {
    pub tone: SheetTone,
    pub glyph: SheetGlyph,
    pub title: String,
    pub pool: String,
    /// The hero: what quantity, before → after, and one sentence.
    pub caption: String,
    pub before: Option<Figure>,
    pub after: Option<Figure>,
    pub lead: String,
    pub status: String,
    /// What is wrong with the choices shown, said beside them.
    pub problem: String,
    /// A consequence the owner must know before confirming.
    pub caution: String,
    /// A script's proposal that this window did not apply, and why.
    pub notice: Option<String>,
    pub facts_label: String,
    pub facts: Vec<Fact>,
    pub people_visible: bool,
    pub people_label: String,
    pub people: Vec<Person>,
    pub people_note: String,
    /// A list this window doesn't change folds into one row ("People who
    /// can request · 3 people") that opens in place.
    pub people_collapsible: bool,
    pub people_open: bool,
    pub people_summary: String,
    /// The add-one-person row under the list.
    pub people_editable: bool,
    pub add_placeholder: String,
    pub add_error: String,
    pub add_hint: String,
    /// A record of this month's list offers to add people in this window.
    pub people_add_visible: bool,
    pub choosing: bool,
    pub quota_visible: bool,
    /// What the slider sets: the per-request limit or the monthly maximum.
    pub quota_label: String,
    pub calendar: Option<hagency_core::recurring_quota::Calendar>,
    pub renewal_mode: Option<hagency_core::recurring_quota::RenewalMode>,
    pub quota_hint: String,
    /// The technical details disclosure.
    pub more_label: String,
    pub more_open: bool,
    /// When sharing ends: a visible row (`expiry` is its value) that opens
    /// the 1 day / 7 days / to month end choice and its note in place.
    pub sharing_visible: bool,
    pub sharing_open: bool,
    pub expiry: String,
    pub expiry_note: String,
    pub technical: String,
    pub recover: String,
    pub recover_visible: bool,
    pub cancel_intent_visible: bool,
    pub primary: String,
    pub primary_visible: bool,
    pub primary_enabled: bool,
    /// The primary button runs the recovery step (Check Again, Choose Again).
    pub primary_recovers: bool,
    /// The primary button closes the sheet: Done after a finished change.
    pub primary_closes: bool,
    pub cancel: String,
    /// The amount a refusal's primary button ("Change to 0.6 M") puts back in the form.
    pub fix_tokens: Option<u64>,
    /// The recovery step stays a row of its own beside a primary that also recovers:
    /// "Choose Again" under "Change to 2 M".
    pub recover_secondary: bool,
}
impl SheetView {
    /// Whether the hero card has anything to show.
    pub(super) fn hero_visible(&self) -> bool {
        self.after.is_some() || !self.caption.is_empty() || !self.lead.is_empty()
    }
    /// Whether the details row has anything behind it.
    pub(super) fn more_visible(&self) -> bool {
        !self.technical.is_empty()
    }
    /// Whether the recovery step shows as its own row: when the primary button
    /// isn't that step, or when it fixes the value and this row picks again.
    pub(super) fn recover_row(&self) -> bool {
        self.recover_visible && (!self.primary_recovers || self.recover_secondary)
    }
}
/// The text scale the sheet's layout reads (`RINX_TEXT_SCALE`), as it is now.
fn text_scale(cx: &mut Cx) -> f64 {
    cx.with_vm(|vm| {
        let widgets = vm.module(id!(widgets));
        vm.bx
            .heap
            .value(widgets, LiveId::from_str("RINX_TEXT_SCALE").into(), NoTrap)
            .as_number()
            .unwrap_or(1.0)
    })
}
/// The widget path of `rest` under one sheet root.
pub(super) fn sheet_path(root: LiveId, rest: &[LiveId]) -> Vec<LiveId> {
    let mut path = Vec::with_capacity(rest.len() + 1);
    path.push(root);
    path.extend_from_slice(rest);
    path
}
pub(super) const FACT_ROWS: usize = 6;
pub(super) const PEOPLE_ROWS: usize = 8;
const FACTS: [LiveId; FACT_ROWS] = [id!(r0), id!(r1), id!(r2), id!(r3), id!(r4), id!(r5)];
pub(super) const PEOPLE: [LiveId; PEOPLE_ROWS] =
    [id!(p0), id!(p1), id!(p2), id!(p3), id!(p4), id!(p5), id!(p6), id!(p7)];
/// How many people rows are drawn, the last row giving way to a count.
pub(super) fn people_shown(count: usize) -> usize {
    if count > PEOPLE_ROWS { PEOPLE_ROWS - 1 } else { count }
}
impl MiniAppsPanel {
    /// Write one computed sheet into the sheet under `root`. The native fields
    /// (amount, the add-person field) are left alone; callers refill them.
    pub(super) fn apply_sheet(&mut self, cx: &mut Cx, root: LiveId, v: &SheetView) {
        let tr = crate::i18n::tr;
        let at = |rest: &[LiveId]| sheet_path(root, rest);
        let text = |this: &mut Self, cx: &mut Cx, path: &[LiveId], value: &str| {
            this.view.label(cx, &at(path)).set_text(cx, value);
            this.view.widget(cx, &at(path)).set_visible(cx, !value.is_empty());
        };
        let show = |this: &mut Self, cx: &mut Cx, path: &[LiveId], visible: bool| {
            this.view.widget(cx, &at(path)).set_visible(cx, visible);
        };
        // The status icon: what a proposal does, or how it ended.
        let icon = match v.tone {
            SheetTone::Proposal => match v.glyph {
                SheetGlyph::Publish => id!(glyph_publish),
                SheetGlyph::Change => id!(glyph_change),
                SheetGlyph::People => id!(glyph_people),
                SheetGlyph::Renew => id!(glyph_renew),
                SheetGlyph::Maximum => id!(glyph_maximum),
            },
            SheetTone::Working => id!(glyph_working),
            SheetTone::Done => id!(glyph_done),
            SheetTone::Failed => id!(glyph_failed),
            SheetTone::Unsure => id!(glyph_unsure),
        };
        for badge in [
            id!(glyph_publish), id!(glyph_change), id!(glyph_people), id!(glyph_renew), id!(glyph_maximum),
            id!(glyph_working), id!(glyph_done), id!(glyph_failed), id!(glyph_unsure),
        ] {
            show(self, cx, &[id!(sheet_scroll), id!(heading), id!(icons), badge], badge == icon);
        }
        self.view.label(cx, &at(&[id!(sheet_scroll), id!(heading), id!(title)])).set_text(cx, &v.title);
        text(self, cx, &[id!(sheet_scroll), id!(heading), id!(pool)], &v.pool);
        // The hero: before → after in big numbers, then one sentence.
        show(self, cx, &[id!(sheet_scroll), id!(hero)], v.hero_visible());
        text(self, cx, &[id!(sheet_scroll), id!(hero), id!(caption)], &v.caption);
        for (slot, figure) in [(id!(before), &v.before), (id!(after), &v.after)] {
            show(self, cx, &[id!(sheet_scroll), id!(hero), id!(figures), slot], figure.is_some());
            let figure = figure.clone().unwrap_or_default();
            text(self, cx, &[id!(sheet_scroll), id!(hero), id!(figures), slot, id!(value)], &figure.value);
            text(self, cx, &[id!(sheet_scroll), id!(hero), id!(figures), slot, id!(tag)], &figure.tag);
        }
        show(self, cx, &[id!(sheet_scroll), id!(hero), id!(figures)], v.after.is_some());
        show(self, cx, &[id!(sheet_scroll), id!(hero), id!(figures), id!(arrow)], v.before.is_some() && v.after.is_some());
        text(self, cx, &[id!(sheet_scroll), id!(hero), id!(lead)], &v.lead);
        text(self, cx, &[id!(sheet_scroll), id!(status)], &v.status);
        let mut status = self.view.label(cx, &at(&[id!(sheet_scroll), id!(status)]));
        match v.tone {
            SheetTone::Failed => script_apply_eval!(cx, status, {draw_text +: {color: mod.widgets.RINX_DANGER_FG}}),
            SheetTone::Unsure => script_apply_eval!(cx, status, {draw_text +: {color: mod.widgets.RINX_WARNING_FG}}),
            SheetTone::Done => script_apply_eval!(cx, status, {draw_text +: {color: mod.widgets.RINX_SUCCESS_FG}}),
            _ => script_apply_eval!(cx, status, {draw_text +: {color: mod.widgets.RINX_INK}}),
        }
        show(self, cx, &[id!(sheet_scroll), id!(problem_banner)], !v.problem.is_empty());
        text(self, cx, &[id!(sheet_scroll), id!(problem_banner), id!(problem)], &v.problem);
        show(self, cx, &[id!(sheet_scroll), id!(caution)], !v.caution.is_empty());
        self.view.label(cx, &at(&[id!(sheet_scroll), id!(caution), id!(caution_text)])).set_text(cx, &v.caution);
        text(self, cx, &[id!(sheet_scroll), id!(change)], v.notice.as_deref().unwrap_or_default());
        // Grouped facts; the first visible row has no line above it.
        show(self, cx, &[id!(sheet_scroll), id!(facts)], !v.facts.is_empty());
        text(self, cx, &[id!(sheet_scroll), id!(facts), id!(facts_label)], &v.facts_label);
        for (index, slot) in FACTS.iter().enumerate() {
            let fact = v.facts.get(index);
            show(self, cx, &[id!(sheet_scroll), id!(facts), *slot], fact.is_some());
            let fact = fact.cloned().unwrap_or_default();
            show(self, cx, &[id!(sheet_scroll), id!(facts), *slot, id!(line)], index > 0);
            self.view.label(cx, &at(&[id!(sheet_scroll), id!(facts), *slot, id!(body), id!(label)])).set_text(cx, &fact.label);
            text(self, cx, &[id!(sheet_scroll), id!(facts), *slot, id!(body), id!(value)], &fact.value);
            text(self, cx, &[id!(sheet_scroll), id!(facts), *slot, id!(note)], &fact.note);
        }
        // People: listed people locked, people added here removable. A list
        // this window doesn't change starts folded into one row.
        show(self, cx, &[id!(sheet_scroll), id!(people)], v.people_visible);
        let folded = v.people_collapsible && !v.people_open;
        text(self, cx, &[id!(sheet_scroll), id!(people), id!(people_label)], if v.people_collapsible { "" } else { &v.people_label });
        let toggle = [id!(sheet_scroll), id!(people), id!(list), id!(people_toggle_row)];
        show(self, cx, &toggle, v.people_collapsible);
        self.view
            .label(cx, &at(&[id!(sheet_scroll), id!(people), id!(list), id!(people_toggle_row), id!(people_toggle_body), id!(people_toggle_label)]))
            .set_text(cx, tr("People who can request"));
        self.view
            .label(cx, &at(&[id!(sheet_scroll), id!(people), id!(list), id!(people_toggle_row), id!(people_toggle_body), id!(people_toggle_value)]))
            .set_text(cx, &v.people_summary);
        show(self, cx, &[id!(sheet_scroll), id!(people), id!(list), id!(people_toggle_row), id!(people_toggle_body), id!(people_closed)], !v.people_open);
        show(self, cx, &[id!(sheet_scroll), id!(people), id!(list), id!(people_toggle_row), id!(people_toggle_body), id!(people_opened)], v.people_open);
        let shown = if folded { 0 } else { people_shown(v.people.len()) };
        for (index, slot) in PEOPLE.iter().enumerate() {
            let person = v.people.get(index).filter(|_| index < shown);
            let row = [id!(sheet_scroll), id!(people), id!(list), *slot];
            show(self, cx, &row, person.is_some());
            let person = person.cloned().unwrap_or_default();
            show(self, cx, &[id!(sheet_scroll), id!(people), id!(list), *slot, id!(line)], index > 0 || v.people_collapsible);
            self.view.label(cx, &at(&[id!(sheet_scroll), id!(people), id!(list), *slot, id!(body), id!(name)])).set_text(cx, &person.name);
            show(self, cx, &[id!(sheet_scroll), id!(people), id!(list), *slot, id!(body), id!(tag)], person.added);
            self.view
                .label(cx, &at(&[id!(sheet_scroll), id!(people), id!(list), *slot, id!(body), id!(tag), id!(tag_text)]))
                .set_text(cx, tr("New"));
            show(self, cx, &[id!(sheet_scroll), id!(people), id!(list), *slot, id!(body), id!(locked)], !person.removable && !person.added);
            show(self, cx, &[id!(sheet_scroll), id!(people), id!(list), *slot, id!(body), id!(remove)], person.removable);
        }
        let more = if folded { 0 } else { v.people.len().saturating_sub(shown) };
        let more_text = if more > 0 {
            crate::i18n::format("And {count} more", &[("count", more.to_string())])
        } else {
            String::new()
        };
        text(self, cx, &[id!(sheet_scroll), id!(people), id!(list), id!(more_people)], &more_text);
        show(self, cx, &[id!(sheet_scroll), id!(people), id!(list), id!(add)], v.people_editable && !folded);
        show(self, cx, &[id!(sheet_scroll), id!(people), id!(list), id!(open_add)], v.people_add_visible && !folded);
        show(self, cx, &[id!(sheet_scroll), id!(people), id!(list), id!(add), id!(add_line)], !v.people.is_empty() || v.people_collapsible);
        self.view
            .text_input(cx, &at(&[id!(sheet_scroll), id!(people), id!(list), id!(add), id!(add_name)]))
            .set_empty_text(cx, v.add_placeholder.clone());
        text(self, cx, &[id!(sheet_scroll), id!(people), id!(list), id!(add), id!(add_error)], &v.add_error);
        text(self, cx, &[id!(sheet_scroll), id!(people), id!(list), id!(add), id!(add_hint)], if v.add_error.is_empty() { &v.add_hint } else { "" });
        show(self, cx, &[id!(sheet_scroll), id!(people), id!(list)], !v.people.is_empty() || v.people_editable || v.people_collapsible || v.people_add_visible);
        text(self, cx, &[id!(sheet_scroll), id!(people), id!(people_note)], if folded { "" } else { &v.people_note });
        // The per-request limit (or the monthly maximum), chosen with a slider and a value beside it.
        show(self, cx, &[id!(sheet_scroll), id!(quota)], v.choosing && v.quota_visible);
        self.view
            .label(cx, &at(&[id!(sheet_scroll), id!(quota), id!(quota_label)]))
            .set_text(cx, if v.quota_label.is_empty() { tr("Per-request limit") } else { &v.quota_label });
        text(self, cx, &[id!(sheet_scroll), id!(quota), id!(quota_hint)], &v.quota_hint);
        // Recovery steps for a saved record.
        let recover = v.recover_row();
        show(self, cx, &[id!(sheet_scroll), id!(actions)], recover || v.cancel_intent_visible);
        show(self, cx, &[id!(sheet_scroll), id!(actions), id!(recover)], recover);
        self.view.button(cx, &at(&[id!(sheet_scroll), id!(actions), id!(recover)])).set_text(cx, &v.recover);
        show(self, cx, &[id!(sheet_scroll), id!(actions), id!(actions_line)], recover && v.cancel_intent_visible);
        show(self, cx, &[id!(sheet_scroll), id!(actions), id!(cancel_intent)], v.cancel_intent_visible);
        // When sharing ends: its own row, which opens the period choice in place.
        show(self, cx, &[id!(sheet_scroll), id!(sharing)], v.sharing_visible);
        self.view.label(cx, &at(&[id!(sheet_scroll), id!(sharing), id!(sharing_row), id!(sharing_body), id!(sharing_label)])).set_text(cx, tr("Shared until"));
        // With large text, or a time too long to sit beside the row's name on a
        // narrow window, the time goes under the name, where it can wrap.
        let large = text_scale(cx) > 1.2;
        let below = large || v.expiry.chars().count() > 30;
        text(self, cx, &[id!(sheet_scroll), id!(sharing), id!(sharing_row), id!(sharing_body), id!(sharing_value)], if below { "" } else { &v.expiry });
        text(self, cx, &[id!(sheet_scroll), id!(sharing), id!(sharing_value_below)], if below { &v.expiry } else { "" });
        show(self, cx, &[id!(sheet_scroll), id!(sharing), id!(sharing_row), id!(sharing_body), id!(sharing_closed)], !v.sharing_open);
        show(self, cx, &[id!(sheet_scroll), id!(sharing), id!(sharing_row), id!(sharing_body), id!(sharing_opened)], v.sharing_open);
        show(self, cx, &[id!(sheet_scroll), id!(sharing), id!(period)], v.sharing_visible && v.sharing_open);
        text(self, cx, &[id!(sheet_scroll), id!(sharing), id!(period), id!(expiry_note)], &v.expiry_note);
        // Technical details, behind a row of their own.
        show(self, cx, &[id!(sheet_scroll), id!(more)], v.more_visible());
        self.view.label(cx, &at(&[id!(sheet_scroll), id!(more), id!(more_row), id!(more_body), id!(more_label)])).set_text(cx, &v.more_label);
        show(self, cx, &[id!(sheet_scroll), id!(more), id!(details)], v.more_open && !v.technical.is_empty());
        self.view.label(cx, &at(&[id!(sheet_scroll), id!(more), id!(details), id!(copy)])).set_text(cx, &v.technical);
        // A short sheet keeps its buttons right under its last card; a long one keeps them at the foot.
        // With large text even a short sheet can outgrow the window: it scrolls, so its buttons stay reachable.
        let short = !large && !v.choosing && !v.people_visible && !v.more_open && !v.sharing_visible && v.facts.len() <= 3;
        let scroll = self.view.view(cx, &at(&[id!(sheet_scroll)]));
        let height = if short { Size::fit() } else { Size::fill() };
        if let Some(mut walk) = scroll.borrow().map(|view| view.walk.clone()).filter(|walk| walk.height != height) {
            walk.height = height;
            scroll.set_walk(cx, walk);
        }
        // The footer: one secondary and one primary button.
        self.view.button(cx, &at(&[id!(buttons), id!(cancel)])).set_text(cx, &v.cancel);
        self.view.widget(cx, &at(&[id!(buttons), id!(cancel)])).set_visible(cx, !v.cancel.is_empty());
        self.view.button(cx, &at(&[id!(buttons), id!(primary)])).set_text(cx, &v.primary);
        self.view.widget(cx, &at(&[id!(buttons), id!(primary)])).set_visible(cx, v.primary_visible);
        self.view.button(cx, &at(&[id!(buttons), id!(primary)])).set_enabled(cx, v.primary_enabled);
        self.view.button(cx, &at(&[id!(buttons), id!(primary)])).set_disabled(cx, !v.primary_enabled);
    }
    /// The chevron and label of the single disclosure row.
    pub(super) fn apply_sheet_disclosure(&mut self, cx: &mut Cx, root: LiveId, open: bool) {
        let at = |rest: &[LiveId]| sheet_path(root, rest);
        self.view
            .widget(cx, &at(&[id!(sheet_scroll), id!(more), id!(more_row), id!(more_body), id!(more_closed)]))
            .set_visible(cx, !open);
        self.view
            .widget(cx, &at(&[id!(sheet_scroll), id!(more), id!(more_row), id!(more_body), id!(more_opened)]))
            .set_visible(cx, open);
    }
}
