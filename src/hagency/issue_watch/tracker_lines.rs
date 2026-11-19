//! The status parts of the issue loop's follow-through (split from
//! `tracker.rs`, R4): one short line per tracked issue, in the words the room
//! already saw. UI text, translated here.
use super::tracker::Followed;
use ::hagency::issue_watch::issue_track::{MAX_ATTEMPTS, State, Track};

/// What a followed pull request waits for, in the words the room already saw.
pub(crate) fn hold_of(followed: &Followed) -> Option<String> {
    match followed {
        Followed::Held(missing) => Some(missing.iter().map(|m| m.label()).collect::<Vec<_>>().join("; ")),
        Followed::NeedsPerson => Some("a person's review and merge approval".to_owned()),
        Followed::WaitingCi => Some("CI".to_owned()),
        _ => None,
    }
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
