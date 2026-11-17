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

pub(crate) fn footer(card: &Card, elapsed_secs: u64, waiting_approval: bool) -> Footer {
    if waiting_approval && card.state.active() {
        return Footer::Approval(format!("⚠️ {}", crate::i18n::tr("Waiting for your approval")));
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

/// Whether the owner's approval holds this run up (a pending request on its message).
pub(crate) fn waits_for_approval(card: &Card) -> bool {
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

/// The link to the run's progress thread, "3 updates"; `None` for none.
pub(crate) fn updates_text(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some(crate::i18n::tr("1 update").to_owned()),
        n => Some(crate::i18n::format("{n} updates", &[("n", n.to_string())])),
    }
}

/// Asks the room to open a run's progress thread.
#[derive(Clone, Debug)]
pub struct ShowRunUpdates {
    pub room: String,
    pub thread_root: String,
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
        assert_eq!(footer(&done, 9, false), Footer::Quiet);
        assert_eq!(footer(&done, 9, true), Footer::Quiet, "a finished run no longer waits");
        assert_eq!(duration(&done).as_deref(), Some(format_elapsed(4).as_str()));
        let running = card(State::Running, "", steps("running ls", 1, None));
        assert!(matches!(footer(&running, 5, false), Footer::Working(t) if t.starts_with("⏳") && t.contains("running ls")));
        assert!(matches!(footer(&running, 5, true), Footer::Approval(t) if t.starts_with("⚠️")));
        let failed = card(State::Failed, "", steps("running cargo test", 2, Some(3)));
        assert!(matches!(footer(&failed, 0, false), Footer::Failed(t) if t.starts_with("❌") && t.ends_with("running cargo test")));
        assert!(matches!(footer(&card(State::Completed, "", None), 0, false), Footer::Other(_)), "no answer: say so");
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
            "1 step", "{n} steps", "Last step: {step}", "Time: {t}", "Runtime: {runtime}"] {
            assert_eq!(text_for(Language::English, key), key);
            let zh = text_for(Language::Chinese, key);
            assert_ne!(zh, key, "{key} has no Chinese text");
            for slot in ["{n}", "{step}", "{t}", "{runtime}"] {
                assert_eq!(key.contains(slot), zh.contains(slot), "{key}: {slot}");
            }
        }
    }
}
