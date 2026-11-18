//! What the Status tab lists, as plain lines (ADR 0023): open Agent tasks
//! for every room; this account's watches; and only for a room with
//! repositories, the cross-repository summary and one section per repository
//! (or the one the filter picked). A room without repositories shows no issue
//! or pull request lines at all.
use super::Watches;
use super::status::RoomStatus;
use super::tasks::Task;
use ::hagency::repo_status::table::{self, Row, Table, Tracker};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// A bold section heading.
    Section(String),
    /// A row: its title and its detail line.
    Item(String, String),
}

fn age(secs: i64) -> String {
    let (n, unit) = match secs {
        s if s < 60 => return crate::i18n::tr("just now").to_owned(),
        s if s < 3600 => (s / 60, "{n} min ago"),
        s if s < 86_400 => (s / 3600, "{n} h ago"),
        s => (s / 86_400, "{n} d ago"),
    };
    crate::i18n::format(unit, &[("n", n.to_string())])
}

fn name(status: &RoomStatus, id: &str) -> String {
    status.names.get(id).cloned().unwrap_or_else(|| id.to_owned())
}

pub fn task_line(status: &RoomStatus, task: &Task) -> Line {
    let title = format!("{} · {}", name(status, &task.agent), crate::i18n::tr(task.state.words()));
    let mut detail = crate::i18n::format("asked by {who} · {age}", &[("who", name(status, &task.requester)), ("age", age(task.age_secs))]);
    if !task.step.is_empty() {
        detail = format!("{detail} · {}", task.step);
    }
    Line::Item(title, detail)
}

/// The localized form of `table::tracker_text`.
fn tracker_text(t: &Tracker) -> String {
    let mut parts = vec![crate::i18n::format("{state} (attempt {n})", &[("state", crate::i18n::tr(t.state.words()).to_owned()), ("n", t.attempts.max(1).to_string())])];
    parts.extend(t.pr.map(|pr| format!("PR #{pr}")));
    parts.extend(t.hold.as_ref().map(|h| crate::i18n::format("held: {hold}", &[("hold", h.clone())])));
    parts.push(format!("{} ({})", t.agent, t.owner));
    parts.join(" · ")
}

fn row_line(row: &Row) -> Line {
    let kind = if row.is_pr { "PR" } else { crate::i18n::tr("issue") };
    let title = format!("#{} {} · {}", row.number, row.title, kind);
    let github = row.github.map_or(crate::i18n::tr("not listed on GitHub"), |g| crate::i18n::tr(g.words()));
    let mut parts = vec![crate::i18n::format("GitHub: {state}", &[("state", github.to_owned())])];
    parts.extend(row.fixes.map(|n| crate::i18n::format("fixes #{number}", &[("number", n.to_string())])));
    parts.extend(row.trackers.iter().map(tracker_text));
    Line::Item(title, parts.join(" · "))
}

fn table_lines(table: &Table, out: &mut Vec<Line>) {
    out.push(Line::Section(table.repo.clone()));
    if table.rows.is_empty() {
        out.push(Line::Item(crate::i18n::tr("Nothing open or recently closed").to_owned(), String::new()));
    }
    out.extend(table.rows.iter().map(row_line));
    if table.truncated > 0 {
        out.push(Line::Item(crate::i18n::format("{n} more not shown", &[("n", table.truncated.to_string())]), String::new()));
    }
}

/// Every line of the tab. `filter` names one repository, or `None` for all.
pub fn lines(watches: &Watches, filter: Option<&str>) -> Vec<Line> {
    let mut out = Vec::new();
    if let Ok(status) = &watches.status {
        out.push(Line::Section(crate::i18n::tr("Agent tasks").to_owned()));
        if status.tasks.is_empty() {
            out.push(Line::Item(crate::i18n::tr("No open Agent tasks").to_owned(), String::new()));
        }
        out.extend(status.tasks.iter().map(|t| task_line(status, t)));
    }
    if !watches.rows.is_empty() {
        out.push(Line::Section(crate::i18n::tr("Your Agents' watches").to_owned()));
        out.extend(watches.rows.iter().map(|w| {
            let (title, detail) = super::panel::row_text(w);
            Line::Item(title, detail)
        }));
    }
    let Ok(status) = &watches.status else { return out };
    if status.tables.is_empty() {
        return out;
    }
    let busy: usize = status.tables.iter().map(table::in_progress).sum();
    let template = if status.tables.len() == 1 { "{busy} in progress across {count} repo" } else { "{busy} in progress across {count} repos" };
    out.push(Line::Section(crate::i18n::format(template, &[("busy", busy.to_string()), ("count", status.tables.len().to_string())])));
    for t in status.tables.iter().filter(|t| filter.is_none_or(|f| t.repo.eq_ignore_ascii_case(f))) {
        table_lines(t, &mut out);
    }
    out
}

/// The repository filter's choices: "All repositories", then each repository.
pub fn filter_labels(watches: &Watches) -> Vec<String> {
    let repos = watches.status.as_ref().map(|s| s.tables.iter().map(|t| t.repo.clone()).collect::<Vec<_>>()).unwrap_or_default();
    std::iter::once(crate::i18n::tr("All repositories").to_owned()).chain(repos).collect()
}
