//! Deterministic, keyset-paginated contact queries over the shared account DB.

use std::collections::HashMap;

use diesel::prelude::*;
use diesel::sql_types::{BigInt, Integer, Nullable, Text};
use wacore::store::error::StoreError;
use wasabi_domain::{
    AvatarRef, ChatId, ContactPage, ContactPageCursor, ContactSummary, ErrorKind, ServiceError,
};
use whatsapp_rust::Jid;
use whatsapp_rust_sqlite_storage::SharedSqlite;

const MAX_CONTACT_PAGE: usize = 200;

diesel::table! {
    wasabi_contact_cache (device_id, jid) {
        device_id -> Integer,
        jid -> Text,
        avatar_ref -> Nullable<Text>,
    }
}

const CONTACT_PAGE_SQL: &str = r#"
WITH canonical AS (
    SELECT
        CASE
            WHEN m.phone_number IS NOT NULL THEN m.phone_number || '@s.whatsapp.net'
            ELSE c.jid
        END AS jid,
        c.full_name,
        c.first_name,
        c.push_name,
        c.business_name,
        COALESCE(w_raw.display_name, w_canonical.display_name) AS cached_name,
        COALESCE(w_raw.avatar_ref, w_canonical.avatar_ref) AS avatar_ref
    FROM contacts c
    LEFT JOIN lid_pn_mapping m
      ON m.device_id = c.device_id AND c.jid = m.lid || '@lid'
    LEFT JOIN wasabi_contact_cache w_raw
      ON w_raw.device_id = c.device_id AND w_raw.jid = c.jid
    LEFT JOIN wasabi_contact_cache w_canonical
      ON w_canonical.device_id = c.device_id
     AND w_canonical.jid = CASE
            WHEN m.phone_number IS NOT NULL THEN m.phone_number || '@s.whatsapp.net'
            ELSE c.jid
         END
    WHERE c.device_id = ?
      AND (c.jid LIKE '%@s.whatsapp.net' OR c.jid LIKE '%@lid')
), merged AS (
    SELECT
        jid,
        COALESCE(
            MAX(NULLIF(TRIM(full_name), '')),
            MAX(NULLIF(TRIM(first_name), '')),
            MAX(NULLIF(TRIM(push_name), '')),
            MAX(NULLIF(TRIM(business_name), '')),
            MAX(NULLIF(TRIM(cached_name), '')),
            jid
        ) AS display_name,
        MAX(avatar_ref) AS avatar_ref
    FROM canonical
    GROUP BY jid
), projected AS (
    SELECT jid, display_name, LOWER(display_name) AS sort_name, avatar_ref
    FROM merged
)
SELECT jid, display_name, sort_name, avatar_ref
FROM projected
WHERE (LOWER(display_name) LIKE LOWER(?) ESCAPE '\' OR LOWER(jid) LIKE LOWER(?) ESCAPE '\')
  AND (? = 0 OR sort_name > ? OR (sort_name = ? AND jid > ?))
ORDER BY sort_name ASC, jid ASC
LIMIT ?
"#;

#[derive(QueryableByName)]
struct ContactRow {
    #[diesel(sql_type = Text)]
    jid: String,
    #[diesel(sql_type = Text)]
    display_name: String,
    #[diesel(sql_type = Text)]
    sort_name: String,
    #[diesel(sql_type = Nullable<Text>)]
    avatar_ref: Option<String>,
}

#[derive(QueryableByName)]
pub(crate) struct CachedContactMetadata {
    #[diesel(sql_type = Nullable<Text>)]
    pub display_name: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub about: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub avatar_ref: Option<String>,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = wasabi_contact_cache)]
struct AvatarRefRow {
    jid: String,
    avatar_ref: Option<String>,
}

pub(crate) async fn load_avatar_refs(
    shared: SharedSqlite,
    device_id: i32,
    jids: Vec<String>,
) -> Result<HashMap<String, AvatarRef>, ServiceError> {
    if jids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = shared
        .read(move |connection| {
            use self::wasabi_contact_cache::dsl;
            dsl::wasabi_contact_cache
                .filter(dsl::device_id.eq(device_id).and(dsl::jid.eq_any(jids)))
                .select(AvatarRefRow::as_select())
                .load(connection)
                .map_err(|error| StoreError::Database(Box::new(error)))
        })
        .await
        .map_err(database_error)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.avatar_ref
                .filter(|value| !value.is_empty())
                .map(|avatar| (row.jid, AvatarRef(avatar)))
        })
        .collect())
}

pub(crate) async fn load_metadata(
    shared: SharedSqlite,
    device_id: i32,
    jid: String,
) -> Result<Option<CachedContactMetadata>, ServiceError> {
    shared
        .read(move |connection| {
            diesel::sql_query(
                "SELECT display_name, about, avatar_ref
                 FROM wasabi_contact_cache
                 WHERE device_id = ? AND jid = ?",
            )
            .bind::<Integer, _>(device_id)
            .bind::<Text, _>(jid)
            .get_result::<CachedContactMetadata>(connection)
            .optional()
            .map_err(|error| StoreError::Database(Box::new(error)))
        })
        .await
        .map_err(database_error)
}

pub(crate) async fn save_metadata(
    shared: SharedSqlite,
    device_id: i32,
    jid: String,
    display_name: Option<String>,
    about: Option<String>,
    avatar_ref: Option<String>,
) -> Result<(), ServiceError> {
    let fetched_at_ms = chrono::Utc::now().timestamp_millis();
    shared
        .run(move |connection| {
            diesel::sql_query(
                "INSERT INTO wasabi_contact_cache
                    (device_id, jid, display_name, about, avatar_ref, fetched_at_ms)
                 VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(device_id, jid) DO UPDATE SET
                    display_name = excluded.display_name,
                    about = excluded.about,
                    avatar_ref = excluded.avatar_ref,
                    fetched_at_ms = excluded.fetched_at_ms",
            )
            .bind::<Integer, _>(device_id)
            .bind::<Text, _>(jid)
            .bind::<Nullable<Text>, _>(display_name)
            .bind::<Nullable<Text>, _>(about)
            .bind::<Nullable<Text>, _>(avatar_ref)
            .bind::<BigInt, _>(fetched_at_ms)
            .execute(connection)
            .map(|_| ())
            .map_err(|error| StoreError::Database(Box::new(error)))
        })
        .await
        .map_err(database_error)
}

pub async fn page(
    shared: SharedSqlite,
    device_id: i32,
    query: String,
    after: Option<ContactPageCursor>,
    limit: usize,
) -> Result<ContactPage, ServiceError> {
    let limit = limit.clamp(1, MAX_CONTACT_PAGE);
    let query = query.trim().chars().take(256).collect::<String>();
    let pattern = format!("%{}%", escape_like(&query));
    let (cursor_enabled, cursor_name, cursor_jid) = after.map_or_else(
        || (0, String::new(), String::new()),
        |after| (1, after.sort_name, after.jid.as_str().to_string()),
    );
    let fetch_limit = i64::try_from(limit.saturating_add(1)).unwrap_or(MAX_CONTACT_PAGE as i64 + 1);
    let rows = shared
        .read(move |connection| {
            diesel::sql_query(CONTACT_PAGE_SQL)
                .bind::<Integer, _>(device_id)
                .bind::<Text, _>(&pattern)
                .bind::<Text, _>(&pattern)
                .bind::<Integer, _>(cursor_enabled)
                .bind::<Text, _>(&cursor_name)
                .bind::<Text, _>(&cursor_name)
                .bind::<Text, _>(&cursor_jid)
                .bind::<diesel::sql_types::BigInt, _>(fetch_limit)
                .load::<ContactRow>(connection)
                .map_err(|error| StoreError::Database(Box::new(error)))
        })
        .await
        .map_err(database_error)?;

    let has_more = rows.len() > limit;
    let mut rows = rows.into_iter().take(limit).collect::<Vec<_>>();
    let next_after = has_more.then(|| {
        let last = rows.last().expect("a full contact page has a final row");
        ContactPageCursor {
            sort_name: last.sort_name.clone(),
            jid: ChatId::new(last.jid.clone()),
        }
    });
    let rows = rows
        .drain(..)
        .map(project_contact)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ContactPage { rows, next_after })
}

fn project_contact(row: ContactRow) -> Result<ContactSummary, ServiceError> {
    let jid = row
        .jid
        .parse::<Jid>()
        .map_err(|_| ServiceError::new(ErrorKind::Database, "invalid cached contact identity"))?;
    let display_name = if row.display_name == row.jid {
        jid.user.to_string()
    } else {
        row.display_name
    };
    Ok(ContactSummary {
        jid: ChatId::new(row.jid),
        display_name,
        phone_number: jid.is_pn().then(|| jid.user.to_string()),
        avatar: row.avatar_ref.map(AvatarRef),
    })
}

fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn database_error(error: StoreError) -> ServiceError {
    ServiceError::new(ErrorKind::Database, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{escape_like, load_metadata, save_metadata};
    use tempfile::TempDir;
    use whatsapp_rust_sqlite_storage::{SqliteStore, SqliteStoreConfig};

    #[test]
    fn search_literals_do_not_become_sql_wildcards() {
        assert_eq!(escape_like(r"a_b%c\d"), r"a\_b\%c\\d");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contact_metadata_cache_persists_and_clears_authoritative_fields() {
        let directory = TempDir::new().expect("tempdir");
        let url = format!("sqlite://{}", directory.path().join("account.db").display());
        let sqlite = SqliteStore::with_config(&url, SqliteStoreConfig::default())
            .await
            .expect("open sqlite store");
        crate::wasabi_schema::migrate(sqlite.shared())
            .await
            .expect("migrate");

        save_metadata(
            sqlite.shared(),
            7,
            "15550000001@s.whatsapp.net".to_string(),
            Some("Alice".to_string()),
            Some("Available".to_string()),
            Some("picture-1".to_string()),
        )
        .await
        .expect("save metadata");
        let cached = load_metadata(sqlite.shared(), 7, "15550000001@s.whatsapp.net".to_string())
            .await
            .expect("load metadata")
            .expect("cached row");
        assert_eq!(cached.display_name.as_deref(), Some("Alice"));
        assert_eq!(cached.about.as_deref(), Some("Available"));
        assert_eq!(cached.avatar_ref.as_deref(), Some("picture-1"));

        save_metadata(
            sqlite.shared(),
            7,
            "15550000001@s.whatsapp.net".to_string(),
            Some("Alice Updated".to_string()),
            None,
            None,
        )
        .await
        .expect("overwrite metadata");
        let cached = load_metadata(sqlite.shared(), 7, "15550000001@s.whatsapp.net".to_string())
            .await
            .expect("load updated metadata")
            .expect("cached row");
        assert_eq!(cached.display_name.as_deref(), Some("Alice Updated"));
        assert_eq!(cached.about, None);
        assert_eq!(cached.avatar_ref, None);
    }
}
