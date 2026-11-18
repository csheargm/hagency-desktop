//! The Safeguards card states what the code enforces: every number and
//! protected name comes from `Enforced` and the gate's parsed policy, so a
//! changed constant (here: a changed fixture) changes the line.
use super::safeguards::{Card, Enforced, Repo, Watch, card, trial_owner_merges, watch_of};
use ::hagency::issue_watch::safeguards::{self as engine, PolicyFile, parse_policy};
use serde_json::json;

/// The sandbox repository's own policy file (csheargm/hagency-demo-sandbox).
const SANDBOX_POLICY: &str = "low_risk_paths: [\"**/*.md\", \"docs/**\"]\nmax_changed_lines: 50\nrequired_agent_approvals: 1\nrequire_ci: false\n";

fn sandbox_watch() -> Watch {
    Watch { repo: "csheargm/hagency-demo-sandbox".into(), push_freely: true, auto_merge: true, trial_owner_merges: None }
}
fn read(policy: &str, protected: Option<bool>) -> Repo {
    let policy = parse_policy(policy).map_or(PolicyFile::Unreadable, PolicyFile::Read);
    Repo::Read { branch: "main".into(), protected, policy }
}
fn all_lines(card: &Card) -> String {
    [&card.alone, &card.needs_you, &card.limits].into_iter().flatten().cloned().chain([card.protection.clone()]).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_enforced_values_are_the_engines_constants() {
    let enforced = Enforced::current();
    assert_eq!(enforced.max_attempts, engine::MAX_ATTEMPTS);
    assert_eq!(enforced.free_retries, engine::MAX_FREE_RETRIES);
    assert_eq!(enforced.handoff_secs, engine::HANDOFF_TIMEOUT_SECS);
    assert_eq!(enforced.policy_path, engine::POLICY_PATH);
    assert_eq!(enforced.protected_paths, engine::PROTECTED_PREFIXES.to_vec());
    assert_eq!(enforced.protected_branches, engine::PROTECTED_BRANCHES.to_vec());
    let text = all_lines(&card(&sandbox_watch(), &read(SANDBOX_POLICY, Some(false)), &enforced));
    assert!(text.contains(&format!("{} tries per issue", engine::MAX_ATTEMPTS)), "{text}");
    assert!(text.contains(&format!("{} minutes per try", engine::HANDOFF_TIMEOUT_SECS / 60)), "{text}");
    assert!(text.contains(engine::CODEOWNERS), "{text}");
}

#[test]
fn changing_an_enforced_value_changes_its_line() {
    let enforced = Enforced {
        max_attempts: 7,
        free_retries: 4,
        handoff_secs: 90 * 60,
        protected_paths: vec!["ops/".into()],
        protected_branches: vec!["trunk".into()],
        ..Enforced::current()
    };
    let text = all_lines(&card(&sandbox_watch(), &read(SANDBOX_POLICY, None), &enforced));
    for expected in ["7 tries per issue", "4 extra tries", "90 minutes per try", "Always a person: ops/, CODEOWNERS", "never the default branch or trunk"] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    assert!(!text.contains("3 tries per issue") && !text.contains(".github/"), "{text}");
}

#[test]
fn the_merge_rule_line_follows_the_policy_file() {
    let enforced = Enforced::current();
    let rule = |policy: &str| card(&sandbox_watch(), &read(policy, None), &enforced).limits[3].clone();
    assert_eq!(rule(SANDBOX_POLICY), "Merge rule (.hagency/merge-policy.yml): **/*.md, docs/**, up to 50 changed lines, CI not required, 1 approval(s) from another Agent");
    let stricter = "low_risk_paths: [\"docs/**\"]\nmax_changed_lines: 10\nrequired_agent_approvals: 2\nrequire_ci: true\nalways_human: [\"docs/legal/**\"]\n";
    assert_eq!(rule(stricter), "Merge rule (.hagency/merge-policy.yml): docs/**, up to 10 changed lines, CI required, 2 approval(s) from another Agent");
    assert_eq!(rule("tier: trusted\n"), "Merge rule (.hagency/merge-policy.yml): any path, no line limit, CI required, 1 approval(s) from another Agent");
    assert!(card(&sandbox_watch(), &read(stricter, None), &enforced).limits[4].ends_with(", docs/legal/**"));
    let zero = "low_risk_paths: [\"docs/**\"]\nrequired_agent_approvals: 0\n";
    assert!(rule(zero).ends_with(&format!("{} approval(s) from another Agent", engine::MIN_AGENT_APPROVALS)), "the gate's floor, not the file's 0");
}

#[test]
fn merges_need_the_owner_without_the_switch_or_a_readable_policy() {
    let enforced = Enforced::current();
    let manual = Watch { auto_merge: false, ..sandbox_watch() };
    let off = card(&manual, &read(SANDBOX_POLICY, None), &enforced);
    assert!(off.needs_you.contains(&"Every merge (merges are set to People merge)".to_owned()));
    assert!(!all_lines(&off).contains("Approve and merge"));
    let absent = card(&sandbox_watch(), &Repo::Read { branch: "main".into(), protected: None, policy: PolicyFile::Absent }, &enforced);
    assert!(absent.needs_you.contains(&"Every merge: the repository has no .hagency/merge-policy.yml".to_owned()));
    assert_eq!(absent.limits[3], "Merge rule: none, the repository has no .hagency/merge-policy.yml");
    let bad = card(&sandbox_watch(), &read("merge_everything: yes\n", None), &enforced);
    assert!(bad.needs_you.iter().any(|l| l.contains("does not accept")));
    let failed = card(&sandbox_watch(), &Repo::Failed("HTTP 403".into()), &enforced);
    assert!(failed.limits[3].ends_with(": HTTP 403") && !all_lines(&failed).contains("Approve and merge"));
}

#[test]
fn without_push_freely_nothing_runs_alone() {
    let ask = Watch { push_freely: false, auto_merge: false, ..sandbox_watch() };
    let shown = card(&ask, &Repo::Loading, &Enforced::current());
    assert_eq!(shown.alone, vec!["Nothing yet: every step asks you".to_owned()]);
    assert_eq!(shown.needs_you[0], "Every push, pull request and command");
    assert_eq!(shown.limits[3], "Merge rule: reading .hagency/merge-policy.yml…");
}

#[test]
fn branch_protection_is_recommended_with_its_state_when_known() {
    let enforced = Enforced::current();
    let line = |protected| card(&sandbox_watch(), &read(SANDBOX_POLICY, protected), &enforced).protection;
    assert_eq!(line(Some(false)), "Recommended: turn on GitHub branch protection for main (it is off).");
    assert_eq!(line(Some(true)), "GitHub branch protection is on for main. Keep it on.");
    assert_eq!(line(None), "Recommended: turn on GitHub branch protection for the default branch of csheargm/hagency-demo-sandbox.");
}

#[test]
fn the_saved_watch_maps_to_the_cards_watch_and_the_trial_setting_is_optional() {
    let saved = json!({"mode": "trusted", "repo": "acme/app", "pushTrusted": true});
    assert_eq!(watch_of(&saved, true), Some(Watch { repo: "acme/app".into(), push_freely: true, auto_merge: true, trial_owner_merges: None }));
    assert_eq!(watch_of(&json!({"mode": "off", "repo": "acme/app"}), true), None, "Off shows no card");
    assert_eq!(watch_of(&json!({"mode": "all", "repo": ""}), true), None);
    assert_eq!(trial_owner_merges(&json!({"trialOwnerMerges": 3})), Some(3));
    let trial = Watch { trial_owner_merges: Some(3), ..sandbox_watch() };
    let shown = card(&trial, &read(SANDBOX_POLICY, None), &Enforced::current());
    assert!(shown.needs_you.contains(&"Merging its first 3 pull requests (trial)".to_owned()));
}

/// The card as the owner sees it: hidden until a watch is shown, then the
/// three groups and the recommendation, and hidden again for an Off watch.
#[test]
fn the_card_widget_shows_the_models_lines() {
    use super::safeguards_card::{SafeguardsCard, bullets, test_support::show};
    use makepad_widgets::*;
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut widget = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.SafeguardsCard {}});
        let widget = SafeguardsCard::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty());
        widget
    });
    assert!(!widget.visible(), "hidden before anything is read");
    let repo = read(SANDBOX_POLICY, Some(false));
    show(&mut widget, &mut cx, "agt_1", Some((sandbox_watch(), repo.clone())));
    let model = card(&sandbox_watch(), &repo, &Enforced::current());
    assert!(widget.visible());
    assert_eq!(widget.label(&mut cx, ids!(alone)).text(), bullets(&model.alone));
    assert_eq!(widget.label(&mut cx, ids!(needs_you)).text(), bullets(&model.needs_you));
    assert_eq!(widget.label(&mut cx, ids!(limits)).text(), bullets(&model.limits));
    assert_eq!(widget.label(&mut cx, ids!(protection)).text(), model.protection);
    show(&mut widget, &mut cx, "agt_1", None);
    assert!(!widget.visible(), "an Off watch hides the card");
}

/// Prints the card for the sandbox watch (`--nocapture`), for the report.
#[test]
fn sandbox_card_text() {
    let shown = card(&sandbox_watch(), &read(SANDBOX_POLICY, Some(false)), &Enforced::current());
    let group = |title: &str, lines: &[String]| format!("{title}\n{}", super::safeguards_card::bullets(lines));
    println!("Safeguards\n{}\n{}\n{}\n{}", group("May do alone", &shown.alone), group("Needs you", &shown.needs_you), group("Limits", &shown.limits), shown.protection);
}
