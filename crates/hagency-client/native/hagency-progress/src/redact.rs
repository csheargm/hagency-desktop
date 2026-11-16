//! Secrets never reach a progress step. Steps are shown in the room and kept
//! in its history, so a command, URL, pattern or query is scrubbed of
//! credentials before it is shortened: header values, secret flags and
//! assignments, URL credentials and queries, and anything shaped like a key.

/// What a removed secret reads as.
pub const REDACTED: &str = "[redacted]";
/// Names that mark the value after them as secret.
const SECRET_NAMES: [&str; 9] =
    ["token", "secret", "passw", "pwd", "apikey", "api_key", "api-key", "auth", "credential"];
/// Prefixes of well-known credentials (OpenAI, Anthropic, GitHub, Slack, AWS, JWT).
const KEY_PREFIXES: [&str; 9] =
    ["sk-", "sk_", "ghp_", "gho_", "ghs_", "github_pat_", "xox", "AKIA", "eyJ"];
/// A run of key-like characters at least this long is treated as a key.
const MIN_BLOB_CHARS: usize = 24;
/// Short flags that take a password in these programs only (`-p` is a port or
/// a package elsewhere): `mysql -p secret`, `curl -u user:pass`.
const SECRET_SHORT_FLAGS: [(&str, &str); 6] =
    [("mysql", "-p"), ("mysqldump", "-p"), ("mariadb", "-p"), ("sshpass", "-p"), ("curl", "-u"), ("wget", "--password")];

fn secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET_NAMES.iter().any(|n| name.contains(n))
}
fn quotes(c: char) -> bool {
    matches!(c, '\'' | '"' | '`')
}
/// A credential-shaped word: a known key prefix, or a long base64/hex run.
fn key_shaped(word: &str) -> bool {
    let core = word.trim_matches(|c: char| quotes(c) || matches!(c, ',' | ';' | ')' | '('));
    if KEY_PREFIXES.iter().any(|p| core.starts_with(p) && core.len() >= p.len() + 8) {
        return true;
    }
    let keyish = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-');
    core.len() >= MIN_BLOB_CHARS
        && core.chars().all(keyish)
        && core.chars().any(|c| c.is_ascii_digit())
        && core.chars().any(|c| c.is_ascii_alphabetic())
        && !core.contains("//")
}
/// `https://user:pass@host/p?q=1#f` -> `https://[redacted]@host/p?[redacted]`.
fn url(word: &str) -> Option<String> {
    let start = word.find("://")?;
    let (scheme, rest) = word.split_at(start + 3);
    let (rest, userinfo) = match rest.find('@') {
        Some(at) if !rest[..at].contains('/') => (&rest[at + 1..], true),
        _ => (rest, false),
    };
    let mut out = scheme.to_owned();
    if userinfo {
        out.push_str(REDACTED);
        out.push('@');
    }
    match rest.find(['?', '#']) {
        Some(cut) => {
            out.push_str(&rest[..cut]);
            out.push('?');
            out.push_str(REDACTED);
        }
        None => out.push_str(rest),
    }
    Some(out)
}
/// `NAME=value` or `--name=value` with a secret name keeps only the name.
fn assignment(word: &str) -> Option<String> {
    let (name, _) = word.split_once('=')?;
    let bare = name.trim_start_matches(|c: char| c == '-' || quotes(c));
    (secret_name(bare) && !bare.is_empty()).then(|| format!("{name}={REDACTED}"))
}
/// Whether the word makes the next one secret (`Bearer x`, `--token x`, `Authorization: x`).
fn marks_next(word: &str, first: &str) -> bool {
    let bare = word.trim_matches(quotes).to_ascii_lowercase();
    if matches!(bare.as_str(), "bearer" | "basic" | "token") {
        return true;
    }
    if let Some(name) = bare.strip_suffix(':') {
        return secret_name(name);
    }
    if let Some(flag) = bare.strip_prefix("--") {
        return secret_name(flag) && !flag.contains('=');
    }
    let program = first.rsplit('/').next().unwrap_or(first);
    SECRET_SHORT_FLAGS.iter().any(|(p, f)| *p == program && *f == bare)
}

/// The text with every secret replaced by [`REDACTED`]; whitespace is folded.
pub fn redact(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let first = words.first().copied().unwrap_or("");
    let mut out = Vec::with_capacity(words.len());
    let mut hide_next = false;
    for word in &words {
        let shown = if std::mem::take(&mut hide_next) || key_shaped(word) {
            REDACTED.to_owned()
        } else if let Some(clean) = url(word).or_else(|| assignment(word)) {
            clean
        } else {
            word.to_string()
        };
        hide_next = marks_next(word, first);
        out.push(shown);
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hides(text: &str, secret: &str) {
        let shown = redact(text);
        assert!(!shown.contains(secret), "{text:?} -> {shown:?}");
        assert!(shown.contains(REDACTED), "{text:?} -> {shown:?}");
    }
    #[test]
    fn credentials_in_commands_are_removed() {
        hides("curl -H 'Authorization: Bearer sk-secretvalue123' https://service/", "secretvalue");
        hides("curl -H \"X-Api-Key: abc123\" https://x/", "abc123");
        hides("gh auth login --with-token hunter2", "hunter2");
        hides("tool --token=hunter2 run", "hunter2");
        hides("tool --password hunter2", "hunter2");
        hides("mysql -u root -p hunter2 db", "hunter2");
        hides("curl -u alice:hunter2 https://x/", "hunter2");
        hides("GITHUB_TOKEN=hunter2 gh pr create", "hunter2");
        hides("export OPENAI_API_KEY=hunter2", "hunter2");
        hides("git clone https://alice:hunter2@github.com/o/r.git", "hunter2");
        hides("curl https://x/cb?access_token=hunter2&a=1", "hunter2");
        hides("echo ghp_abcdefghijklmnop1234", "ghp_abcdefghijklmnop1234");
        hides("echo dGhpcyBpcyBhIGxvbmcgc2VjcmV0IGtleTEyMzQ=", "dGhpcyBpcyBh");
        hides("echo 0123456789abcdef0123456789abcdef", "0123456789abcdef");
    }
    #[test]
    fn ordinary_commands_read_unchanged() {
        for text in ["gh pr create --fill", "cargo test -p hagency", "mkdir -p src/bin", "git log --oneline -5",
            "https://docs.rs/serde/latest/serde/", "src/console/owned_runtime/run_controls.rs"] {
            assert_eq!(redact(text), text);
        }
    }
}

#[cfg(test)]
mod review_cases {
    /// GPT/Ollama review (Nov 15): dotted bearer tokens (JWT-like) and secrets
    /// inside header values must not survive.
    #[test]
    fn dotted_bearer_tokens_are_hidden() {
        for line in [
            "curl -H 'Authorization: Bearer sk.secret1234567890abcd' https://api.example/",
            "curl -H \"Authorization: Bearer eyJhbGciOi.eyJzdWIiOi.c2lnbmF0dXJl\" https://x/",
            "curl -H 'X-Api-Key: a.b.c.secretvalue123' https://x/",
        ] {
            let out = super::redact(line);
            assert!(!out.contains("secret1234567890abcd") && !out.contains("eyJzdWIiOi") && !out.contains("secretvalue123"), "{line} -> {out}");
        }
    }
}
