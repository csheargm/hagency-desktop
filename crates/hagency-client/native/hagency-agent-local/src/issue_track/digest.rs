//! The daily digest's ledger side (ADR 0028): when each tracked record last
//! moved (`changed_at`, so one day's digest counts that day's moves), and the
//! last local day a digest was posted per watched repository, so the desktop
//! posts at most one a day for it, across app and guardian restarts.
//!
//! The digest log keeps what one record alone cannot (integration GPT
//! review): every move of a record as its own step, so a move before the
//! cutoff still counts in its own day after a later one, and when each
//! hand-off's approval card went unanswered, kept apart from the bounded
//! Activity view and timed by the card, not the request's receipt.
use super::{State, Track};
use crate::{key, Error, Ledger, Result};
use serde::Serialize;
use std::collections::BTreeMap;

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS issue_digest_sent (agent TEXT NOT NULL, repo TEXT NOT NULL, day TEXT NOT NULL, PRIMARY KEY(agent,repo)) STRICT; \
CREATE TABLE IF NOT EXISTS issue_track_step (agent TEXT NOT NULL, repo TEXT NOT NULL, number INTEGER NOT NULL, state TEXT NOT NULL, pr INTEGER, notified INTEGER NOT NULL, at INTEGER NOT NULL) STRICT; \
CREATE INDEX IF NOT EXISTS issue_track_step_at ON issue_track_step(agent, at); \
CREATE TABLE IF NOT EXISTS issue_digest_card (dispatch TEXT PRIMARY KEY, agent TEXT NOT NULL, repo TEXT NOT NULL, number INTEGER NOT NULL, requester TEXT NOT NULL, at INTEGER NOT NULL) STRICT;";

/// How long the digest log keeps a step or card: a week of daily digests
/// and a day's grace for a late one.
pub const LOG_RETAIN_SECS: i64 = 8 * 24 * 60 * 60;

/// One move of a record: its state right after `at` (Unix seconds).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackStep {
    pub repo: String,
    pub number: i64,
    pub state: State,
    pub pr: Option<u64>,
    pub notified: bool,
    pub at: i64,
}

/// A hand-off's approval card nobody answered, by when it went unanswered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnansweredCard {
    pub repo: String,
    pub number: i64,
    /// Who sent the hand-off, so a digest counts its owner's own only.
    pub requester: String,
    pub at: i64,
}

/// The digest log of one Agent within [`LOG_RETAIN_SECS`], oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct DigestLog {
    pub steps: Vec<TrackStep>,
    pub cards: Vec<UnansweredCard>,
}

/// Keep `track`'s move as a step when this change moved it (`changed_at`
/// stamped now and the record differs), and drop steps past retention.
pub(crate) fn record_step(db: &rusqlite::Connection, agent: &str, before: Option<&Track>, track: &Track, now: i64) -> Result<()> {
    if track.changed_at != now || before == Some(track) {
        return Ok(());
    }
    db.execute(
        "INSERT INTO issue_track_step (agent, repo, number, state, pr, notified, at) VALUES (?,?,?,?,?,?,?)",
        (agent, &track.repo, track.number, track.state.wire(), track.pr.map(|p| p as i64), track.notified as i64, now),
    )?;
    db.execute("DELETE FROM issue_track_step WHERE at < ?", [now - LOG_RETAIN_SECS])?;
    Ok(())
}

/// Note that hand-off `dispatch`'s approval card went unanswered at `at`;
/// the first such card of a hand-off is the one kept. Not a hand-off: nothing.
pub(crate) fn note_card(db: &rusqlite::Connection, dispatch: &str, at: i64) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO issue_digest_card (dispatch, agent, repo, number, requester, at) SELECT h.dispatch, b.agent, h.repo, h.number, \
         coalesce(json_extract(i.envelope,'$.requesterMxid'),''), ? FROM handoff_issues h JOIN local_inbox i ON i.id=h.dispatch JOIN bindings b ON b.id=i.binding WHERE h.dispatch=?",
        (at, dispatch),
    )?;
    db.execute("DELETE FROM issue_digest_card WHERE at < ?", [at - LOG_RETAIN_SECS])?;
    Ok(())
}

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
    /// `agent`'s digest log (see [`DigestLog`]).
    pub fn digest_log(&self, agent: &str) -> Result<DigestLog> {
        let mut statement = self.db.prepare("SELECT repo, number, state, pr, notified, at FROM issue_track_step WHERE agent=? ORDER BY at, rowid")?;
        let rows = statement.query_map([agent], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, i64>(4)?, r.get(5)?)))?;
        let steps = rows
            .map(|row| {
                let (repo, number, state, pr, notified, at) = row?;
                Ok(TrackStep { repo, number, state: State::parse(&state)?, pr: pr.map(|p| p as u64), notified: notified != 0, at })
            })
            .collect::<Result<_>>()?;
        let mut statement = self.db.prepare("SELECT repo, number, requester, at FROM issue_digest_card WHERE agent=? ORDER BY at, dispatch")?;
        let rows = statement.query_map([agent], |r| Ok(UnansweredCard { repo: r.get(0)?, number: r.get(1)?, requester: r.get(2)?, at: r.get(3)? }))?;
        Ok(DigestLog { steps, cards: rows.collect::<rusqlite::Result<_>>()? })
    }

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

    fn change(value: serde_json::Value) -> super::super::TrackChange {
        serde_json::from_value(value).unwrap()
    }

    /// Integration GPT review: reviewed 17:50, merged 18:02, digest at 18:05 —
    /// the record alone says only "merged at 18:02"; the log keeps 17:50.
    #[test]
    fn every_move_is_kept_as_its_own_step_and_a_replay_adds_none() {
        let (_t, mut l) = ledger();
        l.track_issue("a", &change(serde_json::json!({"repo": "acme/app", "number": 7, "handed": 1})), 100).unwrap();
        l.track_issue("a", &change(serde_json::json!({"repo": "acme/app", "number": 7, "state": "reviewed", "pr": 9})), 200).unwrap();
        l.track_issue("a", &change(serde_json::json!({"repo": "acme/app", "number": 7, "state": "reviewed", "pr": 9})), 250).unwrap();
        l.track_issue("a", &change(serde_json::json!({"repo": "acme/app", "number": 7, "state": "merged", "notified": true})), 300).unwrap();
        let steps = l.digest_log("a").unwrap().steps;
        let got: Vec<(State, Option<u64>, i64)> = steps.iter().map(|s| (s.state, s.pr, s.at)).collect();
        assert_eq!(got, [(State::HandedOff, None, 100), (State::Reviewed, Some(9), 200), (State::Merged, Some(9), 300)]);
        assert!(steps[2].notified && !steps[1].notified);
        assert!(l.digest_log("b").unwrap().steps.is_empty(), "per Agent");
        // Older than the retention: dropped on the next move.
        l.track_issue("a", &change(serde_json::json!({"repo": "acme/app", "number": 8, "handed": 1})), 200 + LOG_RETAIN_SECS).unwrap();
        let left: Vec<i64> = l.digest_log("a").unwrap().steps.iter().map(|s| s.at).collect();
        assert_eq!(left, [200, 300, 200 + LOG_RETAIN_SECS]);
    }

    fn hand_off(l: &mut Ledger, id: &str, body: &str, at: i64) {
        let d = crate::inbox::Dispatch {
            id: id.into(), binding_id: format!("bind-{id}"), agent_id: "a".into(), event_id: format!("${id}:t"), room_id: "!r:t".into(),
            requester_mxid: "@alice:example.org".into(), thread_root: "$root:t".into(), body: body.into(), state: "offered".into(),
            binding_generation: 1, dispatch_epoch: Some(1), dispatch_device_id: Some("dev".into()), execution_id: None, outcome: None,
        };
        l.register_binding("@alice:example.org", &d.scope()).unwrap();
        l.receive_dispatch("@alice:example.org", &d, crate::inbox::Limits::default(), at).unwrap();
    }

    /// Integration GPT review: a card is timed by when it went unanswered,
    /// and stays in the log however many requests came after it.
    #[test]
    fn an_unanswered_card_is_kept_by_its_own_time_past_the_activity_view() {
        let (_t, mut l) = ledger();
        hand_off(&mut l, "early", "🔔 New GitHub issue #69 in acme/app — please fix it", 10);
        for n in 0..25 {
            hand_off(&mut l, &format!("later-{n}"), "plain request", 20 + n);
        }
        note_card(&l.db, "early", 500).unwrap();
        note_card(&l.db, "early", 600).unwrap(); // a second card of the same hand-off
        note_card(&l.db, "later-0", 600).unwrap(); // not a hand-off
        let cards = l.digest_log("a").unwrap().cards;
        assert_eq!(cards, [UnansweredCard { repo: "acme/app".into(), number: 69, requester: "@alice:example.org".into(), at: 500 }]);
        assert!(l.recent_activity("@alice:example.org", "a", 20).unwrap().iter().all(|r| r.dispatch_id != "early"), "the bounded view lost it");
    }

    #[test]
    fn the_live_unanswered_mark_also_logs_the_card() {
        let (_t, mut l) = ledger();
        hand_off(&mut l, "d", "🔔 New GitHub issue #5 in acme/app — please fix it", 10);
        l.note_unanswered_approval("d").unwrap();
        let cards = l.digest_log("a").unwrap().cards;
        assert_eq!(cards.len(), 1);
        assert!(cards[0].at > 1_700_000_000, "timed by the clock, not the request");
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
