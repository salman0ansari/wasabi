//! Additive Wasabi-owned account tables. These live in the protocol database
//! but use its shared writer permit, so there is still exactly one SQLite WAL
//! writer and one connection pool per account.

use diesel::connection::Connection as _;
use diesel::prelude::*;
use wacore::store::error::StoreError;
use whatsapp_rust_sqlite_storage::SharedSqlite;

const SCHEMA_VERSION: i32 = 2;

const CREATE_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS wasabi_schema_version (version INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS wasabi_chat_preferences (
        device_id INTEGER NOT NULL,
        chat_jid TEXT NOT NULL,
        favorite INTEGER NOT NULL DEFAULT 0,
        draft_json TEXT,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (device_id, chat_jid)
    )",
    "CREATE TABLE IF NOT EXISTS wasabi_contact_cache (
        device_id INTEGER NOT NULL,
        jid TEXT NOT NULL,
        display_name TEXT,
        about TEXT,
        avatar_ref TEXT,
        fetched_at_ms INTEGER NOT NULL,
        PRIMARY KEY (device_id, jid)
    )",
    "CREATE TABLE IF NOT EXISTS wasabi_group_cache (
        device_id INTEGER NOT NULL,
        chat_jid TEXT NOT NULL,
        subject TEXT NOT NULL,
        description TEXT,
        avatar_ref TEXT,
        permissions_json TEXT NOT NULL,
        participant_count INTEGER NOT NULL,
        fetched_at_ms INTEGER NOT NULL,
        PRIMARY KEY (device_id, chat_jid)
    )",
    "CREATE TABLE IF NOT EXISTS wasabi_group_participants (
        device_id INTEGER NOT NULL,
        chat_jid TEXT NOT NULL,
        participant_jid TEXT NOT NULL,
        display_name TEXT NOT NULL,
        avatar_ref TEXT,
        role INTEGER NOT NULL,
        is_self INTEGER NOT NULL DEFAULT 0,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (device_id, chat_jid, participant_jid)
    )",
    "CREATE INDEX IF NOT EXISTS wasabi_group_participants_chat
        ON wasabi_group_participants (device_id, chat_jid, role, display_name)",
    "CREATE TABLE IF NOT EXISTS wasabi_transfer_jobs (
        device_id INTEGER NOT NULL,
        transfer_id TEXT NOT NULL,
        chat_jid TEXT NOT NULL,
        message_id TEXT,
        direction INTEGER NOT NULL,
        state INTEGER NOT NULL,
        source_path TEXT,
        destination_path TEXT,
        media_hash TEXT,
        payload_json TEXT,
        bytes_done INTEGER NOT NULL DEFAULT 0,
        bytes_total INTEGER,
        error_kind TEXT,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY (device_id, transfer_id)
    )",
    "CREATE INDEX IF NOT EXISTS wasabi_transfer_jobs_state
        ON wasabi_transfer_jobs (device_id, state, updated_at_ms)",
];

pub async fn migrate(shared: SharedSqlite) -> Result<(), StoreError> {
    shared
        .run(|connection| {
            connection
                .transaction::<_, MigrationError, _>(|connection| {
                    for statement in CREATE_STATEMENTS {
                        diesel::sql_query(*statement).execute(connection)?;
                    }
                    if !table_has_column(connection, "wasabi_transfer_jobs", "payload_json")? {
                        diesel::sql_query(
                            "ALTER TABLE wasabi_transfer_jobs ADD COLUMN payload_json TEXT",
                        )
                        .execute(connection)?;
                    }
                    diesel::sql_query("DELETE FROM wasabi_schema_version").execute(connection)?;
                    diesel::sql_query("INSERT INTO wasabi_schema_version (version) VALUES (?)")
                        .bind::<diesel::sql_types::Integer, _>(SCHEMA_VERSION)
                        .execute(connection)?;
                    Ok(())
                })
                .map_err(MigrationError::into_store_error)
        })
        .await
}

#[derive(QueryableByName)]
struct ColumnRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

fn table_has_column(
    connection: &mut diesel::SqliteConnection,
    table: &str,
    column: &str,
) -> Result<bool, diesel::result::Error> {
    // Both names are compile-time constants at every call site. Keeping the
    // helper private avoids turning PRAGMA construction into a query surface.
    let rows =
        diesel::sql_query(format!("PRAGMA table_info('{table}')")).load::<ColumnRow>(connection)?;
    Ok(rows.into_iter().any(|row| row.name == column))
}

#[derive(Debug)]
struct MigrationError(diesel::result::Error);

impl From<diesel::result::Error> for MigrationError {
    fn from(error: diesel::result::Error) -> Self {
        Self(error)
    }
}

impl MigrationError {
    fn into_store_error(self) -> StoreError {
        StoreError::Database(Box::new(self.0))
    }
}

#[cfg(test)]
pub(crate) const EXPECTED_TABLES: &[&str] = &[
    "wasabi_schema_version",
    "wasabi_chat_preferences",
    "wasabi_contact_cache",
    "wasabi_group_cache",
    "wasabi_group_participants",
    "wasabi_transfer_jobs",
];

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use whatsapp_rust_sqlite_storage::{SqliteStore, SqliteStoreConfig};

    #[derive(QueryableByName)]
    struct NameRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
    }

    #[derive(QueryableByName)]
    struct VersionRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        version: i32,
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migration_is_additive_and_idempotent() {
        let directory = TempDir::new().unwrap();
        let url = format!("sqlite://{}", directory.path().join("account.db").display());
        let sqlite = SqliteStore::with_config(&url, SqliteStoreConfig::default())
            .await
            .unwrap();

        migrate(sqlite.shared()).await.unwrap();
        migrate(sqlite.shared()).await.unwrap();

        let (mut tables, version): (Vec<String>, i32) = sqlite
            .shared()
            .read(|connection| {
                let tables = diesel::sql_query(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'wasabi_%'",
                )
                .load::<NameRow>(connection)
                .map_err(|error| StoreError::Database(Box::new(error)))?
                .into_iter()
                .map(|row| row.name)
                .collect();
                let version = diesel::sql_query("SELECT version FROM wasabi_schema_version")
                    .get_result::<VersionRow>(connection)
                    .map_err(|error| StoreError::Database(Box::new(error)))?
                    .version;
                Ok((tables, version))
            })
            .await
            .unwrap();
        tables.sort();
        let mut expected = EXPECTED_TABLES
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(tables, expected);
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn version_one_transfer_rows_survive_payload_column_upgrade() {
        let directory = TempDir::new().unwrap();
        let url = format!("sqlite://{}", directory.path().join("old.db").display());
        let sqlite = SqliteStore::with_config(&url, SqliteStoreConfig::default())
            .await
            .unwrap();
        sqlite
            .shared()
            .run(|connection| {
                diesel::sql_query(
                    "CREATE TABLE wasabi_transfer_jobs (
                        device_id INTEGER NOT NULL,
                        transfer_id TEXT NOT NULL,
                        chat_jid TEXT NOT NULL,
                        message_id TEXT,
                        direction INTEGER NOT NULL,
                        state INTEGER NOT NULL,
                        source_path TEXT,
                        destination_path TEXT,
                        media_hash TEXT,
                        bytes_done INTEGER NOT NULL DEFAULT 0,
                        bytes_total INTEGER,
                        error_kind TEXT,
                        updated_at_ms INTEGER NOT NULL,
                        PRIMARY KEY (device_id, transfer_id)
                    )",
                )
                .execute(connection)
                .map_err(|error| StoreError::Database(Box::new(error)))?;
                diesel::sql_query(
                    "INSERT INTO wasabi_transfer_jobs (
                        device_id, transfer_id, chat_jid, direction, state,
                        bytes_done, updated_at_ms
                    ) VALUES (1, 'legacy', 'chat@s.whatsapp.net', 1, 0, 0, 1)",
                )
                .execute(connection)
                .map_err(|error| StoreError::Database(Box::new(error)))?;
                Ok(())
            })
            .await
            .unwrap();

        migrate(sqlite.shared()).await.unwrap();
        let (has_payload, count): (bool, i64) = sqlite
            .shared()
            .read(|connection| {
                #[derive(QueryableByName)]
                struct CountRow {
                    #[diesel(sql_type = diesel::sql_types::BigInt)]
                    count: i64,
                }
                let count = diesel::sql_query(
                    "SELECT COUNT(*) AS count FROM wasabi_transfer_jobs WHERE transfer_id = 'legacy'",
                )
                .get_result::<CountRow>(connection)
                .map_err(|error| StoreError::Database(Box::new(error)))?
                .count;
                Ok((
                    table_has_column(connection, "wasabi_transfer_jobs", "payload_json")
                        .map_err(|error| StoreError::Database(Box::new(error)))?,
                    count,
                ))
            })
            .await
            .unwrap();
        assert!(has_payload);
        assert_eq!(count, 1);
    }
}
