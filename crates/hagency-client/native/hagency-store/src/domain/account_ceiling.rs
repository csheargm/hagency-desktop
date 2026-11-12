//! Account limits are explicit owner declarations. Resource enrollment leaves
//! the account declaration unset and never derives it from a resource ceiling.
use super::*;

pub(crate) fn declaration(
    db: &Connection,
    seat_id: &str,
) -> Result<Option<allocation::Declaration>, Error> {
    let raw: Option<String> = db
        .query_row("SELECT config FROM seats WHERE id=?1", [seat_id], |r| r.get(0))
        .optional()?;
    let stored = raw.map(|s| serde_json::from_str::<Seat>(&s)).transpose()?;
    Ok(stored.and_then(|s| s.declaration))
}

pub(crate) fn shared(db: &Connection, seat_id: &str) -> Result<bool, Error> {
    Ok(declaration(db, seat_id)?.is_some_and(|d| d.quota_tokens.is_some()))
}
