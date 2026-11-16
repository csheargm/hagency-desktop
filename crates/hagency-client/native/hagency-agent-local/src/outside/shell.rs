//! A deliberately small shell reader for "ask only for outside changes".
//!
//! It accepts a line only when it can read every part of it: words joined by
//! `&&`, `||`, `;` or `|`, plain or quoted, a `2>&1` redirect, the `$?` status
//! inside double quotes, and one quoted heredoc feeding `git commit -F -`.
//! Anything else (backticks, `$(...)`, variables, globs, braces, `~`, other
//! redirects, backslashes, background `&`) makes the whole line ask.
use std::path::{Path, PathBuf};

/// The words of each segment, and the segment a quoted heredoc feeds.
pub(crate) struct Line {
    pub segments: Vec<Vec<String>>,
    pub heredoc_at: Option<usize>,
    /// `after_and[i]`: segment `i` runs only if segment `i - 1` succeeded
    /// (joined by `&&`); always false for the first segment.
    pub after_and: Vec<bool>,
}

/// Read `line`, or `None` when any part of it is outside what this reader knows.
/// Lines after a heredoc's terminator are further commands, read the same way.
pub(crate) fn parse(line: &str) -> Option<Line> {
    let Some((header, after)) = split_heredoc(line)? else {
        let (segments, after_and) = segments(line)?;
        return Some(Line { segments, heredoc_at: None, after_and });
    };
    let (mut all, mut after_and) = segments(header)?;
    let heredoc_at = Some(all.len() - 1);
    for next in after.split('\n').filter(|l| !l.trim().is_empty()) {
        let (more, mut joins) = segments(next)?;
        // A new line runs whatever the line before it did.
        joins[0] = false;
        all.extend(more);
        after_and.append(&mut joins);
    }
    Some(Line { segments: all, heredoc_at, after_and })
}

/// Split at a `<<'TAG'` heredoc: the command line before it and the text after
/// its terminator line. `Some(None)` when there is no heredoc.
fn split_heredoc(line: &str) -> Option<Option<(&str, &str)>> {
    let Some(at) = line.find("<<") else { return Some(None) };
    let rest = line[at + 2..].strip_prefix('\'')?;
    let close = rest.find('\'')?;
    let tag = &rest[..close];
    if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let after = &rest[close + 1..];
    let newline = after.find('\n')?;
    if !after[..newline].trim_matches([' ', '\t']).is_empty() {
        return None;
    }
    let body = &after[newline + 1..];
    let mut offset = 0;
    for body_line in body.split('\n') {
        offset += body_line.len() + 1;
        if body_line == tag {
            return Some(Some((&line[..at], body.get(offset..).unwrap_or(""))));
        }
    }
    None
}

/// Tokenize one heredoc-free line into segments of words, and whether each
/// segment follows a `&&`.
fn segments(line: &str) -> Option<(Vec<Vec<String>>, Vec<bool>)> {
    let mut out = vec![Vec::new()];
    let mut after_and = vec![false];
    let mut word = String::new();
    let (mut started, mut quoted) = (false, false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => end_word(&mut out, &mut word, &mut started, &mut quoted),
            '\'' => {
                let mut closed = false;
                for q in chars.by_ref() {
                    if q == '\'' { closed = true; break; }
                    word.push(q);
                }
                if !closed { return None; }
                started = true;
                quoted = true;
            }
            '"' => {
                quoted_double(&mut chars, &mut word)?;
                started = true;
                quoted = true;
            }
            '&' | '|' | ';' => {
                if c == '&' && chars.next_if_eq(&'&').is_none() { return None; }
                if c == '|' { chars.next_if_eq(&'|'); }
                end_word(&mut out, &mut word, &mut started, &mut quoted);
                if out.last().is_some_and(Vec::is_empty) { return None; }
                out.push(Vec::new());
                after_and.push(c == '&');
            }
            '>' => {
                // Only `2>&1`, written as one unquoted word.
                if quoted || word != "2" || chars.next() != Some('&') || chars.next() != Some('1') { return None; }
                if chars.peek().is_some_and(|n| !matches!(n, ' ' | '\t' | ';' | '|' | '&')) { return None; }
                word.clear();
                started = false;
            }
            '~' | '#' if !started => return None,
            '`' | '$' | '<' | '(' | ')' | '\\' | '\n' | '\r' | '*' | '?' | '[' | ']' | '{' | '}' | '!' => return None,
            other => { word.push(other); started = true; }
        }
    }
    end_word(&mut out, &mut word, &mut started, &mut quoted);
    (!out.iter().any(Vec::is_empty)).then_some((out, after_and))
}

/// A double-quoted run: literal except `$?`; any other `$`, a backtick, a
/// backslash or `!` makes the line ask.
fn quoted_double(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, word: &mut String) -> Option<()> {
    while let Some(q) = chars.next() {
        match q {
            '"' => return Some(()),
            '$' if chars.next_if_eq(&'?').is_some() => word.push_str("$?"),
            '$' | '`' | '\\' | '!' => return None,
            other => word.push(other),
        }
    }
    None
}

fn end_word(out: &mut [Vec<String>], word: &mut String, started: &mut bool, quoted: &mut bool) {
    if *started && let Some(segment) = out.last_mut() {
        segment.push(std::mem::take(word));
    }
    *started = false;
    *quoted = false;
}

/// Lexically join `path` onto `base` (a `cd` target); `..` pops.
pub(crate) fn lexical_join(base: &Path, path: &str) -> PathBuf {
    let mut resolved = PathBuf::new();
    for part in base.join(path).components() {
        match part {
            std::path::Component::ParentDir => { resolved.pop(); }
            std::path::Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    resolved
}
