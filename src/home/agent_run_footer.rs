//! What a run card says under its answer. A finished answer says nothing (its
//! duration shows on hover); the line appears only while the Agent works,
//! waits for an approval, or failed. "What I did" lists what the run record
//! carries: its steps, the last step, the time and the runtime.
use ::hagency::agent_run_controls::{Card, State};
use super::agent_run_status::{format_elapsed, live_status, status_label};

/// The status line under a run card, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Footer {
    /// A completed run with an answer: the answer speaks for itself.
    Quiet,
    Working(String),
    /// The run waits for an owner's approval; the card offers "Open approval".
    Approval(String),
    Failed(String),
    /// Stopping, stopped, offline, or completed without an answer.
    Other(String),
}
impl Footer {
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Quiet => "",
            Self::Working(t) | Self::Approval(t) | Self::Failed(t) | Self::Other(t) => t,
        }
    }
}

/// Whom a running card waits for, as this viewer sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Waiting {
    No,
    /// This viewer decides: "Waiting for your approval" and "Open approval".
    ForYou,
    /// Someone else's Agent waits for its owner (the requester's view).
    ForOwner,
}

/// `owner` names the run's owner for `Waiting::ForOwner`.
pub(crate) fn footer(card: &Card, elapsed_secs: u64, waiting: Waiting, owner: &str) -> Footer {
    if card.state.active() {
        match waiting {
            Waiting::ForYou => return Footer::Approval(format!("⚠️ {}", crate::i18n::tr("Waiting for your approval"))),
            Waiting::ForOwner => {
                let t = format_elapsed(elapsed_secs);
                let text = crate::i18n::format("Waiting for {owner}'s approval ({t})", &[("owner", owner.to_owned()), ("t", t)]);
                return Footer::Working(format!("⏳ {text}"));
            }
            Waiting::No => {}
        }
    }
    match card.state {
        State::Running => Footer::Working(format!("⏳ {}", live_status(card, elapsed_secs))),
        State::Completed if !card.answer.is_empty() => Footer::Quiet,
        // The run record carries no failure reason; the step it failed on is the best evidence.
        State::Failed => Footer::Failed(match card.progress.as_ref().filter(|p| !p.step.is_empty()) {
            Some(p) => format!("❌ {} · {}", status_label(State::Failed), p.step),
            None => format!("❌ {}", status_label(State::Failed)),
        }),
        _ => Footer::Other(super::agent_run_status::card_status(card)),
    }
}

/// Whom this run waits for, as `viewer` sees it: a pending request on its
/// message that this viewer can still decide, or the run's own step saying
/// it waits for its owner (every viewer sees that step, so the requester
/// learns why it paused). The step alone never asks THIS viewer to act: an
/// Agent writes its own card, so only a real request gives "your approval"
/// and the button (GPT review).
pub(crate) fn waiting(card: &Card) -> Waiting {
    waiting_with(card, card.state.active() && viewer_decides(card))
}

fn waiting_with(card: &Card, decides: bool) -> Waiting {
    let blocked = card.progress.as_ref().is_some_and(|p| p.step == ::hagency::agent_run_controls::APPROVAL_WAIT_STEP);
    match (card.state.active(), decides, blocked) {
        (false, _, _) => Waiting::No,
        (true, true, _) => Waiting::ForYou,
        (true, false, true) => Waiting::ForOwner,
        (true, false, false) => Waiting::No,
    }
}

/// Whether this viewer can decide a pending request on this run's message.
fn viewer_decides(card: &Card) -> bool {
    let Ok(room) = <&matrix_sdk::ruma::RoomId>::try_from(card.room_id.as_str()) else { return false };
    // Only this run's own request (not any approval in its thread), and only
    // one this viewer can still decide.
    use crate::hagency::pending_approvals::{LocalDecision, for_event};
    for_event(room, &card.event_id, false)
        .is_some_and(|p| p.can_decide() && matches!(p.local, LocalDecision::None | LocalDecision::Failed(..)))
}

/// The run's duration, for the answer's hover line; `None` until it finished.
pub(crate) fn duration(card: &Card) -> Option<String> {
    card.progress.as_ref().and_then(|p| p.elapsed_secs).map(|s| format_elapsed(s.max(1)))
}

/// "What I did": `None` (no Details button) when the run took no steps.
pub(crate) fn what_i_did(card: &Card) -> Option<String> {
    let p = card.progress.as_ref().filter(|p| p.steps > 0)?;
    let mut lines = vec![if p.steps == 1 {
        crate::i18n::tr("1 step").to_owned()
    } else {
        crate::i18n::format("{n} steps", &[("n", p.steps.to_string())])
    }];
    if !p.step.is_empty() {
        lines.push(crate::i18n::format("Last step: {step}", &[("step", p.step.clone())]));
    }
    if let Some(t) = duration(card) {
        lines.push(crate::i18n::format("Time: {t}", &[("t", t)]));
    }
    if !card.runtime.is_empty() {
        lines.push(crate::i18n::format("Runtime: {runtime}", &[("runtime", card.runtime.clone())]));
    }
    Some(lines.join("\n"))
}

/// The link that opens the run's progress in place, "3 updates"; `None` for none.
pub(crate) fn updates_text(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some(crate::i18n::tr("1 update").to_owned()),
        n => Some(crate::i18n::format("{n} updates", &[("n", n.to_string())])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::hagency::agent_run_controls::Progress;
    use crate::i18n::{Language, text_for};

    fn card(state: State, answer: &str, progress: Option<Progress>) -> Card {
        let mut card: Card = serde_json::from_value(serde_json::json!({"version":1,"id":"c","dispatchId":"d",
            "executionId":"e","roomId":"!r","eventId":"$e","threadRoot":"$t","requester":"@a","owner":"@b",
            "runtime":"claude","state":"running","revision":0})).unwrap();
        card.state = state;
        card.answer = answer.into();
        card.progress = progress;
        card
    }
    fn steps(step: &str, steps: u32, secs: Option<u64>) -> Option<Progress> {
        Some(Progress { step: step.into(), steps, elapsed_secs: secs })
    }
    #[test]
    fn a_successful_answer_is_quiet_and_only_work_approval_and_failure_speak() {
        let done = card(State::Completed, "42", steps("running gh pr create", 3, Some(4)));
        assert_eq!(footer(&done, 9, Waiting::No, ""), Footer::Quiet);
        assert_eq!(footer(&done, 9, Waiting::ForYou, ""), Footer::Quiet, "a finished run no longer waits");
        assert_eq!(duration(&done).as_deref(), Some(format_elapsed(4).as_str()));
        let running = card(State::Running, "", steps("running ls", 1, None));
        assert!(matches!(footer(&running, 5, Waiting::No, ""), Footer::Working(t) if t.starts_with("⏳") && t.contains("running ls")));
        assert!(matches!(footer(&running, 5, Waiting::ForYou, ""), Footer::Approval(t) if t.starts_with("⚠️")));
        let failed = card(State::Failed, "", steps("running cargo test", 2, Some(3)));
        assert!(matches!(footer(&failed, 0, Waiting::No, ""), Footer::Failed(t) if t.starts_with("❌") && t.ends_with("running cargo test")));
        assert!(matches!(footer(&card(State::Completed, "", None), 0, Waiting::No, ""), Footer::Other(_)), "no answer: say so");
    }
    /// Chris's test: Anna asked Betty's Agent, whose run waits for Betty's
    /// card. Anna reads "⏳ Waiting for Betty's approval (12 s)"; Betty reads
    /// "Waiting for your approval" with "Open approval" once her app holds the
    /// request; the step alone (a card any Agent writes) never asks her to act.
    #[test]
    fn the_requester_sees_whom_a_run_waits_for() {
        let step = ::hagency::agent_run_controls::APPROVAL_WAIT_STEP;
        let blocked = card(State::Running, "", steps(step, 1, None));
        assert_eq!(waiting_with(&blocked, false), Waiting::ForOwner, "the requester, or an owner without the request");
        assert_eq!(waiting_with(&blocked, true), Waiting::ForYou, "a viewer who can decide");
        let shown = footer(&blocked, 12, Waiting::ForOwner, "Betty");
        assert!(matches!(&shown, Footer::Working(t) if t.starts_with("⏳") && t.contains("Betty") && t.contains(&format_elapsed(12))), "{shown:?}");
        let working = card(State::Running, "", steps("running gh issue list", 1, None));
        assert_eq!(waiting_with(&working, false), Waiting::No);
        let mut done = blocked.clone();
        done.state = State::Completed;
        assert_eq!(waiting_with(&done, true), Waiting::No, "a finished run waits for nobody");
    }
    #[test]
    fn what_i_did_lists_the_record_and_is_hidden_without_steps() {
        assert!(what_i_did(&card(State::Completed, "42", None)).is_none());
        assert!(what_i_did(&card(State::Completed, "42", steps("", 0, Some(2)))).is_none());
        let text = what_i_did(&card(State::Completed, "42", steps("running gh pr create", 9, Some(41)))).unwrap();
        for part in ["9", "running gh pr create", &format_elapsed(41), "claude"] {
            assert!(text.contains(part), "{part} missing in {text}");
        }
        assert_eq!(updates_text(0), None);
        assert_eq!(updates_text(1).as_deref(), Some(crate::i18n::tr("1 update")));
        assert!(updates_text(4).unwrap().contains('4'));
    }
    #[test]
    fn quiet_reply_words_are_english_keys_with_a_chinese_translation() {
        for key in ["What I did", "More", "Open approval", "Waiting for your approval", "1 update", "{n} updates",
            "1 step", "{n} steps", "Last step: {step}", "Time: {t}", "Runtime: {runtime}", "Waiting for {owner}'s approval ({t})"] {
            assert_eq!(text_for(Language::English, key), key);
            let zh = text_for(Language::Chinese, key);
            assert_ne!(zh, key, "{key} has no Chinese text");
            for slot in ["{n}", "{step}", "{t}", "{runtime}", "{owner}"] {
                assert_eq!(key.contains(slot), zh.contains(slot), "{key}: {slot}");
            }
        }
    }
}
