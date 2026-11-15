//! The rail's "More" item (UX item A1): the rail shows only Chats and Agents,
//! and everything else — the spaces, Contacts, Moments, Mini apps and File
//! Transfer — sits one click deeper, in a group that "More" opens and closes
//! in place above it. Settings and the account menu stay at the bottom.

use makepad_widgets::*;
use crate::shared::navigation_bar_button::NavigationBarButtonWidgetExt;
use super::{NavigationBarAction, NavigationTabBar, SelectedTab};

/// The secondary items behind "More", top to bottom, with their tooltips.
pub(super) const MORE_ITEMS: [(&[LiveId; 1], &str); 4] = [
    (ids!(contacts_button), "Contacts"),
    (ids!(moments_button), "Moments"),
    (ids!(octoscript_apps_button), "Mini apps"),
    (ids!(file_transfer_button), "File Transfer"),
];

/// Rail children that only show while "More" is open, besides `MORE_ITEMS`.
const MORE_FRAME: [&[LiveId; 1]; 2] = [ids!(top_separator), ids!(more_spaces)];

impl NavigationTabBar {
    /// Shows or hides everything behind "More", and marks "More" while it is open.
    pub(super) fn set_more_open(&mut self, cx: &mut Cx, open: bool) {
        self.more_open = open;
        for id in MORE_FRAME.into_iter().chain(MORE_ITEMS.map(|(id, _)| id)) {
            self.view.widget(cx, id).set_visible(cx, open);
        }
        self.view.view(cx, ids!(rail_spacer)).set_visible(cx, !open);
        self.view.navigation_bar_button(cx, ids!(more_button)).set_selected(cx, open);
        self.view.redraw(cx);
    }

    /// Re-applies the open state after the Desktop variant is (re)created.
    pub(super) fn apply_more_open(&mut self, cx: &mut Cx) {
        let spaces = self.view.view(cx, ids!(more_spaces));
        if !spaces.is_empty() && spaces.visible() != self.more_open {
            self.set_more_open(cx, self.more_open);
        }
    }

    /// Handles "More" and the items behind it. Each item does exactly what its
    /// former rail button did.
    pub(super) fn handle_more_clicks(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.view.navigation_bar_button(cx, ids!(more_button)).clicked(actions) {
            self.set_more_open(cx, !self.more_open);
        }
        if self.view.navigation_bar_button(cx, ids!(contacts_button)).clicked(actions) {
            cx.action(NavigationBarAction::GoToTab(SelectedTab::Contacts));
        }
        if self.view.navigation_bar_button(cx, ids!(moments_button)).clicked(actions) {
            cx.action(crate::moments::ui::MomentsAction::Open { author: None });
        }
        if self.view.navigation_bar_button(cx, ids!(octoscript_apps_button)).clicked(actions) {
            cx.action(crate::octoscript_apps::MiniAppsAction::Open);
        }
        if self.view.navigation_bar_button(cx, ids!(file_transfer_button)).clicked(actions) {
            cx.action(crate::moments::ui::MomentsAction::FileTransfer);
        }
    }

    /// A page that lives behind "More" (Contacts, or a space picked elsewhere)
    /// opens the group, so the marked item is never hidden.
    pub(super) fn reveal_more_for(&mut self, cx: &mut Cx, tab: &SelectedTab) {
        if !self.more_open && matches!(tab, SelectedTab::Contacts | SelectedTab::Space { .. }) {
            self.set_more_open(cx, true);
        }
    }
}
