//! Structural checks for the daemon's current SQLite formats. Inspection never
//! adds columns, creates missing tables, or upgrades an existing file.
use rusqlite::Connection;

pub(super) fn has_tables(db: &Connection, tables: &[(&str, &[&str])]) -> rusqlite::Result<bool> {
    let mut objects = db.prepare(
        "SELECT type, name FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let objects = objects
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if objects.len() != tables.len() {
        return Ok(false);
    }
    for ((kind, name), (expected_name, columns)) in objects.iter().zip(tables) {
        if kind != "table" || name != expected_name {
            return Ok(false);
        }
        let mut statement = db.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?;
        let actual = statement
            .query_map([name], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if actual
            .iter()
            .map(String::as_str)
            .ne(columns.iter().copied())
        {
            return Ok(false);
        }
    }
    Ok(true)
}
