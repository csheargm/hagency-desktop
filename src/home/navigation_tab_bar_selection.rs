//! Visual state of the rail: which button is marked, the view mode, and the
//! Agents alert dot. Split out of `navigation_tab_bar.rs` (R4) with no behavior change.

use makepad_widgets::*;
use crate::{
    login::login_screen::LoginAction,
    settings::app_preferences::{AppPreferencesAction, ViewModeOverride},
    shared::navigation_bar_button::NavigationBarButtonWidgetExt,
};
use super::{NavigationBarAction, NavigationTabBar, SelectedTab, profile_icon::ProfileIconWidgetExt};

impl NavigationTabBar {
    /// Installs a variant selector on our root `AdaptiveView` that honors the
    /// given [`ViewModeOverride`] preference. `Automatic` falls back to the
    /// default width-based selector.
    pub(super) fn apply_view_mode(&mut self, mode: ViewModeOverride) {
        self.view.set_variant_selector(mode.variant_selector());
        self.applied_view_mode = mode;
    }

    /// Updates which navigation tab button is visually marked as selected,
    /// enforcing mutual exclusion across all buttons (like a radio button group).
    ///
    /// If `tab` is `None`, the existing selection is re-applied without changing it.
    pub(super) fn apply_selected_tab(&mut self, cx: &mut Cx, tab: Option<SelectedTab>) {
        if let Some(t) = tab {
            self.selected_tab = t;
        }
        let home    = self.view.navigation_bar_button(cx, ids!(home_button));
        let contacts = self.view.navigation_bar_button(cx, ids!(contacts_button));
        let profile = self.view.profile_icon(cx, ids!(profile_icon));
        // Explore Rooms keeps Home marked: on desktop it is opened from the chat
        // list's "New or Join" menu, so it belongs to Home (as mobile marks Discover).
        home.set_selected(cx, matches!(self.selected_tab, SelectedTab::Home | SelectedTab::AddRoom));
        contacts.set_selected(cx, self.selected_tab == SelectedTab::Contacts);
        profile.set_selected(cx, matches!(self.selected_tab, SelectedTab::Settings | SelectedTab::Me));
        let chats_uid = self.view.navigation_bar_button(cx, ids!(chats_tab)).widget_uid();
        let palette = (chats_uid, self.selected_tab.clone());
        let update_palette = self.mobile_palette.as_ref() != Some(&palette);
        for (id, selected) in [
            (ids!(chats_tab), matches!(self.selected_tab, SelectedTab::Home | SelectedTab::Space {..})),
            (ids!(contacts_tab), self.selected_tab == SelectedTab::Contacts),
            (ids!(discover_tab), matches!(self.selected_tab, SelectedTab::Discover | SelectedTab::AddRoom)),
            (ids!(me_tab), matches!(self.selected_tab, SelectedTab::Me | SelectedTab::Settings)),
        ] {
            let button = self.view.navigation_bar_button(cx, id);
            button.set_selected(cx, selected);
            if update_palette && !button.is_empty() {
                let color = if selected {
                    self.appearance.accent
                } else {
                    self.appearance.ink
                };
                let mut icon = self.view.icon(cx, &[id[0], id!(icon)]);
                script_apply_eval!(cx, icon, {draw_icon +: {color: #(color)}});
                self.view.label(cx, &[id[0], id!(label)]).set_text_color(cx, color);
            }
        }
        self.mobile_palette = Some(palette);
    }
}

impl NavigationTabBar {
    /// A permission request that waits for this account turns the Agents item
    /// red, with a dot after its label, until the request is decided or gone.
    pub(super) fn apply_approval_alert(&mut self, cx: &mut Cx) {
        // So does a resource admission request that waits for the server's administrator.
        let alert = crate::hagency::pending_approvals::total() > 0
            || crate::hagency::pending_admissions::total() > 0;
        self.apply_approval_alert_state(cx, alert);
    }

    pub(super) fn apply_approval_alert_state(&mut self, cx: &mut Cx, alert: bool) {
        // A dot also invites a first Agent; the rail item itself always shows.
        let first_agent = crate::hagency::agent_presence::needs_first_agent();
        let label = self.view.label(cx, ids!(agent_button.label));
        let mut icon = self.view.icon(cx, ids!(agent_button.icon));
        // AdaptiveView creates its desktop variant lazily; there is no Agent
        // icon in the mobile variant. Retry after the desktop widgets exist.
        if label.is_empty() || icon.is_empty() {
            self.approval_alert = None;
            return;
        }
        let text = if alert || first_agent { format!("{} ●", crate::i18n::tr("Agents")) } else { crate::i18n::tr("Agents").to_owned() };
        if self.approval_alert == Some(alert) && label.text() == text {
            return;
        }
        self.approval_alert = Some(alert);
        // The rail is dark in the light theme (ink-on-accent), so the page ink
        // vanished on it until hover; use the rail's own foreground instead.
        let color = if alert { self.appearance.role("color.status.danger.foreground") } else { self.appearance.nav_ink() };
        script_apply_eval!(cx, icon, {draw_icon +: {color: #(color)}});
        label.set_text(cx, &text);
        label.set_text_color(cx, color);
        self.view.redraw(cx);
    }

    /// Follows selections made elsewhere, re-login and view-mode changes.
    pub(super) fn handle_nav_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            // If another widget programmatically selected a new tab,
            // update our buttons' visual selection state accordingly.
            if let Some(NavigationBarAction::TabSelected(tab)) = action.downcast_ref() {
                self.apply_selected_tab(cx, Some(tab.clone()));
                self.reveal_more_for(cx, tab);
                continue;
            }

            // Upon login (mostly re-login), go back to the home tab
            // because the profile/settings tab will have been selected upon logout.
            if let Some(LoginAction::LoginSuccess) = action.downcast_ref() {
                self.apply_selected_tab(cx, Some(SelectedTab::Home));
                cx.action(NavigationBarAction::GoToHome);
                continue;
            }

            if let Some(AppPreferencesAction::ViewModeChanged(new_mode)) = action.downcast_ref() {
                if *new_mode != self.applied_view_mode {
                    self.apply_view_mode(*new_mode);
                    self.view.redraw(cx);
                }
            }
        }
    }
}
