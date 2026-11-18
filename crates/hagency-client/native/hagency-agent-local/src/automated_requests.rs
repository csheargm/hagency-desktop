//! Requests a Hagency automation posted, not a person typing: the owner's
//! issue watcher hands an issue off and its review loop asks for a review
//! or a merge, from the owner's own account. The server tells them apart by
//! their content fields (hagency-server `agent-service/src/automated_requests.rs`,
//! `io.hagency.issue_handoff` / `io.hagency.review_loop`); a device's
//! dispatch carries only the body, so this reads the same messages by the
//! leads the watcher writes (the ones the Activity view and the loop's
//! re-send already read). Nobody waits in the room for these runs: the
//! tracker's own lines report progress and outcome, so the Agent posts no
//! run card and no interim text for them (owner, 2026-11-18).

/// Whether `text` (the request as the Agent reads it) from `requester` is
/// one of `owner`'s automations. Only the owner's own account counts: a
/// room member's look-alike message keeps its card.
pub fn is_automated(owner: &str, requester: &str, text: &str) -> bool {
    requester == owner
        && (crate::activity::handoff_issue(text).is_some()
            || crate::merge_policy::step_resend::loop_step_of(text).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge_policy::step_resend::{MERGE_NOTICE_LEAD, REVIEW_NOTICE_LEAD, merge_command, review_ask};
    const OWNER: &str = "@anna:x";
    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn the_owners_hand_off_and_loop_steps_are_automated() {
        let handoff = format!("{}#12 in a/b — @agent please fix it on a branch", hagency_runtime::attribution::ISSUE_NOTICE_LEAD);
        assert!(is_automated(OWNER, OWNER, &handoff));
        let review = format!("{REVIEW_NOTICE_LEAD}@agent:x {}: url", review_ask(7, "a/b", HEAD));
        assert!(is_automated(OWNER, OWNER, &review));
        let merge = format!("{MERGE_NOTICE_LEAD}{} — merging: @agent:x run exactly `{}`", &HEAD[..12], merge_command(7, HEAD, "a/b"));
        assert!(is_automated(OWNER, OWNER, &merge));
    }

    #[test]
    fn a_typed_request_or_a_members_look_alike_is_not() {
        assert!(!is_automated(OWNER, OWNER, "@agent please fix issue #12 in a/b"));
        let handoff = format!("{}#12 in a/b", hagency_runtime::attribution::ISSUE_NOTICE_LEAD);
        assert!(!is_automated(OWNER, "@bob:x", &handoff), "only the owner's own account posts hand-offs");
        assert!(!is_automated(OWNER, OWNER, ""), "an unreadable request is a person's");
    }
}
