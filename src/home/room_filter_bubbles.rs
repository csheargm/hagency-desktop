//! Include/exclude room filters from Robrix 8fed6f1d, 4a349ea6 and da2674db.
//! Applied on top of Hagency's categories, space scope and hidden-room rules.
use bitflags::bitflags;
use makepad_widgets::*;
use ruma::events::tag::TagName;
use crate::{room::room_display_filter::FilterableRoom, sliding_sync::UserPowerLevels};

bitflags! {
    /// A set of the filters that the bubbles can apply to the rooms list.
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
    pub struct BubbleFilters: u8 {
        const Unreads     = 0b0000_0001;
        const Mentions    = 0b0000_0010;
        const People      = 0b0000_0100;
        const Rooms       = 0b0000_1000;
        const Favorites   = 0b0001_0000;
        const Invites     = 0b0010_0000;
        const LowPriority = 0b0100_0000;
        const ModAdmin    = 0b1000_0000;
    }
}

impl BubbleFilters {
    /// Returns whether the given room matches all of these filters.
    fn matches(self, room: &dyn FilterableRoom, show_invites_in_unread_filters: bool) -> bool {
        self.iter().all(|filter| match filter {
            Self::Unreads | Self::Mentions if room.is_invite() => show_invites_in_unread_filters,
            Self::Unreads => room.has_unreads(),
            Self::Mentions => room.unread_mentions() > 0,
            Self::People => room.is_direct(),
            Self::Rooms => !room.is_direct(),
            Self::ModAdmin => {
                !room.is_direct()
                    && room
                        .user_power_levels()
                        .is_some_and(UserPowerLevels::is_mod_or_admin)
            }
            Self::Favorites => room.tags().contains_key(&TagName::Favorite),
            Self::Invites => room.is_invite(),
            Self::LowPriority => room.tags().contains_key(&TagName::LowPriority),
            _ => true,
        })
    }
}

/// The filters of the active bubbles.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BubbleSelection {
    included: BubbleFilters,
    excluded: BubbleFilters,
}

impl BubbleSelection {
    pub fn is_empty(self) -> bool {
        self.active().is_empty()
    }

    /// Returns the filters that are either included or excluded.
    pub fn active(self) -> BubbleFilters {
        self.included | self.excluded
    }

    pub fn matches(self, room: &dyn FilterableRoom, show_invites_in_unread_filters: bool) -> bool {
        self.included.matches(room, show_invites_in_unread_filters)
            && !self
                .excluded
                .iter()
                .any(|filter| filter.matches(room, show_invites_in_unread_filters))
    }

    /// Excludes the given filter, or deactivates it if it's already excluded.
    pub(crate) fn toggle_excluded(&mut self, filter: BubbleFilters) {
        self.included.remove(filter);
        self.excluded.toggle(filter);
    }

    /// Returns whether no room could ever match these filters, like "People" with "Rooms",
    /// as opposed to none of our rooms matching them right now.
    fn is_contradictory(self, show_invites_in_unread_filters: bool) -> bool {
        use BubbleFilters as F;
        // Each kind of room we could show, as the filters it matches. Mentions are always unread,
        // and the SDK never lets a room be both a favorite and low priority.
        let joined_rooms = [F::People, F::Rooms, F::Rooms | F::ModAdmin]
            .into_iter()
            .flat_map(|kind| {
                [F::empty(), F::Unreads, F::Unreads | F::Mentions].map(|unreads| kind | unreads)
            })
            .flat_map(|room| [F::empty(), F::Favorites, F::LowPriority].map(|tag| room | tag));
        // Invites can be to a DM or a room (spaces count as rooms), and they don't have tags or power levels.
        let invite_unreads = if show_invites_in_unread_filters {
            F::Unreads | F::Mentions
        } else {
            F::empty()
        };
        let invites = [F::People, F::Rooms].map(|kind| kind | F::Invites | invite_unreads);
        !joined_rooms
            .chain(invites)
            .any(|room| room.contains(self.included) && !room.intersects(self.excluded))
    }

    /// Returns whether including or excluding the given filter alongside the others would always show
    /// either no rooms or the same rooms, so its bubble isn't worth showing.
    fn is_pointless(self, filter: BubbleFilters, show_invites_in_unread_filters: bool) -> bool {
        let others = BubbleSelection {
            included: self.included - filter,
            excluded: self.excluded - filter,
        };
        BubbleSelection {
            included: others.included | filter,
            ..others
        }
        .is_contradictory(show_invites_in_unread_filters)
            || BubbleSelection {
                excluded: others.excluded | filter,
                ..others
            }
            .is_contradictory(show_invites_in_unread_filters)
    }
}

/// The ID of each bubble's slot, the bubble's filter, and its text.
const BUBBLES: [(LiveId, BubbleFilters, &str); 8] = [
    (id!(unreads), BubbleFilters::Unreads, "Unreads"),
    (id!(mentions), BubbleFilters::Mentions, "Mentions"),
    (id!(people), BubbleFilters::People, "People"),
    (id!(rooms), BubbleFilters::Rooms, "Rooms"),
    (id!(mod_admin), BubbleFilters::ModAdmin, "Mod/Admin"),
    (id!(favorites), BubbleFilters::Favorites, "Favorites"),
    (id!(invites), BubbleFilters::Invites, "Invites"),
    (
        id!(low_priority),
        BubbleFilters::LowPriority,
        "Low Priority",
    ),
];

impl BubbleSelection {
    fn toggle(&mut self, filter: BubbleFilters, exclude: bool, count_invites: bool) {
        let previous = *self;
        if exclude {
            self.toggle_excluded(filter);
        } else if self.excluded.contains(filter) {
            self.excluded.remove(filter);
        } else {
            self.included.toggle(filter);
        }
        // Preserve independent filters, giving the newly selected one precedence
        // over mutually exclusive/redundant older ones.
        let requested = *self;
        let mut kept = Self {
            included: requested.included & filter,
            excluded: requested.excluded & filter,
        };
        for old in previous.active().iter().filter(|old| *old != filter) {
            let candidate = Self {
                included: kept.included | (requested.included & old),
                excluded: kept.excluded | (requested.excluded & old),
            };
            if !candidate.is_contradictory(count_invites)
                && !candidate.is_pointless(old, count_invites)
            {
                kept = candidate;
            }
        }
        *self = kept;
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RoomFilterBubblesAction {
    Changed(BubbleSelection),
    InvitesInUnread(bool),
}

#[derive(Default)]
struct FilterGlobal {
    selection: BubbleSelection,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let FilterButton = RobrixNeutralIconButton {
        width: Fit, height: 28, margin: 0, padding: Inset{left: 10, right: 10}, spacing: 0
        enable_long_press: true
        draw_bg +: {border_radius: 14}
        draw_text +: {text_style: REGULAR_TEXT {font_size: 10}}
    }
    mod.widgets.RoomFilterBubbles = #(RoomFilterBubbles::register_widget(vm)) {
        width: Fill, height: 32
        bubbles := ScrollXView {
            width: Fill, height: Fill, flow: Right, spacing: 4
            clear := FilterButton {text: "×"}
            unreads := FilterButton {} mentions := FilterButton {}
            people := FilterButton {} rooms := FilterButton {} mod_admin := FilterButton {}
            favorites := FilterButton {} invites := FilterButton {} low_priority := FilterButton {}
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RoomFilterBubbles {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for RoomFilterBubbles {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::Actions(actions) = event {
            for action in actions {
                if let Some(RoomFilterBubblesAction::Changed(selection)) = action.downcast_ref() {
                    cx.global::<FilterGlobal>().selection = *selection;
                    self.redraw(cx);
                }
                if action
                    .downcast_ref::<crate::logout::logout_confirm_modal::LogoutAction>()
                    .is_some()
                {
                    cx.global::<FilterGlobal>().selection = BubbleSelection::default();
                    self.redraw(cx);
                }
            }
        }
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        let secondary = match event {
            Event::MouseDown(e)
                if (e.button.is_secondary() || e.button.is_middle())
                    && !e.handled.get().is_empty() =>
            {
                Some(e.handled.get())
            }
            _ => None,
        };
        let mut selection = cx.global::<FilterGlobal>().selection;
        let previous = selection;
        let count_invites = cx
            .global::<crate::settings::app_preferences::AppPreferencesGlobal>()
            .0
            .show_invites_in_unread_filters;
        if self.button(cx, ids!(clear)).clicked(&actions) {
            selection = BubbleSelection::default();
        }
        for (id, filter, _) in BUBBLES {
            let button = self.view.button(cx, &[id]);
            let excluded = button.long_pressed(&actions) || secondary == Some(button.area());
            if excluded || button.clicked(&actions) {
                selection.toggle(filter, excluded, count_invites);
            }
        }
        if selection != previous {
            cx.global::<FilterGlobal>().selection = selection;
            cx.action(RoomFilterBubblesAction::Changed(selection));
            self.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let selection = cx.global::<FilterGlobal>().selection;
        let count_invites = cx
            .global::<crate::settings::app_preferences::AppPreferencesGlobal>()
            .0
            .show_invites_in_unread_filters;
        self.button(cx, ids!(clear))
            .set_visible(cx, !selection.is_empty());
        for (id, filter, label) in BUBBLES {
            let button = self.view.button(cx, &[id]);
            button.set_visible(
                cx,
                selection.active().contains(filter)
                    || !selection.is_pointless(filter, count_invites),
            );
            let label = crate::i18n::tr(label);
            let text = if selection.excluded.contains(filter) {
                crate::i18n::format("Not {filter}", &[("filter", label.to_owned())])
            } else if selection.included.contains(filter) {
                format!("✓ {label}")
            } else {
                label.to_owned()
            };
            button.set_text(cx, &text);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contradictory_categories_replace_each_other_without_losing_independent_filters() {
        let mut selected = BubbleSelection::default();
        selected.toggle(BubbleFilters::Favorites, false, true);
        selected.toggle(BubbleFilters::People, false, true);
        selected.toggle(BubbleFilters::Rooms, false, true);
        assert_eq!(
            selected.included,
            BubbleFilters::Favorites | BubbleFilters::Rooms
        );
        selected.toggle(BubbleFilters::Favorites, true, true);
        assert_eq!(selected.included, BubbleFilters::Rooms);
        assert_eq!(selected.excluded, BubbleFilters::Favorites);
        selected.toggle(BubbleFilters::Favorites, true, true);
        assert!(selected.excluded.is_empty());
    }

    #[test]
    fn mentions_replace_redundant_unreads_and_invite_policy_changes_combinations() {
        let mut selected = BubbleSelection::default();
        selected.toggle(BubbleFilters::Unreads, false, true);
        selected.toggle(BubbleFilters::Mentions, false, true);
        assert_eq!(selected.included, BubbleFilters::Mentions);
        let invites = BubbleSelection {
            included: BubbleFilters::Invites | BubbleFilters::Unreads,
            excluded: BubbleFilters::empty(),
        };
        assert!(!invites.is_contradictory(true));
        assert!(invites.is_contradictory(false));
    }
    struct Room {
        direct: bool,
        invite: bool,
        unread: bool,
        mentions: u64,
        tags: ruma::events::tag::Tags,
        powers: Option<UserPowerLevels>,
    }
    impl FilterableRoom for Room {
        fn room_id(&self) -> &ruma::RoomId {
            ruma::room_id!("!test:example.org")
        }
        fn room_name(&self) -> std::borrow::Cow<'_, str> {
            "Team".into()
        }
        fn unread_mentions(&self) -> u64 {
            self.mentions
        }
        fn unread_messages(&self) -> u64 {
            0
        }
        fn canonical_alias(&self) -> Option<std::borrow::Cow<'_, ruma::RoomAliasId>> {
            None
        }
        fn alt_aliases(&self) -> std::borrow::Cow<'_, [ruma::OwnedRoomAliasId]> {
            (&[][..]).into()
        }
        fn tags(&self) -> &ruma::events::tag::Tags {
            &self.tags
        }
        fn is_direct(&self) -> bool {
            self.direct
        }
        fn is_invite(&self) -> bool {
            self.invite
        }
        fn has_unreads(&self) -> bool {
            self.unread || self.mentions > 0
        }
        fn user_power_levels(&self) -> Option<UserPowerLevels> {
            self.powers
        }
    }
    #[test]
    fn room_filters_intersect_with_search_and_handle_invites_manual_unreads_and_unknown_powers() {
        let mut room = Room {
            direct: false,
            invite: false,
            unread: true,
            mentions: 0,
            tags: Default::default(),
            powers: None,
        };
        let selection = BubbleSelection {
            included: BubbleFilters::Unreads | BubbleFilters::Rooms,
            excluded: BubbleFilters::LowPriority,
        };
        assert!(selection.matches(&room, true));
        let (search, _) = crate::room::room_display_filter::RoomDisplayFilterBuilder::new()
            .set_keywords("missing".into())
            .build();
        assert!(!(search(&room) && selection.matches(&room, true)));
        room.unread = false;
        assert!(!selection.matches(&room, true));
        room.mentions = 1;
        assert!(selection.matches(&room, true));
        room.invite = true;
        assert!(selection.matches(&room, true));
        assert!(!selection.matches(&room, false));
        room.invite = false;
        let admin = BubbleSelection {
            included: BubbleFilters::ModAdmin,
            excluded: BubbleFilters::empty(),
        };
        assert!(!admin.matches(&room, true));
        room.powers = Some(UserPowerLevels::ModOrAdmin);
        assert!(admin.matches(&room, true));
        room.direct = true;
        assert!(!admin.matches(&room, true));
    }
}
