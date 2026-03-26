use std::path::Path;

use refinery::embed_migrations;
use rusqlite::Connection;

embed_migrations!("src/migrations");

/// Open (or create) the SQLite database and run pending migrations.
pub fn open_db(path: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    migrations::runner().run(&mut conn)?;

    Ok(conn)
}
