//! When a person/Agent hover card opens and closes over an [`super::avatar::Avatar`].
//!
//! [`HoverMachine`] is the pure state machine (unit-tested below); the
//! functions at the bottom are the only hooks `avatar.rs` calls, and they
//! forward to the app-level card in [`crate::profile::hover_card`].

use makepad_widgets::{Cx, Rect, WidgetUid};

use crate::profile::user_profile::UserProfileAndRoomId;

/// How long the pointer must rest on an avatar before its card opens.
pub const HOVER_SHOW_DELAY_SECS: f64 = 0.45;
/// How long the card stays after the pointer leaves both avatar and card.
pub const HOVER_HIDE_DELAY_SECS: f64 = 0.2;

/// Where the card is in its life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HoverPhase {
    #[default]
    Hidden,
    /// The pointer is on an avatar; the show timer is running.
    Opening,
    Open,
    /// The pointer left avatar and card; the hide timer is running.
    Closing,
}

/// What happened to the pointer (or the timer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverInput {
    EnterAnchor,
    LeaveAnchor,
    EnterCard,
    LeaveCard,
    Timeout,
    /// A click (on the avatar or a card button): close at once.
    Dismiss,
}

/// What the card widget must do in response.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HoverEffect {
    Nothing,
    /// (Re)start the timer for this many seconds.
    Arm(f64),
    /// Stop the running timer.
    Disarm,
    Show,
    /// Hide the card and stop any timer.
    Hide,
}

/// The hover card's state: its phase, and where the pointer is.
#[derive(Clone, Copy, Debug, Default)]
pub struct HoverMachine {
    phase: HoverPhase,
    over_anchor: bool,
    over_card: bool,
}

impl HoverMachine {
    pub fn phase(&self) -> HoverPhase {
        self.phase
    }

    /// Advances the machine by one input.
    pub fn step(&mut self, input: HoverInput) -> HoverEffect {
        use HoverInput::*;
        match input {
            EnterAnchor => self.over_anchor = true,
            LeaveAnchor => self.over_anchor = false,
            EnterCard => self.over_card = true,
            LeaveCard => self.over_card = false,
            Timeout | Dismiss => {}
        }
        let (phase, effect) = self.next(input);
        self.phase = phase;
        effect
    }

    fn next(&self, input: HoverInput) -> (HoverPhase, HoverEffect) {
        use {HoverEffect as E, HoverInput as I, HoverPhase as P};
        let pointer_inside = self.over_anchor || self.over_card;
        match (self.phase, input) {
            (_, I::Dismiss) => (P::Hidden, E::Hide),
            (P::Hidden, I::EnterAnchor) => (P::Opening, E::Arm(HOVER_SHOW_DELAY_SECS)),
            (P::Opening, I::LeaveAnchor) => (P::Hidden, E::Disarm),
            (P::Opening, I::Timeout) => (P::Open, E::Show),
            (P::Open, I::LeaveAnchor | I::LeaveCard) if !pointer_inside => {
                (P::Closing, E::Arm(HOVER_HIDE_DELAY_SECS))
            }
            (P::Closing, I::EnterAnchor | I::EnterCard) => (P::Open, E::Disarm),
            (P::Closing, I::Timeout) => (P::Hidden, E::Hide),
            (phase, _) => (phase, E::Nothing),
        }
    }
}

/// The pointer entered the avatar `anchor` (widget `uid`) showing `info`.
pub fn anchor_entered(cx: &mut Cx, uid: WidgetUid, info: &UserProfileAndRoomId, anchor: Rect) {
    crate::profile::hover_card::with_card(cx, |cx, card| card.anchor_entered(cx, uid, info.clone(), anchor));
}

/// The pointer left the avatar widget `uid`.
pub fn anchor_left(cx: &mut Cx, uid: WidgetUid) {
    crate::profile::hover_card::with_card(cx, |cx, card| card.anchor_left(cx, uid));
}

/// The avatar was clicked: its profile opens, so the card closes.
pub fn dismiss(cx: &mut Cx) {
    crate::profile::hover_card::with_card(cx, |cx, card| card.dismiss(cx));
}

#[cfg(test)]
mod tests {
    use super::{HoverEffect as E, HoverInput as I, HoverPhase as P, *};

    fn open_machine() -> HoverMachine {
        let mut m = HoverMachine::default();
        m.step(I::EnterAnchor);
        m.step(I::Timeout);
        m
    }

    #[test]
    fn entering_an_avatar_shows_the_card_only_after_the_hover_delay() {
        let mut m = HoverMachine::default();
        assert_eq!(m.step(I::EnterAnchor), E::Arm(HOVER_SHOW_DELAY_SECS));
        assert_eq!(m.phase(), P::Opening);
        assert_eq!(m.step(I::Timeout), E::Show);
        assert_eq!(m.phase(), P::Open);
    }

    #[test]
    fn passing_over_an_avatar_never_opens_the_card() {
        let mut m = HoverMachine::default();
        m.step(I::EnterAnchor);
        assert_eq!(m.step(I::LeaveAnchor), E::Disarm);
        assert_eq!(m.phase(), P::Hidden);
        assert_eq!(m.step(I::Timeout), E::Nothing);
    }

    #[test]
    fn leaving_the_avatar_hides_the_card_after_the_hide_delay() {
        let mut m = open_machine();
        assert_eq!(m.step(I::LeaveAnchor), E::Arm(HOVER_HIDE_DELAY_SECS));
        assert_eq!(m.phase(), P::Closing);
        assert_eq!(m.step(I::Timeout), E::Hide);
        assert_eq!(m.phase(), P::Hidden);
    }

    #[test]
    fn moving_from_the_avatar_into_the_card_keeps_it_open() {
        let mut m = open_machine();
        m.step(I::LeaveAnchor);
        assert_eq!(m.step(I::EnterCard), E::Disarm);
        assert_eq!(m.phase(), P::Open);
        // A late timer from the cancelled close does nothing.
        assert_eq!(m.step(I::Timeout), E::Nothing);
        assert_eq!(m.phase(), P::Open);
    }

    #[test]
    fn leaving_the_card_hides_it_after_the_delay() {
        let mut m = open_machine();
        m.step(I::LeaveAnchor);
        m.step(I::EnterCard);
        assert_eq!(m.step(I::LeaveCard), E::Arm(HOVER_HIDE_DELAY_SECS));
        assert_eq!(m.step(I::Timeout), E::Hide);
    }

    #[test]
    fn card_entered_before_avatar_left_stays_open() {
        let mut m = open_machine();
        m.step(I::EnterCard);
        assert_eq!(m.step(I::LeaveAnchor), E::Nothing);
        assert_eq!(m.phase(), P::Open);
    }

    #[test]
    fn dismiss_hides_from_any_phase() {
        let mut m = open_machine();
        assert_eq!(m.step(I::Dismiss), E::Hide);
        assert_eq!(m.phase(), P::Hidden);
        m.step(I::EnterAnchor);
        assert_eq!(m.step(I::Dismiss), E::Hide);
        assert_eq!(m.phase(), P::Hidden);
    }
}
