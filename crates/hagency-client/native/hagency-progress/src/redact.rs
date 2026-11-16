//! Secrets never reach a progress step. Steps are shown in the room and kept
//! in its history, so a command, URL, pattern or query is scrubbed of
//! credentials before it is shortened: header values, secret flags and
//! assignments, URL credentials and queries, and anything shaped like a key.

/// What a removed secret reads as.
pub const REDACTED: &str = "[redacted]";
/// Names that mark the value after them as secret.
const SECRET_NAMES: [&str; 10] =
    ["token", "secret", "passw", "pwd", "apikey", "api_key", "api-key", "auth", "credential", "cookie"];
/// Words that announce a credential right after them (`Bearer x`, `Basic x`).
const AUTH_SCHEMES: [&str; 4] = ["bearer", "basic", "token", "digest"];
/// Prefixes of well-known credentials (OpenAI, Anthropic, GitHub, GitLab, Slack, AWS, Google, JWT).
const KEY_PREFIXES: [&str; 12] =
    ["sk-", "sk_", "ghp_", "gho_", "ghs_", "github_pat_", "glpat-", "xox", "AKIA", "AIza", "npm_", "eyJ"];
/// A run of base64/hex characters at least this long is treated as a key.
const MIN_BLOB_CHARS: usize = 24;
/// An opaque run of letters, digits, dots, dashes and underscores at least this long is a key.
const MIN_OPAQUE_CHARS: usize = 20;
/// A long run must hold one unbroken part at least this long to be a key.
const MIN_KEY_SEGMENT: usize = 12;
/// A dotted token (`a.b.c`, JWT) at least this long whose last part is this long is a key.
const MIN_DOTTED_CHARS: usize = 16;
const MIN_DOTTED_TAIL: usize = 8;
/// Flags that take a credential in these programs only (`-p` is a port or a
/// package elsewhere). The program may appear anywhere earlier in the same
/// command, so wrappers (`sudo`, `env X=1`, `nice`, `time`, `xargs`) still count.
const SECRET_PROGRAM_FLAGS: [(&str, &str); 10] = [
    ("mysql", "-p"), ("mysqldump", "-p"), ("mariadb", "-p"), ("sshpass", "-p"), ("curl", "-u"),
    ("curl", "-U"), ("curl", "--user"), ("curl", "--proxy-user"), ("wget", "--user"), ("wget", "--password"),
];
/// Tokens that start a new command, so earlier programs stop applying.
const SEPARATORS: [&str; 5] = ["|", "||", "&&", ";", "&"];

fn secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET_NAMES.iter().any(|n| name.contains(n))
}
fn quotes(c: char) -> bool {
    matches!(c, '\'' | '"' | '`')
}
fn unquoted(word: &str) -> &str {
    word.trim_matches(quotes)
}
fn scheme(word: &str) -> bool {
    AUTH_SCHEMES.contains(&unquoted(word).to_ascii_lowercase().as_str())
}
/// `name.ext` with a short alphabetic extension reads as a file, not a key.
fn file_like(core: &str) -> bool {
    core.rsplit_once('.').is_some_and(|(stem, ext)| {
        (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphabetic()) && !stem.contains('.')
    })
}
/// A credential-shaped word: a known prefix, a long base64/hex run, a long
/// opaque token, or a dotted token such as a JWT.
fn key_shaped(word: &str) -> bool {
    let core = word.trim_matches(|c: char| quotes(c) || matches!(c, ',' | ';' | ')' | '('));
    if KEY_PREFIXES.iter().any(|p| core.starts_with(p) && core.len() >= p.len() + 8) {
        return true;
    }
    // Branch and file names split into short words (`release-2026-11-15-hotfix`); keys do not.
    let longest = core.split(['/', '-', '_', '.', '+', '=']).map(str::len).max().unwrap_or(0);
    let mixed = longest >= MIN_KEY_SEGMENT
        && core.chars().any(|c| c.is_ascii_digit())
        && core.chars().any(|c| c.is_ascii_alphabetic());
    let blob = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-');
    if core.len() >= MIN_BLOB_CHARS && mixed && core.chars().all(blob) && !core.contains("//") {
        return true;
    }
    let opaque = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    if !core.chars().all(opaque) {
        return false;
    }
    let tail = core.rsplit('.').next().unwrap_or("");
    let dotted = core.matches('.').count() >= 2 && core.len() >= MIN_DOTTED_CHARS && tail.len() >= MIN_DOTTED_TAIL;
    dotted || (core.len() >= MIN_OPAQUE_CHARS && mixed && !file_like(core))
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
/// `X-Api-Key:v`, `'Authorization: Bearer v'`: everything after the colon is
/// hidden. Returns the shown word and whether the next word is secret too
/// (the value was empty or only a scheme such as `Authorization:Basic`).
fn header(word: &str) -> Option<(String, bool)> {
    let (name, value) = unquoted(word).split_once(':')?;
    let header_chars = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_');
    if name.is_empty() || !name.chars().all(header_chars) || !secret_name(name) {
        return None;
    }
    let value = value.trim();
    if value.is_empty() || scheme(value) {
        return Some((word.to_owned(), true));
    }
    let quote = word.chars().next().filter(|c| quotes(*c)).map(String::from).unwrap_or_default();
    Some((format!("{quote}{name}: {REDACTED}{quote}"), false))
}
/// Conservative fallback: a secret name inside a word followed by `:` or `=`
/// (`-H'Authorization: x'`, `--header=X-Api-Key:x`, `{"password":"x"}`)
/// hides the rest of the word, and the next word if nothing followed.
fn embedded(word: &str) -> Option<(String, bool)> {
    let lower = word.to_ascii_lowercase();
    let cut = SECRET_NAMES.iter().filter_map(|n| {
        let at = lower.find(n)? + n.len();
        let name_end = at + lower[at..].find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')))?;
        let gap = lower[name_end..].find(|c: char| !(quotes(c) || c == ' '))?;
        matches!(lower[name_end + gap..].chars().next(), Some(':' | '=')).then_some(name_end + gap + 1)
    }).min()?;
    let rest = unquoted(&word[cut..]).trim();
    Some((format!("{}{REDACTED}", &word[..cut]), rest.is_empty() || scheme(rest)))
}
/// A credential flag of a program named earlier: `-p` alone hides the next
/// word, `-phunter2` and `--user=a:b` hide the attached value.
fn program_flag(word: &str, programs: &[String]) -> Option<(String, bool)> {
    let flags = SECRET_PROGRAM_FLAGS.iter().filter(|(p, _)| programs.iter().any(|q| q == p));
    for (_, flag) in flags {
        if word == *flag {
            return Some((word.to_owned(), true));
        }
        let Some(rest) = word.strip_prefix(flag) else { continue };
        if !flag.starts_with("--") || rest.starts_with('=') {
            let joint = if flag.starts_with("--") { "=" } else { "" };
            return Some((format!("{flag}{joint}{REDACTED}"), false));
        }
    }
    None
}
/// One word as shown, and whether the word after it is secret.
fn word(tok: &str, programs: &[String]) -> (String, bool) {
    if let Some(found) = program_flag(tok, programs) {
        return found;
    }
    let bare = unquoted(tok).to_ascii_lowercase();
    if scheme(tok) || bare.strip_prefix("--").is_some_and(|f| secret_name(f) && !f.contains('=')) {
        return (tok.to_owned(), true);
    }
    // A bare URL (no `NAME=` before it, no spaces) keeps its host and path.
    let lone_url = !tok.contains(char::is_whitespace) && tok.split("://").next().is_some_and(|s| !s.contains('='));
    if let Some(clean) = url(tok).filter(|_| lone_url) {
        return (clean, false);
    }
    if let Some(found) = assignment(tok).map(|s| (s, false)).or_else(|| header(tok)) {
        return found;
    }
    if let Some(found) = embedded(tok) {
        return found;
    }
    if let Some(clean) = url(tok).filter(|_| !tok.contains(char::is_whitespace)) {
        return (clean, false);
    }
    if key_shaped(tok) {
        return (REDACTED.to_owned(), false);
    }
    match tok.chars().next() {
        // A quoted span such as `sh -c "curl -u a:b x"` is scrubbed as its own command.
        Some(q) if quotes(q) && tok.contains(char::is_whitespace) => (format!("{q}{}{q}", redact(unquoted(tok))), false),
        _ => (tok.to_owned(), false),
    }
}
/// Words split on whitespace outside quotes; a quoted span stays one word,
/// and an unclosed quote runs to the end, so hiding it hides everything after.
fn words(text: &str) -> Vec<String> {
    let (mut out, mut cur, mut open) = (Vec::new(), String::new(), None);
    for c in text.chars() {
        match open {
            Some(q) => {
                cur.push(c);
                if c == q {
                    open = None;
                }
            }
            None if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => {
                open = Some(c).filter(|c| quotes(*c));
                cur.push(c);
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The text with every secret replaced by [`REDACTED`]; whitespace between
/// words is folded. A secret flag or header hides its whole (quoted) value.
pub fn redact(text: &str) -> String {
    let mut programs: Vec<String> = Vec::new();
    let mut hide_next = false;
    let mut out = Vec::new();
    for tok in words(text) {
        if std::mem::take(&mut hide_next) {
            hide_next = scheme(&tok);
            out.push(REDACTED.to_owned());
            continue;
        }
        if SEPARATORS.contains(&tok.as_str()) {
            programs.clear();
            out.push(tok);
            continue;
        }
        let (shown, next) = word(&tok, &programs);
        hide_next = next;
        if !tok.starts_with('-') && !tok.contains('=') {
            programs.push(unquoted(&tok).rsplit('/').next().unwrap_or("").to_owned());
        }
        if tok.ends_with(';') {
            programs.clear();
        }
        out.push(shown);
    }
    out.join(" ")
}

#[cfg(test)]
#[path = "redact_cases.rs"]
mod adversarial;

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
