use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use diesel::prelude::*;
use itertools::Itertools;
use macro_rules_attribute::derive;
use teloxide::prelude::*;
use teloxide::types::{StickerKind, ThreadId};
use teloxide::utils::command::BotCommands;
use teloxide::utils::html;

use crate::common::{
    filter_command, format_users, format_users2, BotEnv, CommandHandler,
    MyDialogue, State,
};
use crate::db::{DbChatId, DbUserId};
use crate::utils::BotExt;
use crate::{models, schema, HasCommandRules};

#[derive(BotCommands, Clone, HasCommandRules!)]
#[command(
    rename_rule = "snake_case",
    description = "These commands are supported:"
)]
enum Command {
    #[command(description = "display this text.")]
    Help,

    #[command(description = "list residents.")]
    Residents,

    #[command(description = "show status.")]
    Status,

    #[command(description = "show topic list")]
    #[custom(in_group = false)]
    Topics,

    #[command(description = "show bot version.")]
    Version,
}

pub fn command_handler() -> CommandHandler<Result<()>> {
    filter_command::<Command, _>().endpoint(start)
}

async fn start<'a>(
    bot: Bot,
    dialogue: MyDialogue,
    env: Arc<BotEnv>,
    msg: Message,
    command: Command,
) -> Result<()> {
    dialogue.update(State::Start).await?;
    match command {
        Command::Help => {
            bot.reply_message(&msg, Command::descriptions().to_string())
                .await?;
        }
        Command::Residents => cmd_list_residents(bot, env, msg).await?,
        Command::Status => cmd_status(bot, env, msg).await?,
        Command::Version => {
            bot.reply_message(&msg, crate::VERSION).await?;
        }
        Command::Topics => cmd_topics(bot, env, msg).await?,
    }
    Ok(())
}

async fn cmd_list_residents<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let residents = schema::residents::table
        .filter(schema::residents::is_resident.eq(true))
        .left_join(
            schema::tg_users::table
                .on(schema::residents::tg_id.eq(schema::tg_users::id)),
        )
        .order(schema::residents::tg_id.asc())
        .load::<(models::Resident, Option<models::TgUser>)>(&mut *env.conn())
        .unwrap();
    let mut text = String::new();

    text.push_str("Residents: ");
    text.push_str(&format_users(residents.iter().map(|(r, u)| (r, u))));
    text.push('.');
    bot.reply_message(&msg, text).await?;
    Ok(())
}

async fn cmd_status(bot: Bot, env: Arc<BotEnv>, msg: Message) -> Result<()> {
    #[derive(serde::Deserialize, Debug)]
    #[serde(rename_all = "kebab-case")]
    struct Lease {
        mac_address: String,
        #[serde(deserialize_with = "crate::utils::deserealize_duration")]
        last_seen: Duration,
    }

    let conf = &env.config.services.mikrotik;
    let leases = env
        .reqwest_client
        .post(format!("https://{}/rest/ip/dhcp-server/lease/print", conf.host))
        .timeout(Duration::from_secs(5))
        .basic_auth(&conf.username, Some(&conf.password))
        .json(&serde_json::json!({
            ".proplist": [
                "mac-address",
                "last-seen",
            ]
        }))
        .send()
        .await?
        .json::<Vec<Lease>>()
        .await;

    let mut text = String::new();
    match leases {
        Ok(leases) => {
            let active_mac_addrs = leases
                .into_iter()
                .filter(|l| l.last_seen < Duration::from_secs(11 * 60))
                .map(|l| l.mac_address)
                .collect::<Vec<_>>();
            let data: Vec<(DbUserId, Option<models::TgUser>)> =
                schema::user_macs::table
                    .left_join(
                        schema::tg_users::table
                            .on(schema::user_macs::tg_id
                                .eq(schema::tg_users::id)),
                    )
                    .filter(schema::user_macs::mac.eq_any(&active_mac_addrs))
                    .select((
                        schema::user_macs::tg_id,
                        schema::tg_users::all_columns.nullable(),
                    ))
                    .distinct()
                    .load(&mut *env.conn())?;
            writeln!(&mut text, "Currently in space: ").unwrap();
            format_users2(&mut text, data.iter().map(|(id, u)| (*id, u)));
        }
        Err(e) => {
            log::error!("Failed to get leases: {}", e);
            writeln!(text, "Failed to get leases.").unwrap();
        }
    }
    bot.reply_message(&msg, text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true)
        .await?;

    Ok(())
}

async fn cmd_topics(bot: Bot, env: Arc<BotEnv>, msg: Message) -> Result<()> {
    let Some(user) = &msg.from else { return Ok(()) };

    let user_chats = schema::tg_users_in_chats::table
        .filter(schema::tg_users_in_chats::user_id.eq(DbUserId::from(user.id)))
        .select(schema::tg_users_in_chats::chat_id)
        .load::<DbChatId>(&mut *env.conn())?;

    if user_chats.is_empty() {
        bot.reply_message(&msg, "You are not in any tracked chats.").await?;
        return Ok(());
    }

    let topics: Vec<models::TgChatTopic> = schema::tg_chat_topics::table
        .filter(schema::tg_chat_topics::chat_id.eq_any(user_chats))
        .select(schema::tg_chat_topics::all_columns)
        .load(&mut *env.conn())?;

    if topics.is_empty() {
        bot.reply_message(&msg, "No topics in your chats.").await?;
        return Ok(());
    }

    let mut emojis = topics
        .iter()
        .filter_map(|t| t.icon_emoji.as_ref())
        .filter(|i| !i.is_empty())
        .cloned()
        .collect_vec();
    emojis.sort();
    emojis.dedup();

    let emojis = bot
        .get_custom_emoji_stickers(emojis)
        .await?
        .into_iter()
        .filter_map(|e| {
            let StickerKind::CustomEmoji { custom_emoji_id } = e.kind else {
                return None;
            };
            Some((custom_emoji_id, e.emoji?))
        })
        .collect::<HashMap<_, _>>();

    let mut chats = HashMap::new();
    for topic in &topics {
        chats.entry(topic.chat_id).or_insert_with(Vec::new).push(topic);
    }

    let mut text = String::new();
    for (chat_id, topics) in chats {
        let chat: models::TgChat = schema::tg_chats::table
            .filter(schema::tg_chats::id.eq(chat_id))
            .first(&mut *env.conn())?;
        writeln!(
            &mut text,
            "<b>{}</b>",
            chat.title.as_ref().map_or(String::new(), |t| html::escape(t))
        )
        .unwrap();

        for topic in topics {
            render_topic_link(&mut text, &emojis, topic);
        }
        text.push('\n');
    }

    bot.reply_message(&msg, text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true)
        .await?;

    Ok(())
}

fn render_topic_link(
    out: &mut String,
    emojis: &HashMap<String, String>,
    topic: &models::TgChatTopic,
) {
    write!(
        out,
        "<a href=\"https://t.me/c/{}/{}\">",
        -ChatId::from(topic.chat_id).0 - 1_000_000_000_000,
        ThreadId::from(topic.topic_id),
    )
    .unwrap();

    out.push_str(
        topic
            .icon_emoji
            .as_ref()
            .and_then(|e| emojis.get(e))
            .map_or("💬", |e| e.as_str()),
    );
    out.push(' ');

    if let Some(name) = &topic.name {
        out.push_str(&html::escape(name));
    } else {
        write!(out, "Topic #{}", ThreadId::from(topic.topic_id)).unwrap();
    }

    out.push_str("</a>");

    out.push('\n');
}
