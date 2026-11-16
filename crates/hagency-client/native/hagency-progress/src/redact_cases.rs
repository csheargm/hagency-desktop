//! Adversarial redaction table: every bypass from the Nov 15 ChatGPT reviews
//! (module review and PR #118 review) plus the earlier dotted-token cases.
use super::{redact, REDACTED};

/// (step text, fragments that must not survive)
const LEAKS: &[(&str, &[&str])] = &[
    // Quoted multi-word secrets after a flag or header.
    ("tool --password 'correct horse battery staple'", &["correct", "horse", "staple"]),
    ("tool --password=\"correct horse battery\"", &["correct", "horse"]),
    ("curl -H 'X-Api-Key: first second' https://x/", &["first", "second"]),
    ("tool --password 'never closed hunter2", &["never", "hunter2"]),
    // Compact and spaced headers.
    ("curl -H 'X-Api-Key:hunter2' https://x/", &["hunter2"]),
    ("curl -H 'X-Api-Key:abc123' https://x/", &["abc123"]),
    ("curl -H 'Authorization:Basic dXNlcjpwYXNz' https://x/", &["dXNlcjpwYXNz"]),
    ("curl -H Authorization:Basic dXNlcjpwYXNz https://x/", &["dXNlcjpwYXNz"]),
    ("curl -H 'Authorization: Bearer abc.def' https://x/", &["abc.def"]),
    ("grep Authorization: Bearer hunter2 logs", &["hunter2"]),
    ("curl -H 'Cookie: session=hunter2' https://x/", &["hunter2"]),
    // Wrappers before the program.
    ("sudo mysql -p hunter2", &["hunter2"]),
    ("env X=1 mysql -p hunter2", &["hunter2"]),
    ("sudo curl -u alice:hunter2 https://x/", &["hunter2"]),
    ("sudo -u root mysql -phunter2 db", &["hunter2"]),
    ("nice -n 5 mysql -p hunter2", &["hunter2"]),
    ("time curl -u alice:hunter2 https://x/", &["hunter2"]),
    ("ls | xargs mysql -p hunter2", &["hunter2"]),
    ("command /usr/bin/mysql -p hunter2", &["hunter2"]),
    ("sh -c \"curl -u alice:hunter2 https://x/\"", &["hunter2"]),
    // Attached short options and long user flags.
    ("mysql -phunter2", &["hunter2"]),
    ("curl -ualice:hunter2 https://x/", &["hunter2"]),
    ("curl --user alice:hunter2 https://x/", &["hunter2"]),
    ("curl --user=alice:hunter2 https://x/", &["hunter2"]),
    ("mysql --password=hunter2", &["hunter2"]),
    // Dotted and opaque tokens without a marker.
    ("echo a.b.c.secretvalue", &["secretvalue"]),
    ("echo eyJhbGciOi.eyJzdWIiOi.c2lnbmF0dXJl", &["eyJzdWIiOi"]),
    ("echo abc_def-ghi.jkl0123456789x", &["jkl0123456789x"]),
    // Earlier review_cases (dotted bearer tokens inside header values).
    ("curl -H 'Authorization: Bearer sk.secret1234567890abcd' https://api.example/", &["secret1234567890abcd"]),
    ("curl -H \"Authorization: Bearer eyJhbGciOi.eyJzdWIiOi.c2lnbmF0dXJl\" https://x/", &["eyJzdWIiOi"]),
    ("curl -H 'X-Api-Key: a.b.c.secretvalue123' https://x/", &["secretvalue123"]),
];

#[test]
fn every_review_bypass_is_hidden() {
    for (text, secrets) in LEAKS {
        let shown = redact(text);
        assert!(shown.contains(REDACTED), "{text:?} -> {shown:?}");
        for secret in *secrets {
            assert!(!shown.contains(secret), "{text:?} leaks {secret:?}: {shown:?}");
        }
    }
}

#[test]
fn common_safe_commands_read_unchanged() {
    for text in ["mkdir -p src/bin", "cargo test -p hagency-progress", "ls -la && echo hello world",
        "npm run build | tee out.log", "sudo apt-get install -y graphviz", "git checkout -b fix/redact",
        "cat crates/hagency-client/native/hagency-progress/src/step.rs"] {
        assert_eq!(redact(text), text);
    }
}
