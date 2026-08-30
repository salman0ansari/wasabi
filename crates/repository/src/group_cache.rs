//! Durable, account-scoped group metadata cache.
//!
//! Live metadata remains authoritative. This cache exists so the information
//! drawer can show the last truthful snapshot while the companion reconnects.

use diesel::connection::Connection as _;
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Integer, Nullable, Text};
use wacore::store::error::StoreError;
use wasabi_domain::{
    AvatarRef, ChatId, ErrorKind, GroupDetails, GroupPermissions, Participant, ParticipantRole,
    ServiceError,
};
use whatsapp_rust::wacore_binary::JidExt as _;
use whatsapp_rust_sqlite_storage::SharedSqlite;

#[derive(QueryableByName)]
struct GroupRow {
    #[diesel(sql_type = Text)]
    chat_jid: String,
    #[diesel(sql_type = Text)]
    subject: String,
    #[diesel(sql_type = Nullable<Text>)]
    description: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    avatar_ref: Option<String>,
    #[diesel(sql_type = Text)]
    permissions_json: String,
    #[diesel(sql_type = BigInt)]
    participant_count: i64,
}

#[derive(QueryableByName)]
struct ParticipantRow {
    #[diesel(sql_type = Text)]
    participant_jid: String,
    #[diesel(sql_type = Text)]
    display_name: String,
    #[diesel(sql_type = Nullable<Text>)]
    avatar_ref: Option<String>,
    #[diesel(sql_type = Integer)]
    role: i32,
    #[diesel(sql_type = Integer)]
    is_self: i32,
}

pub async fn save(
    shared: SharedSqlite,
    device_id: i32,
    details: GroupDetails,
    fetched_at_ms: i64,
) -> Result<(), ServiceError> {
    validate_group_identity(details.chat.as_str())?;
    let subject = details.subject.trim().to_string();
    if subject.is_empty() {
        return Err(ServiceError::new(
            ErrorKind::InvalidRequest,
            "group cache subject is empty",
        ));
    }
    for participant in &details.participants {
        validate_participant_identity(&participant.jid)?;
    }
    let permissions_json = serde_json::to_string(&details.permissions).map_err(cache_error)?;
    let participant_count = i64::try_from(details.participant_count)
        .map_err(|_| ServiceError::new(ErrorKind::InvalidRequest, "participant count overflow"))?;
    let chat = details.chat.as_str().to_string();
    let description = details.description;
    let avatar_ref = details.avatar.map(|avatar| avatar.0);
    let participants = details.participants;

    shared
        .run(move |connection| {
            connection
                .transaction::<_, diesel::result::Error, _>(|connection| {
                    diesel::sql_query(
                        "INSERT INTO wasabi_group_cache
                     (device_id, chat_jid, subject, description, avatar_ref,
                      permissions_json, participant_count, fetched_at_ms)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(device_id, chat_jid) DO UPDATE SET
                       subject = excluded.subject,
                       description = excluded.description,
                       avatar_ref = excluded.avatar_ref,
                       permissions_json = excluded.permissions_json,
                       participant_count = excluded.participant_count,
                       fetched_at_ms = excluded.fetched_at_ms",
                    )
                    .bind::<Integer, _>(device_id)
                    .bind::<Text, _>(&chat)
                    .bind::<Text, _>(&subject)
                    .bind::<Nullable<Text>, _>(description)
                    .bind::<Nullable<Text>, _>(avatar_ref)
                    .bind::<Text, _>(permissions_json)
                    .bind::<BigInt, _>(participant_count)
                    .bind::<BigInt, _>(fetched_at_ms.max(0))
                    .execute(connection)?;

                    diesel::sql_query(
                        "DELETE FROM wasabi_group_participants
                     WHERE device_id = ? AND chat_jid = ?",
                    )
                    .bind::<Integer, _>(device_id)
                    .bind::<Text, _>(&chat)
                    .execute(connection)?;

                    for participant in participants {
                        diesel::sql_query(
                            "INSERT INTO wasabi_group_participants
                         (device_id, chat_jid, participant_jid, display_name,
                          avatar_ref, role, is_self, updated_at_ms)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                        )
                        .bind::<Integer, _>(device_id)
                        .bind::<Text, _>(&chat)
                        .bind::<Text, _>(participant.jid)
                        .bind::<Text, _>(participant.display_name)
                        .bind::<Nullable<Text>, _>(participant.avatar.map(|avatar| avatar.0))
                        .bind::<Integer, _>(role_to_i32(participant.role))
                        .bind::<Integer, _>(i32::from(participant.is_self))
                        .bind::<BigInt, _>(fetched_at_ms.max(0))
                        .execute(connection)?;
                    }
                    Ok(())
                })
                .map_err(database_store_error)
        })
        .await
        .map_err(store_error)
}

pub async fn load(
    shared: SharedSqlite,
    device_id: i32,
    chat: String,
) -> Result<Option<GroupDetails>, ServiceError> {
    validate_group_identity(&chat)?;
    let result = shared
        .read(move |connection| {
            let header = diesel::sql_query(
                "SELECT chat_jid, subject, description, avatar_ref,
                        permissions_json, participant_count
                 FROM wasabi_group_cache
                 WHERE device_id = ? AND chat_jid = ?",
            )
            .bind::<Integer, _>(device_id)
            .bind::<Text, _>(&chat)
            .get_result::<GroupRow>(connection)
            .optional()
            .map_err(database_store_error)?;
            let Some(header) = header else {
                return Ok(None);
            };
            let participants = diesel::sql_query(
                "SELECT participant_jid, display_name, avatar_ref, role, is_self
                 FROM wasabi_group_participants
                 WHERE device_id = ? AND chat_jid = ?
                 ORDER BY is_self DESC, role DESC, LOWER(display_name), participant_jid",
            )
            .bind::<Integer, _>(device_id)
            .bind::<Text, _>(&chat)
            .load::<ParticipantRow>(connection)
            .map_err(database_store_error)?;
            Ok(Some((header, participants)))
        })
        .await
        .map_err(store_error)?;

    let Some((header, rows)) = result else {
        return Ok(None);
    };
    let permissions =
        serde_json::from_str::<GroupPermissions>(&header.permissions_json).map_err(cache_error)?;
    let participant_count = usize::try_from(header.participant_count)
        .map_err(|_| ServiceError::new(ErrorKind::Database, "invalid cached participant count"))?;
    let participants = rows
        .into_iter()
        .map(|row| {
            Ok(Participant {
                jid: row.participant_jid,
                display_name: row.display_name,
                avatar: row.avatar_ref.map(AvatarRef),
                role: role_from_i32(row.role)?,
                is_self: row.is_self != 0,
            })
        })
        .collect::<Result<Vec<_>, ServiceError>>()?;
    Ok(Some(GroupDetails {
        chat: ChatId::new(header.chat_jid),
        subject: header.subject,
        description: header.description,
        avatar: header.avatar_ref.map(AvatarRef),
        participant_count,
        participants,
        permissions,
    }))
}

/// Remove a group snapshot after the server acknowledges that this account
/// left. Header and participant rows disappear in one transaction so an
/// offline reopen can never expose a half-deleted or stale membership view.
pub async fn remove(
    shared: SharedSqlite,
    device_id: i32,
    chat: String,
) -> Result<(), ServiceError> {
    validate_group_identity(&chat)?;
    shared
        .run(move |connection| {
            connection
                .transaction::<_, diesel::result::Error, _>(|connection| {
                    diesel::sql_query(
                        "DELETE FROM wasabi_group_participants
                         WHERE device_id = ? AND chat_jid = ?",
                    )
                    .bind::<Integer, _>(device_id)
                    .bind::<Text, _>(&chat)
                    .execute(connection)?;
                    diesel::sql_query(
                        "DELETE FROM wasabi_group_cache
                         WHERE device_id = ? AND chat_jid = ?",
                    )
                    .bind::<Integer, _>(device_id)
                    .bind::<Text, _>(&chat)
                    .execute(connection)?;
                    Ok(())
                })
                .map_err(database_store_error)
        })
        .await
        .map_err(store_error)
}

fn validate_group_identity(value: &str) -> Result<(), ServiceError> {
    let jid = value
        .parse::<whatsapp_rust::Jid>()
        .map_err(|_| ServiceError::new(ErrorKind::InvalidRequest, "invalid group identity"))?;
    if !jid.is_group() {
        return Err(ServiceError::new(
            ErrorKind::InvalidRequest,
            "conversation is not a group",
        ));
    }
    Ok(())
}

fn validate_participant_identity(value: &str) -> Result<(), ServiceError> {
    value
        .parse::<whatsapp_rust::Jid>()
        .map(|_| ())
        .map_err(|_| ServiceError::new(ErrorKind::InvalidRequest, "invalid participant identity"))
}

fn role_to_i32(role: ParticipantRole) -> i32 {
    match role {
        ParticipantRole::Member => 0,
        ParticipantRole::Admin => 1,
        ParticipantRole::SuperAdmin => 2,
    }
}

fn role_from_i32(role: i32) -> Result<ParticipantRole, ServiceError> {
    match role {
        0 => Ok(ParticipantRole::Member),
        1 => Ok(ParticipantRole::Admin),
        2 => Ok(ParticipantRole::SuperAdmin),
        _ => Err(ServiceError::new(
            ErrorKind::Database,
            "invalid cached participant role",
        )),
    }
}

fn database_store_error(error: diesel::result::Error) -> StoreError {
    StoreError::Database(Box::new(error))
}

fn store_error(error: StoreError) -> ServiceError {
    ServiceError::new(ErrorKind::Database, error.to_string())
}

fn cache_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(ErrorKind::Database, error.to_string())
}
