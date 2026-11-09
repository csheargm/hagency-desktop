//! Original tool replies persist for a turn in a private temporary SQLite file.
//! Replays recheck the original proposal; they never prepare or execute again.
use super::*;
use rusqlite::{Connection, OptionalExtension};
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct Cache(Option<Mutex<Connection>>);
impl Cache {
    pub fn get(&self, call: &str) -> Result<Option<(String, ToolProposal, Value)>> {
        let Some(db) = &self.0 else { return Ok(None) };
        let db = db.lock().map_err(|_| Error::Protocol("host reply cache"))?;
        let raw: Option<String> = db
            .query_row("SELECT reply FROM replies WHERE call=?", [call], |r| {
                r.get(0)
            })
            .optional()
            .map_err(crate::Error::from)?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(|_| Error::Protocol("host reply cache")))
            .transpose()
    }
    pub fn insert(
        &mut self,
        call: &str,
        digest: &str,
        proposal: &ToolProposal,
        result: &Value,
    ) -> Result<()> {
        if self.0.is_none() {
            let db = Connection::open("").map_err(crate::Error::from)?;
            db.execute_batch(
                "PRAGMA cache_size=-512; PRAGMA journal_mode=MEMORY;
                CREATE TABLE replies(call TEXT PRIMARY KEY, reply TEXT NOT NULL);",
            )
            .map_err(crate::Error::from)?;
            self.0 = Some(Mutex::new(db));
        }
        let raw = serde_json::to_string(&(digest, proposal, result))
            .map_err(|_| Error::Protocol("host reply cache"))?;
        self.0
            .as_ref()
            .unwrap()
            .lock()
            .map_err(|_| Error::Protocol("host reply cache"))?
            .execute("INSERT INTO replies VALUES(?,?)", (call, raw))
            .map_err(crate::Error::from)?;
        Ok(())
    }
}
