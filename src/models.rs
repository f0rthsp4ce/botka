//! Database and Serde models.

use std::fmt::Debug;

use diesel::prelude::*;
use salvo_oapi::ToSchema;
use serde::{Deserialize, Serialize};
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

#[derive(Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::forwards)]
pub struct Forward {
    pub orig_chat_id: DbChatId,
    pub orig_msg_id: DbMessageId,

    pub backup_chat_id: DbChatId,
    pub backup_msg_id: DbMessageId,

    pub backup_text: String,
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
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BorrowedItem {
    pub name: String,
    pub returned: Option<chrono::DateTime<chrono::Utc>>,
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

// Database option models

#[derive(Serialize, Deserialize, Debug)]
pub struct Debate {
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub description: String,
}
#[derive(Serialize, Deserialize, Copy, Clone, Debug)]
pub struct NeedsLastPin {
    #[serde(flatten)]
    pub thread_id_pair: ThreadIdPair,
    pub message_id: MessageId,
}
config_option_def!(debate, Debate);
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
