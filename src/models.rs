//! Database and Serde models.

use std::fmt::Debug;

use diesel::prelude::*;
use salvo_oapi::ToSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use teloxide::types::{ChatMember, MessageId, UserId};

use crate::db::{
    config_option_def, DbChatId, DbMessageId, DbThreadId, DbUserId,
};
use crate::utils::{Sqlizer, ThreadIdPair};

// Database models

#[derive(Clone, Debug, Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::tg_users)]
pub struct TgUser {
    pub id: DbUserId,
    pub username: Option<String>,
    pub first_name: String,
    pub last_name: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::tg_users)]
pub struct NewTgUser<'a> {
    pub id: DbUserId,
    pub username: Option<&'a str>,
    pub first_name: &'a str,
    pub last_name: Option<&'a str>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Queryable, Selectable)]
#[diesel(table_name = crate::schema::tg_chats)]
pub struct TgChat {
    pub id: DbChatId,
    pub kind: String,
    pub username: Option<String>,
    pub title: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::tg_chats)]
pub struct NewTgChat<'a> {
    pub id: DbChatId,
    pub kind: &'a str,
    pub username: Option<&'a str>,
    pub title: Option<&'a str>,
}

#[derive(Clone, Debug, Insertable)]
#[diesel(table_name = crate::schema::tg_users_in_chats)]
pub struct NewTgUserInChat {
    pub chat_id: DbChatId,
    pub user_id: DbUserId,
    pub chat_member: Option<Sqlizer<ChatMember>>,
    pub seen: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Queryable, Selectable)]
#[diesel(table_name = crate::schema::tg_chat_topics)]
pub struct TgChatTopic {
    pub chat_id: DbChatId,
    pub topic_id: DbThreadId,
    pub closed: Option<bool>,
    pub name: Option<String>,
    pub icon_color: Option<i32>,
    pub icon_emoji: Option<String>,
    pub id_closed: DbMessageId,
    pub id_name: DbMessageId,
    pub id_icon_emoji: DbMessageId,
}

#[derive(
    Clone, Debug, Insertable, Queryable, Selectable, Serialize, ToSchema,
)]
#[diesel(table_name = crate::schema::residents)]
pub struct Resident {
    pub rowid: i32,
    pub tg_id: DbUserId,
    pub begin_date: chrono::NaiveDateTime,
    pub end_date: Option<chrono::NaiveDateTime>,
}

#[derive(Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::user_macs)]
pub struct UserMac {
    pub tg_id: DbUserId,
    pub mac: Sqlizer<macaddr::MacAddr6>,
}

#[derive(Clone, Debug, Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::tracked_polls)]
pub struct TrackedPoll {
    pub tg_poll_id: String,
    pub creator_id: DbUserId,
    pub info_chat_id: DbChatId,
    pub info_message_id: DbMessageId,
    pub voted_users: Sqlizer<Vec<DbUserId>>,
}

#[derive(Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::options)]
pub struct ConfigOption {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::borrowed_items)]
pub struct BorrowedItems {
    pub chat_id: DbChatId,
    pub thread_id: DbThreadId,
    pub user_message_id: DbMessageId,
    pub bot_message_id: DbMessageId,
    pub user_id: DbUserId,
    pub items: Sqlizer<Vec<BorrowedItem>>,
    pub created_at: chrono::NaiveDateTime,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BorrowedItem {
    pub name: String,
    #[serde(
        default,
        deserialize_with = "deserialize_option_datetime_backward_compatible"
    )]
    pub returned: Option<chrono::DateTime<chrono::Utc>>,
}

fn deserialize_option_datetime_backward_compatible<'de, D>(
    deserializer: D,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = JsonValue::deserialize(deserializer)?;
    match value {
        JsonValue::Null => Ok(None),
        JsonValue::Bool(b) => {
            if b {
                // Backward-compat: previously `returned` could be stored as `true`
                // Use UNIX epoch as a neutral placeholder timestamp
                let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                    .ok_or_else(|| serde::de::Error::custom("invalid epoch"))?;
                Ok(Some(ts))
            } else {
                Ok(None)
            }
        }
        JsonValue::String(s) => {
            if s.trim().is_empty() {
                return Ok(None);
            }
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|dt| Some(dt.with_timezone(&chrono::Utc)))
                .map_err(serde::de::Error::custom)
        }
        other => Err(serde::de::Error::custom(format!(
            "invalid type for returned: {other:?}"
        ))),
    }
}

#[derive(Clone, Debug, Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::borrowed_items_reminders)]
pub struct BorrowedItemsReminder {
    pub chat_id: DbChatId,
    pub user_message_id: DbMessageId,
    pub user_id: DbUserId,
    pub item_name: String,
    pub reminders_sent: i32,
    pub last_reminder_sent: Option<chrono::NaiveDateTime>,
    pub created_at: chrono::NaiveDateTime,
}

#[derive(Clone, Debug, Insertable)]
#[diesel(table_name = crate::schema::needed_items)]
pub struct NewNeededItem<'a> {
    pub request_chat_id: DbChatId,
    pub request_message_id: DbMessageId,
    pub request_user_id: DbUserId,
    pub pinned_chat_id: DbChatId,
    pub pinned_message_id: DbMessageId,
    pub buyer_user_id: Option<DbUserId>,
    pub item: &'a str,
}

#[derive(Clone, Debug, Queryable, Selectable)]
#[diesel(table_name = crate::schema::needed_items)]
pub struct NeededItem {
    pub rowid: i32,
    pub request_chat_id: DbChatId,
    pub request_message_id: DbMessageId,
    pub request_user_id: DbUserId,
    pub pinned_chat_id: DbChatId,
    pub pinned_message_id: DbMessageId,
    pub buyer_user_id: Option<DbUserId>,
    pub item: String,
}

#[derive(Clone, Debug, Insertable)]
#[diesel(table_name = crate::schema::dashboard_messages)]
pub struct NewDashboardMessage<'a> {
    pub chat_id: DbChatId,
    pub thread_id: DbThreadId,
    pub message_id: DbMessageId,
    pub text: &'a str,
}

#[derive(Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::user_ssh_keys)]
pub struct UserSshKey {
    pub tg_id: DbUserId,
    pub key: String,
}

#[allow(clippy::struct_field_names)]
#[derive(Clone, Debug, Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::memories)]
pub struct Memory {
    pub rowid: i32,
    pub memory_text: String,
    pub creation_date: chrono::NaiveDateTime,
    pub expiration_date: Option<chrono::NaiveDateTime>,
    pub chat_id: Option<DbChatId>,
    pub thread_id: Option<DbThreadId>,
    pub user_id: Option<DbUserId>,
}

#[derive(Clone, Debug, Insertable)]
#[diesel(table_name = crate::schema::memories)]
pub struct NewMemory<'a> {
    pub memory_text: &'a str,
    pub creation_date: chrono::NaiveDateTime,
    pub expiration_date: Option<chrono::NaiveDateTime>,
    pub chat_id: Option<DbChatId>,
    pub thread_id: Option<DbThreadId>,
    pub user_id: Option<DbUserId>,
}

#[derive(Clone, Debug, Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::chat_history)]
pub struct ChatHistoryEntry {
    pub rowid: i32,
    pub chat_id: DbChatId,
    pub thread_id: DbThreadId,
    pub message_id: DbMessageId,
    pub from_user_id: Option<DbUserId>,
    pub timestamp: chrono::NaiveDateTime,
    pub message_text: String,
    pub classification_result: Option<String>,
    pub used_model: Option<String>,
}

#[derive(Clone, Debug, Insertable)]
#[diesel(table_name = crate::schema::chat_history)]
pub struct NewChatHistoryEntry<'a> {
    pub chat_id: DbChatId,
    pub thread_id: DbThreadId,
    pub message_id: DbMessageId,
    pub from_user_id: Option<DbUserId>,
    pub timestamp: chrono::NaiveDateTime,
    pub message_text: &'a str,
    pub classification_result: Option<&'a str>,
    pub used_model: Option<&'a str>,
}

#[derive(Clone, Debug, Queryable, Insertable, Selectable)]
#[diesel(table_name = crate::schema::temp_open_tokens)]
pub struct TempOpenToken {
    pub id: i32,
    pub token: String,
    pub resident_tg_id: i64,
    pub guest_tg_id: Option<i64>,
    pub created_at: chrono::NaiveDateTime,
    pub expires_at: chrono::NaiveDateTime,
    pub used_at: Option<chrono::NaiveDateTime>,
}

#[derive(Insertable)]
#[diesel(table_name = crate::schema::temp_open_tokens)]
pub struct NewTempOpenToken<'a> {
    pub token: &'a str,
    pub resident_tg_id: i64,
    pub expires_at: chrono::NaiveDateTime,
}

// Database option models

#[derive(Serialize, Deserialize, Copy, Clone, Debug)]
pub struct NeedsLastPin {
    #[serde(flatten)]
    pub thread_id_pair: ThreadIdPair,
    pub message_id: MessageId,
}
config_option_def!(wikijs_update_state, crate::utils::WikiJsUpdateState);
config_option_def!(needs_last_pin, NeedsLastPin);

// Serde models

#[derive(Serialize, Deserialize, Debug, ToSchema)]
pub struct DataResident {
    #[salvo(schema(value_type = DbUserId))]
    pub id: UserId,
    pub username: Option<String>,
    pub first_name: String,
    pub last_name: Option<String>,
}
