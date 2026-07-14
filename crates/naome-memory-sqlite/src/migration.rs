use rusqlite::{Connection, OptionalExtension};

use crate::{Result, SCHEMA_VERSION, StoreError};

const INITIAL_SCHEMA: &str = include_str!("../migrations/0001_initial.sql");

pub fn migrate(connection: &mut Connection) -> Result<()> {
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if application_id != 0 && application_id != application_id_value() {
        return Err(StoreError::Integrity(format!(
            "unexpected SQLite application_id {application_id}"
        )));
    }

    let current: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema {
            found: current,
            expected: SCHEMA_VERSION,
        });
    }

    if current == 0 {
        let already_initialized = connection
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_metadata'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if already_initialized {
            return Err(StoreError::Integrity(
                "schema tables exist while PRAGMA user_version is zero".into(),
            ));
        }

        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(INITIAL_SCHEMA)?;
        transaction.pragma_update(None, "application_id", application_id_value())?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
    }

    let persisted: String = connection.query_row(
        "SELECT value FROM schema_metadata WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    )?;
    if persisted != SCHEMA_VERSION.to_string() {
        return Err(StoreError::Integrity(format!(
            "schema_metadata reports version {persisted}"
        )));
    }
    Ok(())
}

const fn application_id_value() -> i64 {
    // ASCII "NMM1". Application IDs are advisory but catch accidental DB reuse.
    0x4e4d_4d31
}
