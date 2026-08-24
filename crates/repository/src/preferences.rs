//! Durable device-local chat preferences.

use std::collections::HashMap;

use diesel::prelude::*;
use wacore::store::error::StoreError;
use wasabi_domain::{ChatId, Draft, ErrorKind, ServiceError};
use whatsapp_rust_sqlite_storage::SharedSqlite;

diesel::table! {
    wasabi_chat_preferences (device_id, chat_jid) {
        device_id -> Integer,
        chat_jid -> Text,
        favorite -> Integer,
        draft_json -> Nullable<Text>,
        updated_at_ms -> BigInt,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatPreference {
    pub favorite: bool,
    pub draft: Option<Draft>,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = wasabi_chat_preferences)]
struct PreferenceRow {
    chat_jid: String,
    favorite: i32,
    draft_json: Option<String>,
}

pub(crate) async fn load_for_chats(
    shared: SharedSqlite,
    device_id: i32,
    chats: Vec<String>,
) -> Result<HashMap<String, ChatPreference>, ServiceError> {
    if chats.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = shared
        .read(move |connection| {
            use self::wasabi_chat_preferences::dsl;
            dsl::wasabi_chat_preferences
                .filter(
                    dsl::device_id
                        .eq(device_id)
                        .and(dsl::chat_jid.eq_any(chats)),
                )
                .select(PreferenceRow::as_select())
                .load(connection)
                .map_err(database_error)
        })
        .await
        .map_err(service_error)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let draft = row
                .draft_json
                .and_then(|json| serde_json::from_str(&json).ok());
            (
                row.chat_jid,
                ChatPreference {
                    favorite: row.favorite != 0,
                    draft,
                },
            )
        })
        .collect())
}

pub async fn load(
    shared: SharedSqlite,
    device_id: i32,
    chat: ChatId,
) -> Result<ChatPreference, ServiceError> {
    Ok(
        load_for_chats(shared, device_id, vec![chat.as_str().to_string()])
            .await?
            .remove(chat.as_str())
            .unwrap_or_default(),
    )
}

pub async fn set_favorite(
    shared: SharedSqlite,
    device_id: i32,
    chat: ChatId,
    favorite: bool,
) -> Result<(), ServiceError> {
    let chat = chat.as_str().to_string();
    let favorite = i32::from(favorite);
    let updated_at_ms = chrono::Utc::now().timestamp_millis();
    shared
        .run(move |connection| {
            use self::wasabi_chat_preferences::dsl;
            diesel::insert_into(dsl::wasabi_chat_preferences)
                .values((
                    dsl::device_id.eq(device_id),
                    dsl::chat_jid.eq(chat),
                    dsl::favorite.eq(favorite),
                    dsl::draft_json.eq::<Option<String>>(None),
                    dsl::updated_at_ms.eq(updated_at_ms),
                ))
                .on_conflict((dsl::device_id, dsl::chat_jid))
                .do_update()
                .set((
                    dsl::favorite.eq(favorite),
                    dsl::updated_at_ms.eq(updated_at_ms),
                ))
                .execute(connection)
                .map(|_| ())
                .map_err(database_error)
        })
        .await
        .map_err(service_error)
}

pub async fn save_draft(
    shared: SharedSqlite,
    device_id: i32,
    chat: ChatId,
    draft: Option<Draft>,
) -> Result<(), ServiceError> {
    let chat = chat.as_str().to_string();
    let draft_json = draft
        .map(|draft| serde_json::to_string(&draft))
        .transpose()
        .map_err(|error| ServiceError::new(ErrorKind::Internal, error.to_string()))?;
    let updated_at_ms = chrono::Utc::now().timestamp_millis();
    shared
        .run(move |connection| {
            use self::wasabi_chat_preferences::dsl;
            diesel::insert_into(dsl::wasabi_chat_preferences)
                .values((
                    dsl::device_id.eq(device_id),
                    dsl::chat_jid.eq(chat),
                    dsl::favorite.eq(0),
                    dsl::draft_json.eq(draft_json.clone()),
                    dsl::updated_at_ms.eq(updated_at_ms),
                ))
                .on_conflict((dsl::device_id, dsl::chat_jid))
                .do_update()
                .set((
                    dsl::draft_json.eq(draft_json),
                    dsl::updated_at_ms.eq(updated_at_ms),
                ))
                .execute(connection)
                .map(|_| ())
                .map_err(database_error)
        })
        .await
        .map_err(service_error)
}

fn database_error(error: diesel::result::Error) -> StoreError {
    StoreError::Database(Box::new(error))
}

fn service_error(error: StoreError) -> ServiceError {
    ServiceError::new(ErrorKind::Database, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use whatsapp_rust_sqlite_storage::{SqliteStore, SqliteStoreConfig};

    async fn fixture() -> (TempDir, SqliteStore) {
        let directory = TempDir::new().unwrap();
        let url = format!("sqlite://{}", directory.path().join("prefs.db").display());
        let sqlite = SqliteStore::with_config(&url, SqliteStoreConfig::default())
            .await
            .unwrap();
        crate::wasabi_schema::migrate(sqlite.shared())
            .await
            .unwrap();
        (directory, sqlite)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn favorite_and_draft_update_independently() {
        let (_directory, sqlite) = fixture().await;
        let chat = ChatId::new("123@s.whatsapp.net");
        set_favorite(sqlite.shared(), 7, chat.clone(), true)
            .await
            .unwrap();
        save_draft(
            sqlite.shared(),
            7,
            chat.clone(),
            Some(Draft {
                body: "unfinished reply".to_string(),
                ..Draft::default()
            }),
        )
        .await
        .unwrap();

        let preference = load(sqlite.shared(), 7, chat.clone()).await.unwrap();
        assert!(preference.favorite);
        assert_eq!(preference.draft.unwrap().body, "unfinished reply");

        save_draft(sqlite.shared(), 7, chat.clone(), None)
            .await
            .unwrap();
        let preference = load(sqlite.shared(), 7, chat).await.unwrap();
        assert!(preference.favorite, "clearing a draft preserves favorite");
        assert!(preference.draft.is_none());
    }
}
