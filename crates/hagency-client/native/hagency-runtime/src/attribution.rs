//! How an Agent signs its GitHub work: the per-Agent identity rules every
//! room runtime (Claude's system prompt, Codex's developer instructions) is
//! given, and the per-turn line that names who asked and where. The wording
//! lives only here, so both runtimes say the same thing.

/// Leads the issue-watch notice the desktop poller posts for the owner; a
/// turn that starts with it is an issue-watch hand-off, not a typed request.
pub const ISSUE_NOTICE_LEAD: &str = "🔔 New GitHub issue ";

/// Opens the per-turn context line; the identity rules point at it.
pub const REQUEST_CONTEXT_LEAD: &str = "[Hagency request context: requester ";

/// The identity rules for the Agent `label` with Matrix ID `mxid`.
pub fn identity_note(label: &str, mxid: &str) -> String {
    let localpart = mxid.trim_start_matches('@').split(':').next().filter(|l| !l.is_empty()).unwrap_or("agent");
    let mxid = if mxid.is_empty() { "unknown Matrix ID" } else { mxid };
    format!(
        "You act as the Hagency agent \"{label}\" ({mxid}). Every git commit you make must end with the trailer \
`Co-authored-by: {label} <{localpart}@agents.hagency.invalid>`. Every GitHub issue, pull request or review you open \
must state in its body: `Opened by agent {label} ({mxid}) on request from <requester>`, where <requester> is the value \
given in the turn's `{lead}…` line; never ask for it. Use that line in place of any footer a tool adds by default \
(such as \"Generated with Claude Code\" or a Co-Authored-By line naming the model): remove such a footer. \
Report the resulting GitHub URLs in your reply.",
        lead = REQUEST_CONTEXT_LEAD.trim_end(),
    )
}

/// Who a turn is on request from: the Matrix sender, or the owner's issue
/// watch when the owner's own message is an issue-watch notice.
pub fn requester_label(owner: &str, sender: &str, input: &str) -> String {
    if sender == owner && input.trim_start().starts_with(ISSUE_NOTICE_LEAD) {
        format!("issue watch for {owner}")
    } else {
        sender.to_owned()
    }
}

/// The model-facing turn text: the request context line, then the input.
pub fn with_request_context(requester: &str, room: &str, input: &str) -> String {
    format!("{REQUEST_CONTEXT_LEAD}{requester}, room {room}]\n\n{input}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_note_names_label_mxid_trailer_and_replaces_tool_footer() {
        let text = identity_note("ben-codex", "@_hagency_agt_x:localhost:8452");
        assert!(text.contains("\"ben-codex\" (@_hagency_agt_x:localhost:8452)"));
        assert!(text.contains("`Co-authored-by: ben-codex <_hagency_agt_x@agents.hagency.invalid>`"));
        assert!(text.contains("`Opened by agent ben-codex (@_hagency_agt_x:localhost:8452) on request from <requester>`"));
        assert!(text.contains("Generated with Claude Code"));
        assert!(text.contains(REQUEST_CONTEXT_LEAD.trim_end()));
        assert!(identity_note("x", "").contains("(unknown Matrix ID)"));
    }

    #[test]
    fn requester_is_sender_or_issue_watch_for_owner() {
        assert_eq!(requester_label("@o:t", "@bob:t", "fix it"), "@bob:t");
        let notice = format!("{ISSUE_NOTICE_LEAD}#6 in a/b");
        assert_eq!(requester_label("@o:t", "@o:t", &notice), "issue watch for @o:t");
        assert_eq!(requester_label("@o:t", "@bob:t", &notice), "@bob:t");
        assert_eq!(requester_label("@o:t", "@o:t", "hi"), "@o:t");
    }

    #[test]
    fn request_context_prepends_requester_and_room() {
        let text = with_request_context("@bob:t", "!r:t", "open a PR");
        assert_eq!(text, "[Hagency request context: requester @bob:t, room !r:t]\n\nopen a PR");
    }
}
