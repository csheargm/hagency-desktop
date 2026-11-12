//! A private SQLite temporary database owns turn-local document state. SQLite
//! deletes it on close; only its bounded page cache and one extraction live in
//! memory. Reading a long room does not exhaust a lifetime document allowance.
use super::*;
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::{Mutex, MutexGuard};

#[derive(Default)]
pub(super) struct Index {
    db: Option<Mutex<Connection>>,
}
pub(super) struct Entry {
    pub event: String,
    pub offset: usize,
    pub read: bool,
}
impl Index {
    fn db(&mut self) -> Result<MutexGuard<'_, Connection>, NativeError> {
        if self.db.is_none() {
            // An empty name is SQLite's private, automatically removed file,
            // not an in-memory database or a caller-supplied filesystem path.
            let db = Connection::open("").map_err(|_| invalid())?;
            db.execute_batch(
                "PRAGMA cache_size=-512; PRAGMA journal_mode=MEMORY;
                CREATE TABLE documents(id INTEGER PRIMARY KEY,event TEXT NOT NULL UNIQUE,
                metadata TEXT NOT NULL,extraction TEXT,offset INTEGER NOT NULL DEFAULT 0,
                read INTEGER NOT NULL DEFAULT 0);",
            )
            .map_err(|_| invalid())?;
            self.db = Some(Mutex::new(db));
        }
        self.current()
    }
    fn current(&self) -> Result<MutexGuard<'_, Connection>, NativeError> {
        self.db
            .as_ref()
            .ok_or_else(invalid)?
            .lock()
            .map_err(|_| invalid())
    }
    pub fn describe(&mut self, event: &str, metadata: &Value) -> Result<usize, NativeError> {
        let metadata = metadata.to_string();
        if event.len() > 1024 || metadata.len() > 4096 {
            return Err(invalid());
        }
        let db = self.db()?;
        if let Some(id) = db
            .query_row("SELECT id FROM documents WHERE event=?1", [event], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|_| invalid())?
        {
            return Ok(id);
        }
        db.execute("INSERT INTO documents(id,event,metadata) SELECT coalesce(max(id),-1)+1,?1,?2 FROM documents",
            params![event,metadata]).map_err(|_| invalid())?;
        usize::try_from(db.last_insert_rowid()).map_err(|_| invalid())
    }
    pub fn contains(&self, event: &str) -> bool {
        self.current().is_ok_and(|db| {
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM documents WHERE event=?1)",
                [event],
                |r| r.get(0),
            )
            .unwrap_or(false)
        })
    }
    pub fn list(&self, after: usize, limit: usize) -> Result<Value, NativeError> {
        let Some(db) = self.db.as_ref() else {
            return Ok(json!({"files":[],"next_after":null,"scope":"observed_room_history"}));
        };
        let db = db.lock().map_err(|_| invalid())?;
        let mut query = db
            .prepare("SELECT id,event,metadata FROM documents WHERE id>=?1 ORDER BY id LIMIT ?2")
            .map_err(|_| invalid())?;
        let rows = query
            .query_map(params![after, limit.min(100)], |r| {
                Ok((
                    r.get::<_, usize>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|_| invalid())?;
        let mut files = Vec::new();
        let mut bytes = 0;
        let mut next = after;
        for row in rows {
            let (id, event, raw) = row.map_err(|_| invalid())?;
            let metadata: Value = serde_json::from_str(&raw).map_err(|_| invalid())?;
            let item = json!({"event_id":event,"metadata":metadata});
            bytes += item.to_string().len();
            if bytes > 8 * 1024 {
                break;
            }
            files.push(item);
            next = id.checked_add(1).ok_or_else(invalid)?;
        }
        let more: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM documents WHERE id>=?1)",
                [next],
                |r| r.get(0),
            )
            .map_err(|_| invalid())?;
        Ok(
            json!({"files":files,"next_after":more.then_some(next),"scope":"observed_room_history",
            "guidance":"Read earlier pages with read_room_history to discover older attachments."}),
        )
    }
    pub fn entry(&self, id: usize) -> Result<Entry, NativeError> {
        self.current()?
            .query_row(
                "SELECT event,offset,read FROM documents WHERE id=?1",
                [id],
                |r| {
                    Ok(Entry {
                        event: r.get(0)?,
                        offset: r.get(1)?,
                        read: r.get(2)?,
                    })
                },
            )
            .map_err(|_| invalid())
    }
    pub fn extraction(&self, id: usize) -> Result<Option<Result<Text, String>>, NativeError> {
        let raw: Option<String> = self
            .current()?
            .query_row("SELECT extraction FROM documents WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .map_err(|_| invalid())?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(|_| invalid()))
            .transpose()
    }
    pub fn save(&mut self, id: usize, text: &Result<Text, String>) -> Result<(), NativeError> {
        let raw = serde_json::to_string(text).map_err(|_| invalid())?;
        let n = self
            .db()?
            .execute(
                "UPDATE documents SET extraction=?2 WHERE id=?1",
                params![id, raw],
            )
            .map_err(|_| invalid())?;
        if n != 1 {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn advance(&mut self, id: usize, offset: usize, read: bool) -> Result<(), NativeError> {
        let n = self
            .db()?
            .execute(
                "UPDATE documents SET offset=?2,read=?3 WHERE id=?1",
                params![id, offset, read],
            )
            .map_err(|_| invalid())?;
        if n != 1 {
            return Err(invalid());
        }
        Ok(())
    }
}
