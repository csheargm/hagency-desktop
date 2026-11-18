//! The daily digest's ledger side (ADR 0028): when each tracked record last
//! moved (`changed_at`, so one day's digest counts that day's moves), and the
//! last local day a digest was posted per watched repository, so the desktop
//! posts at most one a day for it, across app and guardian restarts.
use crate::{key, Error, Ledger, Result};
use std::collections::BTreeMap;

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS issue_digest_sent (agent TEXT NOT NULL, repo TEXT NOT NULL, day TEXT NOT NULL, PRIMARY KEY(agent,repo)) STRICT;";

/// The digest table, and `changed_at` on a tracker ledger created before it.
pub(crate) fn ensure_schema(db: &rusqlite::Connection) -> Result<()> {
    db.execute_batch(SCHEMA)?;
    let has: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('issue_track') WHERE name='changed_at')", [], |r| r.get(0))?;
    if !has {
        db.execute_batch("ALTER TABLE issue_track ADD COLUMN changed_at INTEGER NOT NULL DEFAULT 0")?;
    }
    Ok(())
}

/// A local calendar day as `YYYY-MM-DD` (the desktop's clock decides which).
pub fn valid_day(day: &str) -> bool {
    let bytes = day.as_bytes();
    bytes.len() == 10 && bytes.iter().enumerate().all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() })
}

impl Ledger {
    /// The last day a digest was posted, per repository (lowercase).
    pub fn digest_days(&self, agent: &str) -> Result<BTreeMap<String, String>> {
        let mut statement = self.db.prepare("SELECT repo, day FROM issue_digest_sent WHERE agent=?")?;
        let rows = statement.query_map([agent], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record that `day`'s digest for `repo` was posted. Never moves back:
    /// a replayed or late write for an older day changes nothing.
    pub fn record_digest_sent(&mut self, agent: &str, repo: &str, day: &str) -> Result<()> {
        key(agent)?;
        if !crate::issue_watch::valid_repo(repo) || !valid_day(day) {
            return Err(Error::Invalid("a digest needs owner/name and a YYYY-MM-DD day"));
        }
        self.db.execute(
            "INSERT INTO issue_digest_sent (agent, repo, day) VALUES (?,?,?) ON CONFLICT(agent,repo) DO UPDATE SET day=excluded.day WHERE excluded.day > issue_digest_sent.day",
            (agent, repo.to_ascii_lowercase(), day),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let tmp = tempfile::tempdir().unwrap();
        let l = Ledger::open(tmp.path().join("local.db"), "@alice:example.org").unwrap();
        (tmp, l)
    }

    #[test]
    fn the_last_sent_day_survives_reopening_and_never_moves_back() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("local.db");
        {
            let mut l = Ledger::open(&path, "@alice:example.org").unwrap();
            l.record_digest_sent("a", "Acme/App", "2026-11-18").unwrap();
            l.record_digest_sent("a", "acme/app", "2026-11-17").unwrap();
        }
        let l = Ledger::open(&path, "@alice:example.org").unwrap();
        assert_eq!(l.digest_days("a").unwrap().get("acme/app").map(String::as_str), Some("2026-11-18"));
        assert!(l.digest_days("b").unwrap().is_empty(), "per Agent");
    }

    #[test]
    fn a_bad_repository_or_day_is_refused() {
        let (_t, mut l) = ledger();
        for (repo, day) in [("acme", "2026-11-18"), ("acme/app", "2026-1-18"), ("acme/app", "18-11-2026"), ("acme/app", "2026-11-18;")] {
            assert!(l.record_digest_sent("a", repo, day).is_err(), "{repo} {day}");
        }
        assert!(l.digest_days("a").unwrap().is_empty());
    }

    #[test]
    fn an_older_ledger_gains_changed_at() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(super::super::SCHEMA).unwrap();
        db.execute("INSERT INTO issue_track VALUES ('a','acme/app',1,'handed_off',1,0,NULL,0)", []).unwrap();
        super::super::ensure_schema(&db).unwrap();
        super::super::ensure_schema(&db).unwrap();
        let at: i64 = db.query_row("SELECT changed_at FROM issue_track", [], |r| r.get(0)).unwrap();
        assert_eq!(at, 0);
    }
}
