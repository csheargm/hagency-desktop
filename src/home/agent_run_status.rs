//! The localized words for an Agent run: "Working… running gh pr create" while
//! it runs, "Done in 41 s · 9 steps" once it completes. The run card, thread
//! summaries and the rooms list all show these, never the event's stored body.
use ::hagency::agent_run_controls::{Card, Progress, State};

/// One Agent run state, localized.
pub(crate) fn status_label(state: State) -> &'static str {
    crate::i18n::tr(match state {
        State::Running => "Working… you can add instructions or stop this run",
        State::Stopping => "Stopping; waiting for the runtime to finish and settle.",
        State::Completed => "Done",
        State::Interrupted => "Stopped",
        State::Failed => "Failed",
        State::Unknown => "The run went offline; check its result.",
    })
}
/// The state with the live step or the finished summary, when the Agent sent one.
pub(crate) fn status_text(state: State, progress: Option<&Progress>) -> String {
    match (state, progress) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Language, text_for};
    use ::hagency::agent_run_controls::MSGTYPE;

    /// The status a run card shows for an `im.hagency.agent_run` message's data.
    fn status_of_message(data: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
        let card = data.get(MSGTYPE)?;
        let state = serde_json::from_value(card.get("state")?.clone()).ok()?;
        let progress = card.get("progress").and_then(|p| serde_json::from_value(p.clone()).ok());
        Some(status_text(state, progress.as_ref()))
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
    #[test]
    fn progress_words_are_english_keys_with_a_chinese_translation() {
        for key in ["Working… {step}", "Done in {s} s · {n} steps", "Done in {s} s · 1 step"] {
            assert_eq!(text_for(Language::English, key), key);
            let zh = text_for(Language::Chinese, key);
            assert_ne!(zh, key, "{key} has no Chinese text");
            for slot in ["{step}", "{s}", "{n}"] {
                assert_eq!(key.contains(slot), zh.contains(slot), "{key}: {slot}");
            }
        }
    }
}
