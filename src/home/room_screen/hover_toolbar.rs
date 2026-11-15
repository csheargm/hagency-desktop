//! A Buzz-style toolbar that floats at a message's top-right while the mouse hovers it.
//!
//! It adds no behavior of its own: each button re-dispatches an existing
//! [`MessageAction`], and "React" and "…" open the existing message context menu
//! (whose first entry, "Add Reaction", is the existing reaction picker).
//! Mouse-only: touch layouts keep using long-press for the context menu.

use makepad_widgets::*;

use crate::home::new_message_context_menu::{MessageAbilities, MessageDetails};
use crate::shared::navigation_bar_button::{NavigationBarButtonRef, NavigationBarButtonWidgetExt};

use super::MessageAction;

/// Side length of one square toolbar button. Mirrored into the DSL below.
const BUTTON_SIZE: f64 = 28.0;
/// Gap between two toolbar buttons. Mirrored into the DSL below.
const BUTTON_SPACING: f64 = 2.0;
/// Inner padding of the toolbar card. Mirrored into the DSL below.
const TOOLBAR_PADDING: f64 = 3.0;
/// How far the toolbar sits in from the message's right edge.
const TOOLBAR_RIGHT_INSET: f64 = 16.0;
/// How far the toolbar sits below the message's top edge.
const TOOLBAR_TOP_INSET: f64 = 4.0;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let HoverToolbarButton = NavigationBarButton {
        width: #(BUTTON_SIZE) height: #(BUTTON_SIZE) padding: 0
        tooltip_below: true
        draw_bg +: {color_hover: RINX_HOVER color_active: RINX_SELECTED border_radius: 6}
        icon := Icon {icon_walk: Walk{width: 16 height: 16} draw_icon.color: RINX_MUTED}
    }

    mod.widgets.MessageHoverToolbar = #(MessageHoverToolbar::register_widget(vm)) {
        ..mod.widgets.RoundedView
        width: Fit height: Fit
        flow: Right
        align: Align{y: 0.5}
        spacing: #(BUTTON_SPACING)
        padding: #(TOOLBAR_PADDING)
        show_bg: true
        draw_bg +: {
            color: mod.widgets.RINX_PAGE
            border_radius: 8
            border_size: 1
            border_color: mod.widgets.RINX_BORDER
        }
        react_button := HoverToolbarButton {icon.draw_icon.svg: ICON_ADD_REACTION}
        reply_in_thread_button := HoverToolbarButton {icon.draw_icon.svg: ICON_REPLY_IN_THREAD}
        copy_link_button := HoverToolbarButton {icon.draw_icon.svg: ICON_LINK}
        forward_button := HoverToolbarButton {icon.draw_icon.svg: ICON_ANSWER_FORWARD}
        more_button := HoverToolbarButton {icon.draw_icon.svg: ICON_MORE_VERT}
    }
}

/// One button of the hover toolbar, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolbarButton {
    React,
    ReplyInThread,
    CopyLink,
    Forward,
    More,
}

impl ToolbarButton {
    pub(super) const ALL: [Self; 5] = [Self::React, Self::ReplyInThread, Self::CopyLink, Self::Forward, Self::More];

    /// Whether this button applies to a message with the given abilities.
    /// Uses the same ability flags that gate the matching context-menu entries.
    pub(super) fn is_shown(self, abilities: MessageAbilities) -> bool {
        match self {
            Self::React => abilities.contains(MessageAbilities::CanReact),
            Self::ReplyInThread => abilities.contains(MessageAbilities::CanReplyInThread),
            Self::CopyLink => abilities.contains(MessageAbilities::HasEventId),
            Self::Forward => abilities.contains(MessageAbilities::CanForward),
            Self::More => true,
        }
    }

    /// The tooltip, reusing the matching context-menu label (already translated).
    fn tooltip(self) -> &'static str {
        match self {
            Self::React => "Add Reaction",
            Self::ReplyInThread => "Reply In Thread",
            Self::CopyLink => "Copy Link to Message",
            Self::Forward => "Select / Forward",
            Self::More => "More",
        }
    }
}

/// Maps a toolbar button to the existing action it stands for.
///
/// `menu_pos` is where a context menu opened by this button should appear.
pub(super) fn toolbar_action(button: ToolbarButton, details: MessageDetails, menu_pos: DVec2) -> MessageAction {
    match button {
        ToolbarButton::React | ToolbarButton::More => {
            // `own_reactions` is filled in by `Message::open_context_menu`.
            MessageAction::OpenMessageContextMenu { details, own_reactions: Vec::new(), abs_pos: menu_pos }
        }
        ToolbarButton::ReplyInThread => MessageAction::ReplyInThread(details),
        ToolbarButton::CopyLink => MessageAction::CopyLink(details),
        ToolbarButton::Forward => MessageAction::SelectForward(details),
    }
}

/// The toolbar's size when it shows `button_count` buttons.
pub(super) fn toolbar_size(button_count: usize) -> DVec2 {
    let n = button_count as f64;
    let width = n * BUTTON_SIZE + (n - 1.0).max(0.0) * BUTTON_SPACING + 2.0 * TOOLBAR_PADDING;
    dvec2(width, BUTTON_SIZE + 2.0 * TOOLBAR_PADDING)
}

/// Where the toolbar's top-left corner goes: inside the message's top-right corner,
/// but never left of the message itself.
pub(super) fn toolbar_origin(message_rect: Rect, size: DVec2) -> DVec2 {
    let right = message_rect.pos.x + message_rect.size.x - TOOLBAR_RIGHT_INSET;
    dvec2((right - size.x).max(message_rect.pos.x), message_rect.pos.y + TOOLBAR_TOP_INSET)
}

/// The floating toolbar widget. Its owning `Message` decides when it is shown
/// and where it is drawn; this widget only holds its buttons.
#[derive(Script, ScriptHook, Widget)]
pub struct MessageHoverToolbar {
    #[deref] view: View,
    /// The number of buttons currently shown, which sets the drawn size.
    #[rust] shown_buttons: usize,
}

impl Widget for MessageHoverToolbar {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl MessageHoverToolbar {
    fn button(&self, cx: &mut Cx, button: ToolbarButton) -> NavigationBarButtonRef {
        match button {
            ToolbarButton::React => self.view.navigation_bar_button(cx, ids!(react_button)),
            ToolbarButton::ReplyInThread => self.view.navigation_bar_button(cx, ids!(reply_in_thread_button)),
            ToolbarButton::CopyLink => self.view.navigation_bar_button(cx, ids!(copy_link_button)),
            ToolbarButton::Forward => self.view.navigation_bar_button(cx, ids!(forward_button)),
            ToolbarButton::More => self.view.navigation_bar_button(cx, ids!(more_button)),
        }
    }

    /// Shows only the buttons that apply to a message with these abilities.
    fn apply_abilities(&mut self, cx: &mut Cx, abilities: MessageAbilities) {
        self.shown_buttons = 0;
        for kind in ToolbarButton::ALL {
            let button = self.button(cx, kind);
            let shown = kind.is_shown(abilities);
            button.set_visible(cx, shown);
            if let Some(mut inner) = button.borrow_mut() {
                inner.set_tooltip_text(crate::i18n::tr(kind.tooltip()));
            }
            self.shown_buttons += usize::from(shown);
        }
    }

    /// Hides any tooltip a button opened, since a hidden button gets no hover-out.
    fn clear_tooltips(&self, cx: &mut Cx) {
        for kind in ToolbarButton::ALL {
            let button = self.button(cx, kind);
            if let Some(mut inner) = button.borrow_mut() {
                inner.animator_play(cx, ids!(hover.off));
            }
            cx.widget_action(button.widget_uid(), TooltipAction::HoverOut);
        }
    }

    /// The clicked button, if any, with the screen rect it was drawn at.
    fn clicked(&self, cx: &mut Cx, actions: &Actions) -> Option<(ToolbarButton, Rect)> {
        ToolbarButton::ALL.into_iter().find_map(|kind| {
            let button = self.button(cx, kind);
            button.clicked(actions).then(|| (kind, button.area().rect(cx)))
        })
    }
}

impl MessageHoverToolbarRef {
    /// See [`MessageHoverToolbar::apply_abilities()`].
    pub fn apply_abilities(&self, cx: &mut Cx, abilities: MessageAbilities) {
        if let Some(mut inner) = self.borrow_mut() { inner.apply_abilities(cx, abilities); }
    }

    /// See [`MessageHoverToolbar::clear_tooltips()`].
    pub fn clear_tooltips(&self, cx: &mut Cx) {
        if let Some(inner) = self.borrow() { inner.clear_tooltips(cx); }
    }

    /// See [`MessageHoverToolbar::clicked()`].
    pub fn clicked(&self, cx: &mut Cx, actions: &Actions) -> Option<(ToolbarButton, Rect)> {
        self.borrow().and_then(|inner| inner.clicked(cx, actions))
    }

    /// Draws the toolbar floating at the top-right of `message_rect`,
    /// outside the parent's layout flow.
    pub fn draw_over(&self, cx: &mut Cx2d, scope: &mut Scope, message_rect: Rect) {
        let Some(mut inner) = self.borrow_mut() else { return };
        let size = toolbar_size(inner.shown_buttons);
        let walk = Walk::fixed(size.x, size.y).with_abs_pos(toolbar_origin(message_rect, size));
        let _ = inner.draw_walk(cx, scope, walk);
    }

    /// Whether `abs` falls on the toolbar as last drawn.
    pub fn contains(&self, cx: &Cx, abs: DVec2) -> bool {
        self.borrow().is_some_and(|inner| inner.view.area().clipped_rect(cx).contains(abs))
    }
}

#[cfg(test)]
#[path = "hover_toolbar_tests.rs"]
mod tests;
