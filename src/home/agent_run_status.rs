//! The localized words for an Agent run: "Working… running gh pr create" while
//! it runs, "Done in 41 s · 9 steps" once it completes. The run card, thread
//! summaries and the rooms list all show these, never the event's stored body.
use ::hagency::agent_run_controls::{APPROVAL_WAIT_STEP, Card, MSGTYPE, Progress, State};

/// One Agent run state, localized.
pub(crate) fn status_label(state: State) -> &'static str {
    crate::i18n::tr(state.english_status())
}
/// The state with the live step or the finished summary, when the Agent sent one.
pub(crate) fn status_text(state: State, progress: Option<&Progress>) -> String {
    match (state, progress) {
        (State::Running, Some(p)) if p.step == APPROVAL_WAIT_STEP => crate::i18n::tr("Waiting for the owner's approval").to_owned(),
        (State::Running, Some(p)) if !p.step.is_empty() => {
            crate::i18n::tr("Working… {step}").replace("{step}", &p.step)
        }
        (State::Completed, Some(Progress { steps, elapsed_secs: Some(secs), .. })) => {
            let template = if *steps == 1 { "Done in {s} s · 1 step" } else { "Done in {s} s · {n} steps" };
            crate::i18n::tr(template)
                .replace("{s}", &(*secs).max(1).to_string())
                .replace("{n}", &steps.to_string())
        }
        _ => status_label(state).to_owned(),
    }
}
pub(crate) fn card_status(card: &Card) -> String {
    status_text(card.state, card.progress.as_ref())
}
/// Seconds as "45 s" or "1 m 20 s", localized.
pub(crate) fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        return crate::i18n::tr("{s} s").replace("{s}", &secs.to_string());
    }
    crate::i18n::tr("{m} m {s} s")
        .replace("{m}", &(secs / 60).to_string())
        .replace("{s}", &(secs % 60).to_string())
}
/// What a card shows now: while it runs, the step (or "Thinking…" before the
/// first one) with a ticking elapsed time, so a long think never looks stuck.
pub(crate) fn live_status(card: &Card, elapsed_secs: u64) -> String {
    if card.state != State::Running {
        return card_status(card);
    }
    let elapsed = format_elapsed(elapsed_secs);
    match card.progress.as_ref().filter(|p| !p.step.is_empty()) {
        Some(p) if p.step == APPROVAL_WAIT_STEP => crate::i18n::tr("Waiting for the owner's approval · {t}").replace("{t}", &elapsed),
        Some(p) => crate::i18n::tr("Working… {step} · {t}").replace("{step}", &p.step).replace("{t}", &elapsed),
        None => crate::i18n::tr("Thinking… {t}").replace("{t}", &elapsed),
    }
}
/// The run clock ticks only while the run is running.
pub(crate) fn needs_tick(card: &Card) -> bool {
    card.state == State::Running
}
/// An Agent run's clock ticked: timelines refresh live run text.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RunClockTick;
/// The collapsed thread summary: the newest running run's live text, else the summary.
pub(crate) fn thread_summary_text<'a>(live: Option<String>, fallback: &'a str) -> std::borrow::Cow<'a, str> {
    live.map_or(std::borrow::Cow::Borrowed(fallback), std::borrow::Cow::Owned)
}
/// The localized status of an `im.hagency.agent_run` message, from its card data.
pub(crate) fn status_of_message(data: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let card = data.get(MSGTYPE)?;
    // A finished run's one message is its answer: previews show the answer.
    if let Some(answer) = card.get("answer").and_then(|a| a.as_str()).filter(|a| !a.is_empty()) {
        return Some(answer.to_owned());
    }
    let state = serde_json::from_value(card.get("state")?.clone()).ok()?;
    let progress = card.get("progress").and_then(|p| serde_json::from_value(p.clone()).ok());
    Some(status_text(state, progress.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Language, text_for};

    #[test]
    fn run_event_previews_use_the_localized_status_not_the_stored_body() {
        let data = serde_json::json!({"im.hagency.agent_run":{"state":"completed"}});
        assert_eq!(status_of_message(data.as_object().unwrap()).as_deref(), Some(crate::i18n::tr("Done")));
        assert!(status_of_message(serde_json::json!({}).as_object().unwrap()).is_none());
        let answered = serde_json::json!({"im.hagency.agent_run":{"state":"completed","answer":"42"}});
        assert_eq!(status_of_message(answered.as_object().unwrap()).as_deref(), Some("42"));
    }
    /// The demo flow: the card reads the step while the Agent works, then the
    /// summary once it is done; a run without steps keeps the upstream words.
    #[test]
    fn a_run_shows_its_current_step_then_how_long_it_took() {
        let working = serde_json::json!({"im.hagency.agent_run":{"state":"running",
            "progress":{"step":"running gh pr create","steps":8}}});
        let status = status_of_message(working.as_object().unwrap()).unwrap();
        assert!(status.ends_with("running gh pr create"), "{status}");
        let done = serde_json::json!({"im.hagency.agent_run":{"state":"completed",
            "progress":{"steps":9,"elapsedSecs":41}}});
        assert_eq!(status_of_message(done.as_object().unwrap()).unwrap(),
            crate::i18n::tr("Done in {s} s · {n} steps").replace("{s}", "41").replace("{n}", "9"));
        assert_eq!(status_text(State::Completed, Some(&Progress { step: String::new(), steps: 1, elapsed_secs: Some(0) })),
            crate::i18n::tr("Done in {s} s · 1 step").replace("{s}", "1"));
        assert_eq!(status_text(State::Running, None), status_label(State::Running));
    }
    fn card(state: State, step: &str) -> Card {
        let mut card: Card = serde_json::from_value(serde_json::json!({"version":1,"id":"c","dispatchId":"d",
            "executionId":"e","roomId":"!r","eventId":"$e","threadRoot":"$t","requester":"@a","owner":"@b",
            "runtime":"codex","state":"running","revision":0})).unwrap();
        card.state = state;
        if !step.is_empty() {
            card.progress = Some(Progress { step: step.into(), steps: 1, elapsed_secs: None });
        }
        card
    }
    #[test]
    fn elapsed_reads_seconds_then_minutes_and_seconds() {
        assert_eq!(format_elapsed(45), crate::i18n::tr("{s} s").replace("{s}", "45"));
        assert_eq!(format_elapsed(80), crate::i18n::tr("{m} m {s} s").replace("{m}", "1").replace("{s}", "20"));
        assert_eq!(format_elapsed(0), crate::i18n::tr("{s} s").replace("{s}", "0"));
    }
    #[test]
    fn a_running_card_ticks_with_thinking_or_the_tool_and_stops_when_done() {
        let thinking = live_status(&card(State::Running, ""), 45);
        assert_eq!(thinking, crate::i18n::tr("Thinking… {t}").replace("{t}", &format_elapsed(45)));
        let tool = live_status(&card(State::Running, "running gh pr view"), 80);
        assert!(tool.contains("running gh pr view") && tool.ends_with(&format_elapsed(80)), "{tool}");
        assert!(needs_tick(&card(State::Running, "")));
        for state in [State::Completed, State::Stopping, State::Failed, State::Interrupted] {
            assert!(!needs_tick(&card(state, "x")), "{state:?}");
            assert_eq!(live_status(&card(state, "x"), 9), card_status(&card(state, "x")));
        }
    }
    /// Thread summaries and previews put the owner's approval wait in words.
    #[test]
    fn an_approval_wait_reads_as_words_not_the_wire_step() {
        let waiting = card(State::Running, APPROVAL_WAIT_STEP);
        assert_eq!(card_status(&waiting), crate::i18n::tr("Waiting for the owner's approval"));
        let live = live_status(&waiting, 7);
        assert_eq!(live, crate::i18n::tr("Waiting for the owner's approval · {t}").replace("{t}", &format_elapsed(7)));
    }
    #[test]
    fn thread_summary_shows_the_live_step_and_falls_back() {
        let live = live_status(&card(State::Running, "reading src/main.rs"), 5);
        assert_eq!(thread_summary_text(Some(live.clone()), "latest reply"), live);
        assert_eq!(thread_summary_text(None, "latest reply"), "latest reply");
    }
    #[test]
    fn progress_words_are_english_keys_with_a_chinese_translation() {
        for key in ["Working… {step}", "Done in {s} s · {n} steps", "Done in {s} s · 1 step",
            "Working… {step} · {t}", "Thinking… {t}", "{s} s", "{m} m {s} s",
            "Waiting for the owner's approval", "Waiting for the owner's approval · {t}"] {
            assert_eq!(text_for(Language::English, key), key);
            let zh = text_for(Language::Chinese, key);
            assert_ne!(zh, key, "{key} has no Chinese text");
            for slot in ["{step}", "{s}", "{n}", "{t}", "{m}"] {
                assert_eq!(key.contains(slot), zh.contains(slot), "{key}: {slot}");
            }
        }
    }
    #[test]
    fn hosted_run_bodies_follow_the_ui_language() {
        use ::hagency::agent_run_controls::set_status_text;
        set_status_text(super::status_label);
        assert_eq!(super::status_label(State::Running), "Working… you can add instructions or stop this run");
        assert_eq!(text_for(Language::Chinese, State::Running.english_status()), "Agent 正在处理，可追加指令或停止本次任务。");
        for state in [State::Stopping, State::Completed, State::Interrupted, State::Failed, State::Unknown] {
            assert_ne!(text_for(Language::Chinese, state.english_status()), state.english_status(), "{state:?} has a zh-CN entry");
        }
    }
}
