//! The status parts of the issue loop's follow-through (split from
//! `tracker.rs`, R4): one short line per tracked issue, in the words the room
//! already saw. UI text, translated here.
use super::merge_hold::{LEVEL_ONLY_HOLD, Missing};
use super::tracker::Followed;
use ::hagency::issue_watch::issue_track::{MAX_ATTEMPTS, State, Track};

/// What a followed pull request waits for, in the words the room already saw.
pub(crate) fn hold_of(followed: &Followed) -> Option<String> {
    match followed {
        Followed::Held(missing) => Some(missing.iter().map(|m| m.label()).collect::<Vec<_>>().join("; ")),
        Followed::AwaitsPerson => Some(LEVEL_ONLY_HOLD.to_owned()),
        Followed::NeedsPerson => Some("a person's review and merge approval".to_owned()),
        Followed::WaitingCi => Some("CI".to_owned()),
        _ => None,
    }
}

/// Whether a head already told "ready, a person merges" still is: only while
/// this owner may still merge (the gate's own check). A revoked or unreadable
/// permission goes the full gate, which publishes the ordinary hold.
pub(super) fn still_ready(permission: &Result<String, String>) -> bool {
    super::review_loop::can_merge(permission.as_deref().ok())
}

pub(super) fn waiting_line(track: &Track) -> String {
    let n = track.number.to_string();
    match (track.state, track.pr) {
        (State::HandedOff, _) if track.attempts > 1 => crate::i18n::format("#{number} retrying ({attempt}/{max})", &[("number", n), ("attempt", track.attempts.to_string()), ("max", MAX_ATTEMPTS.to_string())]),
        (State::HandedOff, _) => crate::i18n::format("#{number} waiting for a pull request", &[("number", n)]),
        (_, Some(pr)) => crate::i18n::format("#{number} PR #{pr} closed unmerged — needs a person", &[("number", n), ("pr", pr.to_string())]),
        _ => crate::i18n::format("#{number} waiting", &[("number", n)]),
    }
}
pub(super) fn pr_line(number: i64, pr: u64, followed: Followed) -> String {
    let args = [("number", number.to_string()), ("pr", pr.to_string())];
    let template = match followed {
        Followed::Held(missing) => {
            let needs = missing.iter().map(|m| crate::i18n::tr(m.label())).collect::<Vec<_>>().join("; ");
            let [number, pr] = args;
            return crate::i18n::format("#{number} PR #{pr} needs: {needs}", &[number, pr, ("needs", needs)]);
        }
        Followed::Unreachable(failures) => {
            let [number, pr] = args;
            return crate::i18n::format("#{number} PR #{pr} can't reach GitHub ({failures} checks in a row), retrying", &[number, pr, ("failures", failures.to_string())]);
        }
        Followed::WaitingCi => "#{number} PR #{pr} waiting for CI",
        Followed::Merging => "#{number} PR #{pr} approved, merging",
        Followed::Requested => "#{number} PR #{pr} in review",
        Followed::NeedsPerson => "#{number} PR #{pr} waiting for your review",
        Followed::Draft => "#{number} PR #{pr} is a draft",
        // The room's line is unchanged: it needs a person (room setting).
        Followed::AwaitsPerson => return pr_line(number, pr, Followed::Held(vec![Missing::Autonomy])),
    };
    crate::i18n::format(template, &args)
}
pub(crate) fn terminal_line(track: &Track) -> Option<String> {
    let n = track.number.to_string();
    match track.state {
        State::Failed => Some(crate::i18n::format("#{number} gave up after {max} attempts", &[("number", n), ("max", MAX_ATTEMPTS.to_string())])),
        State::NeedsHuman => Some(crate::i18n::format("#{number} needs a person", &[("number", n)])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::merge_hold::Gate;
    use super::super::tracker_live::followed_of;

    /// GPT review, Nov 19: the status said "ready — a person merges" for any
    /// Autonomy hold, also a stale authority or a missing permission.
    #[test]
    fn only_a_level_only_hold_is_published_as_ready() {
        let level = vec![(Missing::Autonomy, ::hagency::issue_watch::safeguards::room_autonomy::ASSIST_REASON.to_owned())];
        let level_only = followed_of(Gate::Hold(level), true);
        assert_eq!(level_only, Followed::AwaitsPerson);
        assert_eq!(hold_of(&level_only).as_deref(), Some(LEVEL_ONLY_HOLD));
        assert_eq!(pr_line(127, 131, level_only), "#127 PR #131 needs: a person (room setting)", "the room's line is unchanged");
        let stale = followed_of(Gate::Hold(vec![(Missing::Autonomy, "the room's authority could not be read".to_owned())]), true);
        assert_eq!(stale, Followed::Held(vec![Missing::Autonomy]));
        assert_eq!(hold_of(&stale).as_deref(), Some(Missing::Autonomy.label()));
    }

    /// Nov 19: write access revoked after the Assist "ready" notice, and the
    /// settled head still published "ready — a person merges".
    #[test]
    fn a_settled_head_whose_merge_permission_was_revoked_is_no_longer_ready() {
        use super::super::merge_hold_tests::{Fake, HEAD, check};
        use ::hagency::issue_watch::safeguards::room_autonomy::ASSIST_REASON;
        assert!(still_ready(&Ok("write".into())), "still allowed: the shortcut stands");
        assert!(!still_ready(&Ok("read".into())), "revoked: the full gate judges again");
        assert!(!still_ready(&Err("timeout".into())), "unreadable: the full gate fails loudly");
        let fake = Fake::new(serde_json::json!({"decision": "human", "head": HEAD, "reason": ASSIST_REASON, "kind": "autonomy", "approvers": 1}));
        let followed = followed_of(check(&fake, Some("read"), true), true);
        assert_eq!(followed, Followed::Held(vec![Missing::Autonomy, Missing::Permission]), "the ordinary hold");
        assert_ne!(hold_of(&followed).as_deref(), Some(LEVEL_ONLY_HOLD));
    }
}
