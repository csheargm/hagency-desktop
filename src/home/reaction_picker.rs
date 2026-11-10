//! Robrix's searchable emoji catalog in Hagency's bounded modal layout.
use std::collections::HashSet;
use emojis::{Emoji, SkinTone};
use makepad_widgets::*;
use crate::shared::{
    emoji_catalog::{get_emoji_catalog, with_skin_tone},
    recent_emojis::{get_frequently_used_emojis, get_pick_counts, get_skin_tone, set_skin_tone},
};
use super::{
    event_reaction_list::get_typed_reaction, new_message_context_menu::MessageDetails,
    room_screen::MessageAction,
};

type PickerEmoji = &'static Emoji;
type PickerCell = (ButtonRef, PickerEmoji);

const TONES: [SkinTone; 6] = [
    SkinTone::Default,
    SkinTone::Light,
    SkinTone::MediumLight,
    SkinTone::Medium,
    SkinTone::MediumDark,
    SkinTone::Dark,
];

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let EmojiCell = RobrixIconButton {
        width: Fill, height: 40, padding: 0, margin: 0, spacing: 0
        icon_walk: Walk{width: 0, height: 0}
        draw_text +: {text_style: theme.font_regular {font_size: 22}}
    }
    mod.widgets.ReactionPicker = set_type_default() do #(ReactionPicker::register_widget(vm)) {
        ..RoundedView
        width: Fill {max: 420}, height: Fill {max: 460}
        padding: 16, spacing: 10, flow: Down
        draw_bg +: {color: mod.widgets.RBX_BG_SURFACE, border_color: mod.widgets.RINX_BORDER, border_size: 1, border_radius: 8}
        View {
            width: Fill, height: Fit, flow: Right, spacing: 8
            Label {width: Fill, text: #(crate::i18n::tr("Add Reaction")) i18n_text: "Add Reaction"}
            close_button := RobrixNeutralIconButton {text: #(crate::i18n::tr("Close")) i18n_text: "Close"}
        }
        search := RobrixTextInput {width: Fill, height: Fit, empty_text: #(crate::i18n::tr("Search emoji or enter a text reaction")) i18n_empty_text: "Search emoji or enter a text reaction"}
        View {
            width: Fill, height: Fit, flow: Right, spacing: 8
            tone_button := RobrixNeutralIconButton {width: Fit, text: "👍"}
            text_button := RobrixNeutralIconButton {width: Fill, text: #(crate::i18n::tr("Use Text Reaction")) i18n_text: "Use Text Reaction"}
        }
        emoji_list := PortalList {
            width: Fill, height: Fill
            Row := View {
                width: Fill, height: 44, flow: Right, spacing: 4
                c0 := EmojiCell {} c1 := EmojiCell {} c2 := EmojiCell {}
                c3 := EmojiCell {} c4 := EmojiCell {} c5 := EmojiCell {}
            }
        }
        result_label := Label {width: Fill, height: Fit, text: ""}
    }
}

#[derive(Clone, Debug)]
pub enum ReactionPickerAction {
    Open {
        details: MessageDetails,
        own_reactions: Vec<String>,
    },
    Close,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ReactionPicker {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    details: Option<MessageDetails>,
    #[rust]
    epoch: u64,
    #[rust]
    results: Vec<PickerEmoji>,
    #[rust]
    own_reactions: HashSet<String>,
    #[rust]
    cells: Vec<PickerCell>,
    #[rust]
    focus_after_draw: bool,
}

impl ReactionPicker {
    fn populate(&mut self, cx: &mut Cx) {
        let query = self.text_input(cx, ids!(search)).text();
        let counts = get_pick_counts(cx);
        let catalog = get_emoji_catalog();
        let mut results = if query.trim().is_empty() {
            get_frequently_used_emojis(cx, 30)
                .into_iter()
                .chain(catalog.emojis.iter().map(|e| e.emoji))
                .collect::<Vec<_>>()
        } else {
            catalog
                .search(query.trim(), &counts)
                .into_iter()
                .map(|e| e.emoji)
                .collect()
        };
        let tone = get_skin_tone(cx);
        let mut seen = HashSet::new();
        results = results
            .into_iter()
            .map(|e| with_skin_tone(e, tone))
            .filter(|e| seen.insert(e.as_str()))
            .collect();
        self.results = results;
        self.cells.clear();
        self.portal_list(cx, ids!(emoji_list))
            .set_first_id_and_scroll(0, 0.0);
        self.button(cx, ids!(tone_button)).set_text(
            cx,
            &format!(
                "{} {}",
                crate::i18n::tr("Skin tone"),
                with_skin_tone(emojis::get("👍").unwrap(), tone).as_str()
            ),
        );
        self.button(cx, ids!(text_button))
            .set_enabled(cx, !query.trim().is_empty());
        self.label(cx, ids!(result_label)).set_text(
            cx,
            if self.results.is_empty() {
                crate::i18n::tr("No matching emoji")
            } else {
                crate::i18n::tr("Select an emoji to react")
            },
        );
        self.redraw(cx);
    }

    fn close(&mut self, cx: &mut Cx) {
        self.details = None;
        self.cells.clear();
        self.own_reactions.clear();
        cx.action(ReactionPickerAction::Close);
    }

    fn pick(&mut self, cx: &mut Cx, reaction: String) {
        if self.epoch == crate::account_session::epoch()
            && let Some(details) = self.details.take()
        {
            cx.widget_action(
                details.room_screen_widget_uid,
                MessageAction::React { details, reaction },
            );
        }
        self.close(cx);
    }
}

impl Widget for ReactionPicker {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::Actions(actions) = event
            && actions
                .iter()
                .any(|a| matches!(a.downcast_ref(), Some(ModalAction::Dismissed)))
        {
            self.details = None;
            self.cells.clear();
            return;
        }
        if self.details.is_none() {
            return;
        }
        if self.epoch != crate::account_session::epoch() {
            self.close(cx);
            return;
        }
        self.view.handle_event(cx, event, scope);
        if event.back_pressed()
            || matches!(
                event,
                Event::KeyDown(KeyEvent {
                    key_code: KeyCode::Escape,
                    ..
                })
            )
        {
            self.close(cx);
            return;
        }
        if let Event::Actions(actions) = event {
            if self.button(cx, ids!(close_button)).clicked(actions) {
                self.close(cx);
                return;
            }
            let search = self.text_input(cx, ids!(search));
            if search.changed(actions).is_some() {
                self.populate(cx);
            }
            if self.button(cx, ids!(tone_button)).clicked(actions) {
                let index = TONES
                    .iter()
                    .position(|t| *t == get_skin_tone(cx))
                    .unwrap_or(0);
                set_skin_tone(cx, TONES[(index + 1) % TONES.len()]);
                self.populate(cx);
            }
            if (self.button(cx, ids!(text_button)).clicked(actions)
                || search.returned(actions).is_some())
                && let Some(reaction) = get_typed_reaction(&search.text())
            {
                self.pick(cx, reaction);
                return;
            }
            if let Some(emoji) = self
                .cells
                .iter()
                .find_map(|(button, emoji)| button.clicked(actions).then_some(*emoji))
            {
                self.pick(cx, emoji.as_str().to_owned());
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.cells.clear();
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.borrow_mut::<PortalList>() {
                list.set_item_range(cx, 0, self.results.len().div_ceil(6));
                while let Some(row) = list.next_visible_item(cx) {
                    let item = list.item(cx, row, id!(Row));
                    for (column, id) in [ids!(c0), ids!(c1), ids!(c2), ids!(c3), ids!(c4), ids!(c5)]
                        .into_iter()
                        .enumerate()
                    {
                        let button = item.button(cx, id);
                        let emoji = self.results.get(row * 6 + column).copied();
                        button.set_visible(cx, emoji.is_some());
                        if let Some(emoji) = emoji {
                            let text = if self.own_reactions.contains(emoji.as_str()) {
                                format!("{} ✓", emoji.as_str())
                            } else {
                                emoji.as_str().to_owned()
                            };
                            button.set_text(cx, &text);
                            self.cells.push((button, emoji));
                        }
                    }
                    item.draw_all(cx, scope);
                }
            }
        }
        if self.focus_after_draw {
            self.text_input(cx, ids!(search)).set_key_focus(cx);
            self.focus_after_draw = false;
        }
        DrawStep::done()
    }
}

impl ReactionPickerRef {
    pub fn show(&self, cx: &mut Cx, details: MessageDetails, own_reactions: &[String]) {
        let Some(mut inner) = self.borrow_mut() else {
            return;
        };
        inner.details = Some(details);
        inner.epoch = crate::account_session::epoch();
        inner.own_reactions = own_reactions
            .iter()
            .map(|r| emojis::get(r).map_or(r.as_str(), |e| e.as_str()).to_owned())
            .collect();
        inner.text_input(cx, ids!(search)).set_text(cx, "");
        inner.focus_after_draw = true;
        inner.populate(cx);
    }
}
