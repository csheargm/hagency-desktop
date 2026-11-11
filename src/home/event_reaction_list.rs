use std::{hash::{Hash, Hasher}, collections::hash_map::DefaultHasher};
use crate::shared::inline_image::{InlineImageRef, InlineImageWidgetRefExt};
use crate::shared::{emoji_catalog::get_emoji_catalog, recent_emojis::record_emoji_pick};
use crate::theme::Snapshot as ThemeSnapshot;
use crate::home::room_screen::RoomScreenTooltipActions;
use crate::profile::user_profile_cache;
use crate::sliding_sync::{current_user_id, submit_async_request, MatrixRequest, TimelineKind};
use indexmap::IndexMap;
use makepad_widgets::*;
use crate::{LivePtr, widget_ref_from_live_ptr};
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use matrix_sdk_ui::timeline::{ReactionInfo, ReactionsByKeyBySender, TimelineEventItemId};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*


    mod.widgets.COLOR_BUTTON_GREY = mod.widgets.RINX_FIELD
    mod.widgets.REACTION_LIST_PADDING_RIGHT = 30.0;

    mod.widgets.ReactionList = #(ReactionList::register_widget(vm)) {
        width: Fill,
        height: Fit,
        flow: Flow.Right{wrap: true},
        margin: Inset{top: 5.0}
        padding: Inset{
            right: (mod.widgets.REACTION_LIST_PADDING_RIGHT)
        }
        item: SolidView {
            width: Fit, height: Fit, flow: Right, spacing: 4,
            align: Align{y: 0.5}, padding: 6,
            margin: Inset{top: 3, bottom: 3, left: 0, right: 6}
            draw_bg +: {
                reaction_bg_color: instance(mod.widgets.RINX_FIELD)
                reaction_border_color: instance(mod.widgets.RINX_BORDER)
                color_hover: instance(mod.widgets.RINX_HOVER)
                hover: instance(0.0)
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(1.5, 1.5, self.rect_size.x - 3.0, self.rect_size.y - 3.0, 3.0)
                    sdf.fill_keep(self.reaction_bg_color.mix(self.color_hover, self.hover * 0.2))
                    sdf.stroke(self.reaction_border_color, 1.5)
                    return sdf.result
                }
            }
            custom_emoji := mod.widgets.InlineImage {visible: false, width: 18, height: 18}
            reaction := Label {width: Fit, height: Fit, draw_text +: {color: mod.widgets.RINX_INK, text_style: REGULAR_TEXT {font_size: 12}}}
            count := Label {width: Fit, height: Fit, draw_text +: {color: mod.widgets.RINX_INK, text_style: REGULAR_TEXT {font_size: 10}}}
        }
    }

}

#[derive(Clone, Debug)]
pub struct ReactionData {
    /// Original reaction string from the backend before emoji shortcode conversion.
    pub reaction: String,
    /// Boolean indicating if the current user is also a sender of this reaction.
    pub includes_user: bool,
    /// List of all users who have reacted to the emoji.
    pub reaction_senders: IndexMap<OwnedUserId, ReactionInfo>,
    /// The ID of the room that the reaction is for
    pub room_id: OwnedRoomId,
}

#[derive(Script, Widget)]
pub struct ReactionList {
    #[source] source: ScriptObjectRef,
    #[rust]
    appearance: ThemeSnapshot,
    #[uid]
    uid: WidgetUid,
    #[redraw]
    #[rust]
    area: Area,
    #[live]
    item: Option<LivePtr>,
    #[rust]
    children: Vec<(ViewRef, ReactionData)>,
    #[rust] custom_emoji_images: Vec<InlineImageRef>,
    #[layout]
    layout: Layout,
    #[walk]
    walk: Walk,

    #[rust] timeline_kind: Option<TimelineKind>,
    #[rust] timeline_event_id: Option<TimelineEventItemId>,

    /// Includes keys and senders: equal counts can still describe different reactions.
    #[rust] last_reaction_fingerprint: Option<u64>,
}
impl ScriptHook for ReactionList {
    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        apply: &Apply,
        scope: &mut Scope,
        _: ScriptValue,
    ) {
        self.appearance = crate::theme::snapshot_for_vm(vm);
        if apply.is_script_reapply() {
            if let Some(template) = self.item {
                for (button, data) in &mut self.children {
                    button.script_apply(vm, apply, scope, template);
                    populate_reaction_button(vm.cx_mut(), button, data);
                    let (bg, border) = if data.includes_user {
                        (self.appearance.selected, self.appearance.accent)
                    } else {
                        (self.appearance.field, self.appearance.border)
                    };
                    set_reaction_colors(vm.cx_mut(), button, bg, border);
                }
            }
        }
    }
}
impl Widget for ReactionList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, self.layout);
        for (button, _) in self.children.iter_mut() {
            let _ = button.draw(cx, scope);
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        for image in &self.custom_emoji_images { image.handle_event(cx, event, scope); }
        for (button_ref, reaction_data) in self.children.iter() {
            let button_area = button_ref.area();
            // Note: the `break` statements are used to break out of the loop over
            // all reaction buttons, since a hit event can only occur on one button.
            match event.hits(cx, button_area) {
                Hit::FingerDown(..) => {
                    cx.set_key_focus(button_area);
                    break;
                }
                Hit::FingerHoverIn(..) => {
                    self.do_hover_in(cx, scope, button_ref, reaction_data.clone());
                    break;
                }
                Hit::FingerHoverOut(_) => {
                    self.do_hover_out(cx, scope, button_ref);
                    break;
                }
                // A long press is treated as a hover-in.
                Hit::FingerLongPress(_) => {
                    self.do_hover_in(cx, scope, button_ref, reaction_data.clone());
                    break;
                }
                Hit::FingerUp(fue) => {
                    // If the finger is not over the button, treat it as a hover-out.
                    if !fue.is_over {
                        self.do_hover_out(cx, scope, button_ref);
                    }
                    // Otherwise, a primary click/press over the button should toggle the reaction.
                    else if fue.is_primary_hit() && fue.was_tap() {
                        let Some(kind) = &self.timeline_kind else { return };
                        let Some(timeline_event_id) = &self.timeline_event_id else {
                            return;
                        };
                        if !reaction_data.includes_user {record_emoji_pick(cx, &reaction_data.reaction);}
                        submit_async_request(MatrixRequest::ToggleReaction {
                            timeline_kind: kind.clone(),
                            timeline_event_id: timeline_event_id.clone(),
                            reaction: reaction_data.reaction.clone(),
                        });
                        // update the reaction button before the timeline is updated
                        let (bg_color, border_color) = if !reaction_data.includes_user {
                            (self.appearance.selected, self.appearance.accent)
                        } else {
                            (self.appearance.field, self.appearance.border)
                        };
                        set_reaction_colors(cx, button_ref, bg_color, border_color);
                        self.do_hover_out(cx, scope, button_ref);
                    }
                    break;
                }
                Hit::FingerScroll(_) => {
                    self.do_hover_out(cx, scope, button_ref);
                    break;
                }
                _ => {}
            }
        }
    }
}

impl ReactionList {
    /// Deals with to any event/hit that triggers a hover-in action.
    fn do_hover_in(
        &self,
        cx: &mut Cx,
        _scope: &mut Scope,
        button_ref: &ViewRef,
        reaction_data: ReactionData,
    ) {
        cx.widget_action(
            self.widget_uid(), 
            RoomScreenTooltipActions::HoverInReactionButton {
                widget_rect: button_ref.area().rect(cx),
                reaction_data,
            },
        );
        if let Some(mut view) = button_ref.borrow_mut() { view.draw_bg.set_dyn_instance(cx, id!(hover), &[1.0]); view.redraw(cx); }
        cx.set_cursor(MouseCursor::Hand);
    }

    /// Deals with to any event/hit that triggers a hover-out action.
    fn do_hover_out(
        &self,
        cx: &mut Cx,
        _scope: &mut Scope,
        button_ref: &ViewRef,
    ) {
        cx.widget_action(self.widget_uid(),  RoomScreenTooltipActions::HoverOut);
        if let Some(mut view) = button_ref.borrow_mut() { view.draw_bg.set_dyn_instance(cx, id!(hover), &[0.0]); view.redraw(cx); }
        cx.set_cursor(MouseCursor::Default);
    }
}


impl ReactionListRef {
    pub fn get_own_reactions(&self) -> Vec<String> {
        self.borrow().map(|inner| inner.children.iter()
            .filter(|(_, data)| data.includes_user).map(|(_, data)| data.reaction.clone()).collect()
        ).unwrap_or_default()
    }

    /// Set the list of reactions and their counts to display in the ReactionList widget,
    /// along with the room ID and event ID that these reactions are for.
    ///
    /// This will clear any existing list of reactions and replace it with the given one.
    ///
    /// The given `event_tl_item_reactions` is a map from each reaction's raw string (including any variant selectors)
    /// to the list of users who have reacted with that reaction.
    ///
    /// The given `room_id` is the ID of the room that these reactions are for.
    ///
    /// The given `timeline_event_item_id` is the ID of the event that these reactions are for.
    /// Required by Matrix API
    pub fn set_list(
        &mut self,
        cx: &mut Cx,
        event_tl_item_reactions: Option<&ReactionsByKeyBySender>,
        timeline_kind: &TimelineKind,
        timeline_event_item_id: &TimelineEventItemId,
        _id: usize,
    ) {
        let Some(mut inner) = self.borrow_mut() else { return };
        let Some(curr_user_id) = current_user_id() else { return };
        let Some(event_tl_item_reactions) = event_tl_item_reactions else {
            inner.children.clear();
            inner.custom_emoji_images.clear();
            inner.last_reaction_fingerprint = None;
            return;
        };

        let fingerprint = reaction_fingerprint(event_tl_item_reactions, &curr_user_id);
        if inner.last_reaction_fingerprint == Some(fingerprint)
            && inner.timeline_event_id.as_ref() == Some(timeline_event_item_id)
            && inner.timeline_kind.as_ref() == Some(timeline_kind) { return; }
        inner.last_reaction_fingerprint = Some(fingerprint);
        inner.children.clear();
        inner.custom_emoji_images.clear();
        for (reaction_text, reaction_senders) in event_tl_item_reactions.iter() {
            let mut includes_user: bool = false;
            for (sender, _) in reaction_senders.iter() {
                if sender == &curr_user_id {
                    includes_user = true;
                }
                // Pre-fetch the profile of each reaction sender into the cache for future use in the tooltip.
                // TODO: we shouldn't do this, it's wasteful. Instead, fetch it on demand
                //       when showing the tooltip; we just need to add a way to update a live tooltip.
                //       And now that we have room member lists, we can just use that (especially once
                //       we convert them to a hashmap in the async backend, in the future)
                let _ = user_profile_cache::with_user_profile(
                    cx,
                    sender.clone(),
                    Some(timeline_kind.room_id()),
                    true, |_, _| { },
                );
            }

            let reaction_data = ReactionData {
                reaction: reaction_text.to_string(),
                includes_user,
                reaction_senders: reaction_senders.clone(),
                room_id: timeline_kind.room_id().clone(),
            };
            let button = widget_ref_from_live_ptr(cx, inner.item).as_view();
            populate_reaction_button(cx, &button, &reaction_data);
            if is_custom_emoji(reaction_text) {
                inner.custom_emoji_images.push(button.inline_image(cx, ids!(custom_emoji)));
            }
            let (bg_color, border_color) = if reaction_data.includes_user {
                (inner.appearance.selected, inner.appearance.accent)
            } else {
                (inner.appearance.field, inner.appearance.border)
            };
            set_reaction_colors(cx, &button, bg_color, border_color);
            inner.children.push((button, reaction_data));
        }
        inner.timeline_kind = Some(timeline_kind.clone());
        inner.timeline_event_id = Some(timeline_event_item_id.clone());
    }

    /// Returns `true` if the `abs` coord is within one of the reaction buttons.
    pub fn contains_button(&self, cx: &Cx, abs: DVec2) -> bool {
        let Some(inner) = self.borrow() else { return false };
        inner.children.iter().any(|(button, _)|
            button.area().clipped_rect(cx).contains(abs)
        )
    }

    /// Returns any `RoomScreenTooltipActions` that occurred in the given list of `actions`.
    ///
    /// This function checks if there is a widget action associated with the current
    /// widget's unique identifier in the provided `actions`.
    /// If an action exists, it is cast to `RoomScreenTooltipActions` and returned.
    /// Otherwise, it returns `RoomScreenTooltipActions::None`.
    pub fn hovered_in(&self, actions: &Actions) -> RoomScreenTooltipActions {
        if let Some(item) = actions.find_widget_action(self.widget_uid()) {
            item.cast()
        } else {
            RoomScreenTooltipActions::None
        }
    }
    /// Returns whether the given `actions` contained a `RoomScreenTooltipActions::HoverOut` action.
    pub fn hovered_out(&self, actions: &Actions) -> bool {
        if let Some(item) = actions.find_widget_action(self.widget_uid()) {
            matches!(item.cast(), RoomScreenTooltipActions::HoverOut)
        } else {
            false
        }
    }
}

#[cfg(test)]
mod theme_tests {
    use super::*;
    use crate::theme::{self, Accent, Appearance, Selection};

    #[test]
    fn retained_reaction_reapplies_without_reentering_the_vm() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut reactions = cx.with_vm(|vm| {
            theme::tests::install(vm, Selection::default());
            crate::shared::styles::script_mod(vm);
            crate::shared::inline_image::script_mod(vm);
            self::super::script_mod(vm);
            let value = script_eval!(vm, {mod.widgets.ReactionList {}});
            ReactionList::script_from_value(vm, value)
        });
        let button = widget_ref_from_live_ptr(&mut cx, reactions.item).as_view();
        button.label(&mut cx, ids!(reaction)).set_text(&mut cx, "👍");
        let uid = button.widget_uid();
        reactions.children.push((button, ReactionData {
            reaction: "👍".into(), includes_user: true,
            reaction_senders: IndexMap::new(),
            room_id: matrix_sdk::ruma::room_id!("!theme:example.org").to_owned(),
        }));
        let old_revision = reactions.appearance.revision;
        cx.with_vm(|vm| vm.with_reload(|vm| {
            theme::tests::install(vm, Selection {appearance: Appearance::Dark, accent: Accent::Violet});
            crate::shared::styles::script_mod(vm);
            crate::shared::inline_image::script_mod(vm);
            self::super::script_mod(vm);
            let value = script_eval!(vm, {mod.widgets.ReactionList {}});
            reactions.script_apply(vm, &Apply::ScriptReapply, &mut Scope::empty(), value);
            assert!(vm.take_errors().is_empty());
        }));
        assert_eq!(reactions.children[0].0.widget_uid(), uid);
        assert_eq!(reactions.children[0].0.label(&mut cx, ids!(reaction)).text(), "👍");
        assert!(reactions.children[0].1.includes_user);
        assert_ne!(reactions.appearance.revision, old_revision);
    }
}

/// Returns the reaction that the user typed, or `None` if it's blank.
/// An emoji or `:shortcode:` becomes its fully-qualified emoji, so it matches everyone else's reactions.
pub fn get_typed_reaction(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let emoji = emojis::get(text).or_else(|| {
        let shortcode = text.strip_prefix(':')?.strip_suffix(':')?;
        get_emoji_catalog().get_by_shortcode(&shortcode.to_lowercase())
    });
    Some(emoji.map_or(text, |emoji| emoji.as_str()).to_owned())
}

/// Toggles `reaction` on a message: if the user already sent it or an emoji that looks the same (like ❤ for ❤️),
/// we remove those, or else we add it, which counts as a recent pick for an emoji.
pub fn toggle_reaction(
    cx: &mut Cx,
    timeline_kind: &TimelineKind,
    timeline_event_id: &TimelineEventItemId,
    reactions: Option<&ReactionsByKeyBySender>,
    reaction: &str,
) {
    let emoji = emojis::get(reaction);
    let own_equivalents: Vec<String> = emoji
        .zip(reactions)
        .zip(current_user_id())
        .map(|((emoji, reactions), user_id)| reactions.iter()
            .filter(|(key, senders)| senders.contains_key(&user_id) && emojis::get(key) == Some(emoji))
            .map(|(key, _)| key.clone())
            .collect()
        )
        .unwrap_or_default();
    let toggled = if own_equivalents.is_empty() {
        if emoji.is_some() {
            record_emoji_pick(cx, reaction);
        }
        vec![reaction.to_owned()]
    } else {
        own_equivalents
    };
    for reaction in toggled {
        submit_async_request(MatrixRequest::ToggleReaction {
            timeline_kind: timeline_kind.clone(),
            timeline_event_id: timeline_event_id.clone(),
            reaction,
        });
    }
}

/// Matrix custom-emoji reaction keys are MXC URIs, never remote web URLs.
pub fn is_custom_emoji(reaction: &str) -> bool {
    <&ruma::MxcUri>::from(reaction).is_valid()
}

fn populate_reaction_button(cx: &mut Cx, button: &ViewRef, data: &ReactionData) {
    let custom = is_custom_emoji(&data.reaction);
    let image = button.inline_image(cx, ids!(custom_emoji));
    image.set_visible(cx, custom);
    if custom { image.set_mxc_uri(cx, data.reaction.as_str().into()); }
    let label = button.label(cx, ids!(reaction));
    label.set_visible(cx, !custom);
    label.set_text(cx, &data.reaction);
    button.label(cx, ids!(count)).set_text(cx, &data.reaction_senders.len().to_string());
}

fn set_reaction_colors(cx: &mut Cx, view: &ViewRef, background: Vec4f, border: Vec4f) {
    if let Some(mut view) = view.borrow_mut() {
        view.draw_bg.set_dyn_instance(cx, id!(reaction_bg_color), &[background.x, background.y, background.z, background.w]);
        view.draw_bg.set_dyn_instance(cx, id!(reaction_border_color), &[border.x, border.y, border.z, border.w]);
        view.redraw(cx);
    }
}

fn reaction_fingerprint(reactions: &ReactionsByKeyBySender, own_user: &ruma::UserId) -> u64 {
    let mut hash = DefaultHasher::new();
    own_user.hash(&mut hash);
    for (key, senders) in reactions.iter() {
        key.hash(&mut hash);
        for (sender, info) in senders {sender.hash(&mut hash); info.timestamp.hash(&mut hash);}
    }
    hash.finish()
}

#[cfg(test)]
mod reaction_tests {
    use super::*;
    use ruma::{user_id, MilliSecondsSinceUnixEpoch, uint};
    #[test]
    fn same_count_key_and_sender_replacements_invalidate_reactions() {
        let mut reactions = ReactionsByKeyBySender::default();
        let own = user_id!("@alice:test");
        let info = ReactionInfo {timestamp: MilliSecondsSinceUnixEpoch(uint!(10)), send_state: None};
        reactions.insert("👍".into(), IndexMap::from([(own.to_owned(), info.clone())]));
        let old = reaction_fingerprint(&reactions, own);
        let senders = reactions.shift_remove("👍").unwrap();
        reactions.insert("mxc://test/emoji".into(), senders);
        let custom = reaction_fingerprint(&reactions, own);
        assert_ne!(old, custom);
        reactions.insert("mxc://test/emoji".into(), IndexMap::from([(user_id!("@bob:test").to_owned(), info)]));
        assert_ne!(custom, reaction_fingerprint(&reactions, own));
    }
    #[test]
    fn typed_reactions_canonicalize_without_turning_web_links_into_images() {
        assert_eq!(get_typed_reaction("  :thumbsup:  "), Some("👍".into()));
        assert_eq!(get_typed_reaction("❤"), Some("❤️".into()));
        assert_eq!(get_typed_reaction("  thanks  "), Some("thanks".into()));
        assert_eq!(get_typed_reaction("  "), None);
        assert!(is_custom_emoji("mxc://example.org/emoji"));
        for invalid in ["https://example.org/emoji.png", "mxc://", "data:image/png;base64,"] {assert!(!is_custom_emoji(invalid));}
    }
}
