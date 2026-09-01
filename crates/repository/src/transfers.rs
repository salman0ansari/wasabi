//! Durable media transfer jobs.
//!
//! Jobs share the account database writer with protocol state. The repository
//! persists only a redacted error class and never logs or exposes local paths.

use std::path::{Path, PathBuf};

use diesel::Connection as _;
use diesel::prelude::*;
use wacore::store::error::StoreError;
use wasabi_domain::{
    ChatId, ErrorKind, MessageId, ServiceError, TransferDirection, TransferId, TransferJob,
    TransferState,
};
use whatsapp_rust_sqlite_storage::SharedSqlite;

diesel::table! {
    wasabi_transfer_jobs (device_id, transfer_id) {
        device_id -> Integer,
        transfer_id -> Text,
        chat_jid -> Text,
        message_id -> Nullable<Text>,
        direction -> Integer,
        state -> Integer,
        source_path -> Nullable<Binary>,
        destination_path -> Nullable<Binary>,
        media_hash -> Nullable<Text>,
        payload_json -> Nullable<Text>,
        bytes_done -> BigInt,
        bytes_total -> Nullable<BigInt>,
        error_kind -> Nullable<Text>,
        updated_at_ms -> BigInt,
    }
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = wasabi_transfer_jobs)]
struct TransferRow {
    transfer_id: String,
    chat_jid: String,
    message_id: Option<String>,
    direction: i32,
    state: i32,
    source_path: Option<Vec<u8>>,
    destination_path: Option<Vec<u8>>,
    media_hash: Option<String>,
    payload_json: Option<String>,
    bytes_done: i64,
    bytes_total: Option<i64>,
    error_kind: Option<String>,
    updated_at_ms: i64,
}

pub async fn save(
    shared: SharedSqlite,
    device_id: i32,
    mut job: TransferJob,
) -> Result<(), ServiceError> {
    validate(&job)?;
    job.updated_at_ms = chrono::Utc::now().timestamp_millis();
    let transfer_id = job.transfer.as_str().to_string();
    let chat_jid = job.chat.as_str().to_string();
    let message_id = job.message.map(|id| id.as_str().to_string());
    let direction = direction_code(job.direction);
    let state = state_code(job.state);
    let source_path = job.source_path.as_deref().map(path_bytes);
    let destination_path = job.destination_path.as_deref().map(path_bytes);
    let bytes_done = as_i64(job.bytes_done, "bytes_done")?;
    let bytes_total = job
        .bytes_total
        .map(|value| as_i64(value, "bytes_total"))
        .transpose()?;
    let error_kind = job.error_kind.map(|kind| kind.to_string());
    let payload_json = job
        .payload
        .map(|payload| serde_json::to_string(&payload))
        .transpose()
        .map_err(|error| ServiceError::new(ErrorKind::Internal, error.to_string()))?;
    let updated_at_ms = job.updated_at_ms;

    let inserted = shared
        .run(move |connection| {
            use self::wasabi_transfer_jobs::dsl;
            diesel::insert_into(dsl::wasabi_transfer_jobs)
                .values((
                    dsl::device_id.eq(device_id),
                    dsl::transfer_id.eq(transfer_id),
                    dsl::chat_jid.eq(chat_jid),
                    dsl::message_id.eq(message_id),
                    dsl::direction.eq(direction),
                    dsl::state.eq(state),
                    dsl::source_path.eq(source_path),
                    dsl::destination_path.eq(destination_path),
                    dsl::media_hash.eq(job.media_hash),
                    dsl::payload_json.eq(payload_json),
                    dsl::bytes_done.eq(bytes_done),
                    dsl::bytes_total.eq(bytes_total),
                    dsl::error_kind.eq(error_kind),
                    dsl::updated_at_ms.eq(updated_at_ms),
                ))
                .on_conflict((dsl::device_id, dsl::transfer_id))
                .do_nothing()
                .execute(connection)
                .map(|changed| changed == 1)
                .map_err(database_error)
        })
        .await
        .map_err(service_error)?;
    if inserted {
        Ok(())
    } else {
        Err(ServiceError::new(
            ErrorKind::InvalidRequest,
            "transfer identity already exists",
        ))
    }
}

/// Load jobs oldest-first so restart recovery is deterministic. Terminal jobs
/// are excluded by default but may be included for Storage/diagnostics UI.
pub async fn load(
    shared: SharedSqlite,
    device_id: i32,
    include_terminal: bool,
) -> Result<Vec<TransferJob>, ServiceError> {
    let rows = shared
        .read(move |connection| {
            use self::wasabi_transfer_jobs::dsl;
            let mut query = dsl::wasabi_transfer_jobs
                .filter(dsl::device_id.eq(device_id))
                .into_boxed();
            if !include_terminal {
                query = query.filter(dsl::state.ne_all([
                    state_code(TransferState::Succeeded),
                    state_code(TransferState::FailedPermanent),
                    state_code(TransferState::Cancelled),
                ]));
            }
            query
                .order((dsl::updated_at_ms.asc(), dsl::transfer_id.asc()))
                .select(TransferRow::as_select())
                .load(connection)
                .map_err(database_error)
        })
        .await
        .map_err(service_error)?;
    rows.into_iter().map(row_to_job).collect()
}

pub async fn load_one(
    shared: SharedSqlite,
    device_id: i32,
    transfer: TransferId,
) -> Result<Option<TransferJob>, ServiceError> {
    let transfer = transfer.as_str().to_string();
    let row = shared
        .read(move |connection| {
            use self::wasabi_transfer_jobs::dsl;
            dsl::wasabi_transfer_jobs
                .filter(
                    dsl::device_id
                        .eq(device_id)
                        .and(dsl::transfer_id.eq(transfer)),
                )
                .select(TransferRow::as_select())
                .first(connection)
                .optional()
                .map_err(database_error)
        })
        .await
        .map_err(service_error)?;
    row.map(row_to_job).transpose()
}

/// Replace restart-critical attachment metadata only while a job is active.
/// A stale composer callback cannot rewrite a terminal transfer.
pub async fn update_payload(
    shared: SharedSqlite,
    device_id: i32,
    transfer: TransferId,
    payload: wasabi_domain::TransferPayload,
) -> Result<bool, ServiceError> {
    let transfer = transfer.as_str().to_string();
    let payload_json = serde_json::to_string(&payload)
        .map_err(|error| ServiceError::new(ErrorKind::Internal, error.to_string()))?;
    let updated_at_ms = chrono::Utc::now().timestamp_millis();
    shared
        .run(move |connection| {
            use self::wasabi_transfer_jobs::dsl;
            diesel::update(
                dsl::wasabi_transfer_jobs.filter(
                    dsl::device_id
                        .eq(device_id)
                        .and(dsl::transfer_id.eq(transfer))
                        .and(dsl::state.ne_all([
                            state_code(TransferState::Succeeded),
                            state_code(TransferState::FailedPermanent),
                            state_code(TransferState::Cancelled),
                        ])),
                ),
            )
            .set((
                dsl::payload_json.eq(payload_json),
                dsl::updated_at_ms.eq(updated_at_ms),
            ))
            .execute(connection)
            .map(|changed| changed != 0)
            .map_err(database_error)
        })
        .await
        .map_err(service_error)
}

/// Advance byte progress without allowing a stale callback to decrease it or
/// resurrect a terminal job.
pub async fn update_progress(
    shared: SharedSqlite,
    device_id: i32,
    transfer: TransferId,
    bytes_done: u64,
    bytes_total: Option<u64>,
) -> Result<bool, ServiceError> {
    if bytes_total.is_some_and(|total| bytes_done > total) {
        return Err(ServiceError::new(
            ErrorKind::InvalidRequest,
            "transfer progress exceeds total",
        ));
    }
    let transfer = transfer.as_str().to_string();
    let bytes_done = as_i64(bytes_done, "bytes_done")?;
    let bytes_total = bytes_total
        .map(|value| as_i64(value, "bytes_total"))
        .transpose()?;
    let updated_at_ms = chrono::Utc::now().timestamp_millis();
    shared
        .run(move |connection| {
            use self::wasabi_transfer_jobs::dsl;
            connection
                .transaction::<_, diesel::result::Error, _>(|connection| {
                    let current = dsl::wasabi_transfer_jobs
                        .filter(
                            dsl::device_id
                                .eq(device_id)
                                .and(dsl::transfer_id.eq(&transfer))
                                .and(dsl::state.eq_any([
                                    state_code(TransferState::Queued),
                                    state_code(TransferState::Running),
                                ])),
                        )
                        .select(dsl::bytes_done)
                        .first::<i64>(connection)
                        .optional()?;
                    let Some(current) = current else {
                        return Ok(false);
                    };
                    diesel::update(
                        dsl::wasabi_transfer_jobs.filter(
                            dsl::device_id
                                .eq(device_id)
                                .and(dsl::transfer_id.eq(transfer)),
                        ),
                    )
                    .set((
                        dsl::state.eq(state_code(TransferState::Running)),
                        dsl::bytes_done.eq(current.max(bytes_done)),
                        dsl::bytes_total.eq(bytes_total),
                        dsl::error_kind.eq::<Option<String>>(None),
                        dsl::updated_at_ms.eq(updated_at_ms),
                    ))
                    .execute(connection)?;
                    Ok(true)
                })
                .map_err(database_error)
        })
        .await
        .map_err(service_error)
}

pub async fn set_state(
    shared: SharedSqlite,
    device_id: i32,
    transfer: TransferId,
    state: TransferState,
    error_kind: Option<ErrorKind>,
) -> Result<bool, ServiceError> {
    validate_state_error(state, error_kind)?;
    let transfer = transfer.as_str().to_string();
    let state_code_value = state_code(state);
    let allowed_from = allowed_previous_states(state);
    let error_kind = error_kind.map(|kind| kind.to_string());
    let updated_at_ms = chrono::Utc::now().timestamp_millis();
    shared
        .run(move |connection| {
            use self::wasabi_transfer_jobs::dsl;
            diesel::update(
                dsl::wasabi_transfer_jobs.filter(
                    dsl::device_id
                        .eq(device_id)
                        .and(dsl::transfer_id.eq(transfer))
                        .and(dsl::state.eq_any(allowed_from)),
                ),
            )
            .set((
                dsl::state.eq(state_code_value),
                dsl::error_kind.eq(error_kind),
                dsl::updated_at_ms.eq(updated_at_ms),
            ))
            .execute(connection)
            .map(|changed| changed != 0)
            .map_err(database_error)
        })
        .await
        .map_err(service_error)
}

/// Remove only terminal jobs. Active payloads cannot disappear due to a stale
/// cleanup request.
pub async fn remove_terminal(
    shared: SharedSqlite,
    device_id: i32,
    transfer: TransferId,
) -> Result<bool, ServiceError> {
    let transfer = transfer.as_str().to_string();
    shared
        .run(move |connection| {
            use self::wasabi_transfer_jobs::dsl;
            diesel::delete(
                dsl::wasabi_transfer_jobs.filter(
                    dsl::device_id
                        .eq(device_id)
                        .and(dsl::transfer_id.eq(transfer))
                        .and(dsl::state.eq_any([
                            state_code(TransferState::Succeeded),
                            state_code(TransferState::FailedPermanent),
                            state_code(TransferState::Cancelled),
                        ])),
                ),
            )
            .execute(connection)
            .map(|changed| changed != 0)
            .map_err(database_error)
        })
        .await
        .map_err(service_error)
}

fn validate(job: &TransferJob) -> Result<(), ServiceError> {
    if job.transfer.as_str().is_empty() || job.chat.as_str().is_empty() {
        return Err(ServiceError::new(
            ErrorKind::InvalidRequest,
            "transfer and chat identities are required",
        ));
    }
    if job.bytes_total.is_some_and(|total| job.bytes_done > total) {
        return Err(ServiceError::new(
            ErrorKind::InvalidRequest,
            "transfer progress exceeds total",
        ));
    }
    validate_state_error(job.state, job.error_kind)
}

fn validate_state_error(
    state: TransferState,
    error_kind: Option<ErrorKind>,
) -> Result<(), ServiceError> {
    let failed = matches!(
        state,
        TransferState::FailedRetryable | TransferState::FailedPermanent
    );
    if failed != error_kind.is_some() {
        return Err(ServiceError::new(
            ErrorKind::InvalidRequest,
            "only failed transfer states require an error kind",
        ));
    }
    Ok(())
}

fn direction_code(direction: TransferDirection) -> i32 {
    match direction {
        TransferDirection::IncomingDownload => 0,
        TransferDirection::OutgoingUpload => 1,
    }
}

fn parse_direction(value: i32) -> Result<TransferDirection, ServiceError> {
    match value {
        0 => Ok(TransferDirection::IncomingDownload),
        1 => Ok(TransferDirection::OutgoingUpload),
        _ => Err(corrupt("transfer direction")),
    }
}

fn state_code(state: TransferState) -> i32 {
    match state {
        TransferState::Staged => 0,
        TransferState::Queued => 1,
        TransferState::Running => 2,
        TransferState::Succeeded => 3,
        TransferState::FailedRetryable => 4,
        TransferState::FailedPermanent => 5,
        TransferState::Cancelled => 6,
    }
}

fn allowed_previous_states(target: TransferState) -> Vec<i32> {
    let mut allowed = match target {
        TransferState::Staged => Vec::new(),
        TransferState::Queued => vec![TransferState::Staged, TransferState::FailedRetryable],
        TransferState::Running => vec![TransferState::Queued],
        TransferState::Succeeded => vec![TransferState::Running],
        TransferState::FailedRetryable => vec![TransferState::Queued, TransferState::Running],
        TransferState::FailedPermanent | TransferState::Cancelled => vec![
            TransferState::Staged,
            TransferState::Queued,
            TransferState::Running,
            TransferState::FailedRetryable,
        ],
    };
    allowed.push(target);
    allowed.into_iter().map(state_code).collect()
}

fn parse_state(value: i32) -> Result<TransferState, ServiceError> {
    match value {
        0 => Ok(TransferState::Staged),
        1 => Ok(TransferState::Queued),
        2 => Ok(TransferState::Running),
        3 => Ok(TransferState::Succeeded),
        4 => Ok(TransferState::FailedRetryable),
        5 => Ok(TransferState::FailedPermanent),
        6 => Ok(TransferState::Cancelled),
        _ => Err(corrupt("transfer state")),
    }
}

fn parse_error_kind(value: &str) -> Result<ErrorKind, ServiceError> {
    match value {
        "NotConnected" => Ok(ErrorKind::NotConnected),
        "NotPaired" => Ok(ErrorKind::NotPaired),
        "InvalidRequest" => Ok(ErrorKind::InvalidRequest),
        "Database" => Ok(ErrorKind::Database),
        "StorageBusy" => Ok(ErrorKind::StorageBusy),
        "MediaUnavailable" => Ok(ErrorKind::MediaUnavailable),
        "Timeout" => Ok(ErrorKind::Timeout),
        "Cancelled" => Ok(ErrorKind::Cancelled),
        "Protocol" => Ok(ErrorKind::Protocol),
        "RateLimited" => Ok(ErrorKind::RateLimited),
        "Overloaded" => Ok(ErrorKind::Overloaded),
        "Unsupported" => Ok(ErrorKind::Unsupported),
        "Internal" => Ok(ErrorKind::Internal),
        _ => Err(corrupt("transfer error kind")),
    }
}

fn row_to_job(row: TransferRow) -> Result<TransferJob, ServiceError> {
    let bytes_done = u64::try_from(row.bytes_done).map_err(|_| corrupt("transfer bytes_done"))?;
    let bytes_total = row
        .bytes_total
        .map(|value| u64::try_from(value).map_err(|_| corrupt("transfer bytes_total")))
        .transpose()?;
    Ok(TransferJob {
        transfer: TransferId::new(row.transfer_id),
        chat: ChatId::new(row.chat_jid),
        message: row.message_id.map(MessageId::new),
        direction: parse_direction(row.direction)?,
        state: parse_state(row.state)?,
        source_path: row.source_path.map(path_from_bytes),
        destination_path: row.destination_path.map(path_from_bytes),
        media_hash: row.media_hash,
        payload: row
            .payload_json
            .map(|json| serde_json::from_str(&json).map_err(|_| corrupt("transfer payload")))
            .transpose()?,
        bytes_done,
        bytes_total,
        error_kind: row
            .error_kind
            .as_deref()
            .map(parse_error_kind)
            .transpose()?,
        updated_at_ms: row.updated_at_ms,
    })
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

fn as_i64(value: u64, field: &'static str) -> Result<i64, ServiceError> {
    i64::try_from(value).map_err(|_| {
        ServiceError::new(
            ErrorKind::InvalidRequest,
            format!("{field} exceeds SQLite range"),
        )
    })
}

fn corrupt(field: &'static str) -> ServiceError {
    ServiceError::new(ErrorKind::Database, format!("corrupt {field}"))
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
        let url = format!(
            "sqlite://{}",
            directory.path().join("transfers.db").display()
        );
        let sqlite = SqliteStore::with_config(&url, SqliteStoreConfig::default())
            .await
            .unwrap();
        crate::wasabi_schema::migrate(sqlite.shared())
            .await
            .unwrap();
        (directory, sqlite)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn transfer_survives_reopen_and_progress_is_monotonic() {
        let (directory, sqlite) = fixture().await;
        let mut job = TransferJob::staged_upload(
            TransferId::new("upload-a"),
            ChatId::new("group-a@g.us"),
            directory.path().join(std::ffi::OsString::from("photo.jpg")),
            100,
        );
        job.payload = Some(wasabi_domain::TransferPayload {
            kind: wasabi_domain::AttachmentKind::Image,
            display_name: "Holiday photo.jpg".to_string(),
            mime_type: "image/jpeg".to_string(),
            caption: Some("A real caption".to_string()),
            voice_note: false,
            duration_seconds: None,
        });
        save(sqlite.shared(), 7, job).await.unwrap();
        set_state(
            sqlite.shared(),
            7,
            TransferId::new("upload-a"),
            TransferState::Queued,
            None,
        )
        .await
        .unwrap();
        assert!(
            update_progress(
                sqlite.shared(),
                7,
                TransferId::new("upload-a"),
                60,
                Some(100)
            )
            .await
            .unwrap()
        );
        update_progress(
            sqlite.shared(),
            7,
            TransferId::new("upload-a"),
            20,
            Some(100),
        )
        .await
        .unwrap();

        let jobs = load(sqlite.shared(), 7, false).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].bytes_done, 60);
        assert_eq!(jobs[0].state, TransferState::Running);
        assert_eq!(jobs[0].bytes_total, Some(100));
        drop(sqlite);

        let url = format!(
            "sqlite://{}",
            directory.path().join("transfers.db").display()
        );
        let reopened = SqliteStore::with_config(&url, SqliteStoreConfig::default())
            .await
            .unwrap();
        let jobs = load(reopened.shared(), 7, false).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].transfer.as_str(), "upload-a");
        assert_eq!(
            jobs[0]
                .payload
                .as_ref()
                .map(|payload| payload.display_name.as_str()),
            Some("Holiday photo.jpg")
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn non_utf8_source_path_round_trips_without_loss() {
        use std::os::unix::ffi::OsStringExt as _;

        let (_directory, sqlite) = fixture().await;
        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![
            b'/', b't', b'm', b'p', b'/', 0xff,
        ]));
        let job = TransferJob::staged_upload(
            TransferId::new("upload-path"),
            ChatId::new("chat@s.whatsapp.net"),
            path.clone(),
            1,
        );
        save(sqlite.shared(), 3, job).await.unwrap();
        let loaded = load(sqlite.shared(), 3, false).await.unwrap();
        assert_eq!(loaded[0].source_path.as_ref(), Some(&path));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failure_is_classified_and_only_terminal_jobs_are_removed() {
        let (_directory, sqlite) = fixture().await;
        let job = TransferJob::staged_upload(
            TransferId::new("upload-b"),
            ChatId::new("chat@s.whatsapp.net"),
            PathBuf::from("/tmp/document.pdf"),
            5,
        );
        save(sqlite.shared(), 9, job).await.unwrap();
        assert!(
            !remove_terminal(sqlite.shared(), 9, TransferId::new("upload-b"))
                .await
                .unwrap()
        );
        assert!(
            set_state(
                sqlite.shared(),
                9,
                TransferId::new("upload-b"),
                TransferState::Queued,
                None,
            )
            .await
            .unwrap()
        );
        assert!(
            set_state(
                sqlite.shared(),
                9,
                TransferId::new("upload-b"),
                TransferState::FailedRetryable,
                Some(ErrorKind::NotConnected),
            )
            .await
            .unwrap()
        );
        let active = load(sqlite.shared(), 9, false).await.unwrap();
        assert_eq!(active[0].error_kind, Some(ErrorKind::NotConnected));
        assert!(
            !remove_terminal(sqlite.shared(), 9, TransferId::new("upload-b"))
                .await
                .unwrap()
        );
        set_state(
            sqlite.shared(),
            9,
            TransferId::new("upload-b"),
            TransferState::FailedPermanent,
            Some(ErrorKind::InvalidRequest),
        )
        .await
        .unwrap();
        assert!(load(sqlite.shared(), 9, false).await.unwrap().is_empty());
        assert_eq!(load(sqlite.shared(), 9, true).await.unwrap().len(), 1);
        assert!(
            remove_terminal(sqlite.shared(), 9, TransferId::new("upload-b"))
                .await
                .unwrap()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn duplicate_creation_and_terminal_resurrection_are_refused() {
        let (_directory, sqlite) = fixture().await;
        let job = TransferJob::staged_upload(
            TransferId::new("upload-immutable"),
            ChatId::new("chat@s.whatsapp.net"),
            PathBuf::from("/tmp/original"),
            5,
        );
        save(sqlite.shared(), 1, job.clone()).await.unwrap();
        let duplicate = save(sqlite.shared(), 1, job).await.unwrap_err();
        assert_eq!(duplicate.kind, ErrorKind::InvalidRequest);
        assert!(
            set_state(
                sqlite.shared(),
                1,
                TransferId::new("upload-immutable"),
                TransferState::Cancelled,
                None,
            )
            .await
            .unwrap()
        );
        assert!(
            !set_state(
                sqlite.shared(),
                1,
                TransferId::new("upload-immutable"),
                TransferState::Queued,
                None,
            )
            .await
            .unwrap()
        );
        assert!(
            !update_progress(
                sqlite.shared(),
                1,
                TransferId::new("upload-immutable"),
                1,
                Some(5),
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_job_lookup_and_payload_update_are_terminal_safe() {
        let (_directory, sqlite) = fixture().await;
        let mut job = TransferJob::staged_upload(
            TransferId::new("upload-caption"),
            ChatId::new("chat@s.whatsapp.net"),
            PathBuf::from("/tmp/photo.jpg"),
            5,
        );
        let payload = wasabi_domain::TransferPayload {
            kind: wasabi_domain::AttachmentKind::Image,
            display_name: "photo.jpg".to_string(),
            mime_type: "image/jpeg".to_string(),
            caption: None,
            voice_note: false,
            duration_seconds: None,
        };
        job.payload = Some(payload.clone());
        save(sqlite.shared(), 4, job).await.unwrap();
        let loaded = load_one(sqlite.shared(), 4, TransferId::new("upload-caption"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.payload, Some(payload.clone()));

        let mut captioned = payload;
        captioned.caption = Some("A durable caption".to_string());
        assert!(
            update_payload(
                sqlite.shared(),
                4,
                TransferId::new("upload-caption"),
                captioned.clone(),
            )
            .await
            .unwrap()
        );
        assert_eq!(
            load_one(sqlite.shared(), 4, TransferId::new("upload-caption"))
                .await
                .unwrap()
                .unwrap()
                .payload,
            Some(captioned.clone())
        );
        set_state(
            sqlite.shared(),
            4,
            TransferId::new("upload-caption"),
            TransferState::Cancelled,
            None,
        )
        .await
        .unwrap();
        captioned.caption = Some("stale rewrite".to_string());
        assert!(
            !update_payload(
                sqlite.shared(),
                4,
                TransferId::new("upload-caption"),
                captioned,
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_progress_and_error_combinations_are_rejected() {
        let (_directory, sqlite) = fixture().await;
        let mut job = TransferJob::staged_upload(
            TransferId::new("upload-c"),
            ChatId::new("chat@s.whatsapp.net"),
            PathBuf::from("/tmp/a"),
            5,
        );
        job.bytes_done = 6;
        assert_eq!(
            save(sqlite.shared(), 1, job).await.unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        assert_eq!(
            set_state(
                sqlite.shared(),
                1,
                TransferId::new("missing"),
                TransferState::Succeeded,
                Some(ErrorKind::Protocol),
            )
            .await
            .unwrap_err()
            .kind,
            ErrorKind::InvalidRequest
        );
    }
}
