use std::fmt::Debug;
use std::net::SocketAddr;

use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use teloxide::types::{ChatId, ChatMember, ThreadId, UserId};

use crate::db::{
    config_option_def, DbChatId, DbMessageId, DbThreadId, DbUserId,
};
use crate::utils::Sqlizer;

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

#[derive(Clone, Debug, Insertable, Queryable, Selectable)]
#[diesel(table_name = crate::schema::residents)]
pub struct Resident {
    pub tg_id: DbUserId,
    pub is_resident: bool,
    pub is_bot_admin: bool,
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

// Database option models

#[derive(Serialize, Deserialize, Debug)]
pub struct Debate {
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub description: String,
}
config_option_def!(debate, Debate);
config_option_def!(wikijs_update_state, crate::utils::WikiJsUpdateState);

// Config models

#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    pub telegram: TelegramConfig,
    pub db: String,
    pub log_file: String,
    pub server_addr: SocketAddr,
    pub services: ServicesConfig,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TelegramConfig {
    pub token: String,
    pub forward_channel: ChatId,
    pub residential_chats: Vec<ChatId>,
    pub borrowed_items_threads: Vec<TelegramConfigThread>,
    pub chats: TelegramConfigChats,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TelegramConfigThread {
    pub chat: ChatId,
    pub thread: ThreadId,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TelegramConfigChats {
    pub wikijs_updates: TelegramConfigThread,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServicesConfig {
    pub mikrotik: MikrotikConfig,
    pub home_assistant: HomeAssistantConfig,
    pub wikijs: WikiJsConfig,
    pub openai: OpenAIConfig,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MikrotikConfig {
    pub host: String,
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct HomeAssistantConfig {
    pub host: String,
    pub token: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct WikiJsConfig {
    pub url: String,
    pub token: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct OpenAIConfig {
    pub api_key: String,
    #[serde(default)]
    pub disable: bool,
}

// Serde models
#[derive(Serialize, Deserialize, Debug)]
pub struct DataResident {
    pub id: UserId,
    pub username: Option<String>,
    pub first_name: String,
    pub last_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_example_config() -> anyhow::Result<()> {
        let config_text = std::fs::read_to_string("config.example.yaml")?;
        let config: Config = serde_yaml::from_str(&config_text)?;

        similar_asserts::assert_serde_eq!(
            serde_yaml::to_value(&config)?,
            serde_yaml::from_str::<serde_yaml::Value>(&config_text)?,
            "Extra fields in config.example.yaml?",
        );

        Ok(())
    }
}
