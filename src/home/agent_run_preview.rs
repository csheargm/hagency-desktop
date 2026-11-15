//! When the run card's live text may replace what is on screen.
/// The live text to put on screen, or `None` to leave the panel as it is.
///
/// The answer grows while the Agent writes, and replacing the text drops a
/// selection. While the reader selects in a running answer the text holds
/// still; the latest text shows once the selection is released, the run ends,
/// or the panel turns to another run.
pub(super) fn preview_to_show<'a>(
    shown: Option<&str>,
    latest: &'a str,
    running: bool,
    same_run: bool,
    selecting: bool,
) -> Option<&'a str> {
    if running && same_run && selecting && shown.is_some() {
        return None;
    }
    (shown != Some(latest)).then_some(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_text_holds_still_while_the_reader_selects_in_a_running_answer() {
        // First text, and every later one, shows when nothing is selected.
        assert_eq!(preview_to_show(None, "a", true, true, false), Some("a"));
        assert_eq!(preview_to_show(Some("a"), "ab", true, true, false), Some("ab"));
        // The same text is not parsed again.
        assert_eq!(preview_to_show(Some("ab"), "ab", true, true, false), None);
        // A selection in the running answer keeps what is on screen.
        assert_eq!(preview_to_show(Some("a"), "ab", true, true, true), None);
        // It shows again once released, when the run ends, or for another run.
        assert_eq!(preview_to_show(Some("a"), "ab", true, true, false), Some("ab"));
        assert_eq!(preview_to_show(Some("a"), "ab", false, true, true), Some("ab"));
        assert_eq!(preview_to_show(Some("a"), "other", true, false, true), Some("other"));
        // Nothing on screen yet is never held back.
        assert_eq!(preview_to_show(None, "a", true, true, true), Some("a"));
    }

}
