//! The Merge rules section: members read the rules, only an admin gets the
//! editor, and the editor's fields round-trip through the gate's parser.
use super::*;
use ::hagency::issue_watch::safeguards::{SUGGESTED_LOW_RISK_PATHS, content_of, parse_policy};

const SANDBOX: &str = "low_risk_paths: [\"**/*.md\", \"docs/**\"]\nmax_changed_lines: 50\nrequired_agent_approvals: 1\nrequire_ci: false\n";

fn rules(text: &str) -> RoomRead {
    RoomRead::Rules { event_id: "$e".into(), sender: "@admin:x".into(), content: content_of(text) }
}

#[test]
fn members_see_the_rules_read_only_and_admins_get_the_editor() {
    let member = Shown::Ready { read: rules(SANDBOX), editable: false };
    let admin = Shown::Ready { read: rules(SANDBOX), editable: true };
    assert_eq!(lines(&member), lines(&admin), "the same plain lines for everyone");
    assert_eq!(lines(&member), [
        "Low-risk paths: **/*.md, docs/**", "Up to 50 changed lines", "1 approval(s): another Agent's, or under an owner's maintainer rules the maintainer's review of an outside author's PR or a trusted GitHub reviewer's", "CI not required",
        "Always a person: .hagency/, .github/, CODEOWNERS", "Set by @admin:x",
    ]);
    assert!(!editor_shown(&member) && editor_shown(&admin));
    assert_eq!(hint(&member), "Only a room admin can change the merge rules.");
    assert!(hint(&admin).contains("Agents and other members cannot"));
    assert!(!editor_shown(&Shown::Loading) && !editor_shown(&Shown::Failed("403".into())));
}

#[test]
fn no_refused_or_invalid_rules_say_every_merge_needs_a_person() {
    let none = lines(&Shown::Ready { read: RoomRead::None, editable: false });
    assert_eq!(none, ["No merge rules in this room: every merge needs a person."]);
    let refused = lines(&Shown::Ready { read: RoomRead::Refused("set by an Agent or a Hagency service account"), editable: false });
    assert!(refused[0].contains("ignored") && refused[0].contains("every merge needs a person"));
    assert!(lines(&Shown::Ready { read: rules("merge_all: yes\n"), editable: false })[0].contains("not valid"));
}

#[test]
fn the_editor_starts_from_the_suggestion_and_saves_what_the_gate_reads() {
    let draft = draft_of(&RoomRead::None);
    assert_eq!(draft.paths, SUGGESTED_LOW_RISK_PATHS.join(", "), "the owner's suggested list, one constant");
    let saved = policy_of(&draft, &RoomRead::None).unwrap();
    assert!(saved.always_human.iter().any(|g| g.contains("LICENSE")), "legal files stay with a person");
    let text = ::hagency::issue_watch::safeguards::render_policy(&saved).unwrap();
    assert_eq!(parse_policy(&text), Some(saved));
    let edited = Draft { paths: "docs/**, *.md".into(), max_lines: "20".into(), approvals: "2".into(), require_ci: true };
    let policy = policy_of(&edited, &rules(SANDBOX)).unwrap();
    assert_eq!((policy.low_risk_paths, policy.max_changed_lines, policy.required_agent_approvals, policy.require_ci), (vec!["docs/**".to_owned(), "*.md".to_owned()], 20, 2, true));
    assert_eq!(draft_of(&rules(SANDBOX)).max_lines, "50", "prefilled from the room's rules");
}

#[test]
fn unusable_fields_are_refused_before_anything_is_sent() {
    let ok = draft_of(&RoomRead::None);
    for bad in [
        Draft { paths: " , ".into(), ..ok.clone() },
        Draft { max_lines: "0".into(), ..ok.clone() },
        Draft { max_lines: "many".into(), ..ok.clone() },
        Draft { approvals: "0".into(), ..ok.clone() },
        Draft { paths: "docs/\"x\"".into(), ..ok.clone() },
        Draft { paths: "a #b".into(), ..ok.clone() },
    ] {
        assert!(policy_of(&bad, &RoomRead::None).is_err(), "{bad:?}");
    }
}
