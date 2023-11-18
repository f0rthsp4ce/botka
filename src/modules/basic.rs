use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use diesel::prelude::*;
use itertools::Itertools;
use macro_rules_attribute::derive;
use teloxide::prelude::*;
use teloxide::types::{InputFile, ThreadId};
use teloxide::utils::command::BotCommands;
use teloxide::utils::html;

use crate::common::{
    filter_command, format_users, BotEnv, CommandHandler, HasCommandRules,
    HasCommandRulesTrait, MyDialogue, State, TopicEmojis,
};
use crate::db::{DbChatId, DbUserId};
use crate::utils::{write_message_link, BotExt};
use crate::{models, schema};

#[derive(BotCommands, Clone, HasCommandRules!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(description = "display this text.")]
    Help,

    #[command(description = "list residents.")]
    Residents,

    #[command(description = "show residents timeline.")]
    #[custom(resident = true)]
    ResidentsTimeline,

    #[command(description = "show status.")]
    Status,

    #[command(description = "show topic list.")]
    #[custom(in_group = false)]
    Topics,

    #[command(description = "show bot version.")]
    Version,
}

pub fn command_handler() -> CommandHandler<Result<()>> {
    filter_command::<Commands, _>().endpoint(start)
}

async fn start<'a>(
    bot: Bot,
    dialogue: MyDialogue,
    env: Arc<BotEnv>,
    msg: Message,
    command: Commands,
) -> Result<()> {
    dialogue.update(State::Start).await?;
    match command {
        Commands::Help => cmd_help(bot, msg).await?,
        Commands::Residents => cmd_list_residents(bot, env, msg).await?,
        Commands::ResidentsTimeline => {
            cmd_show_residents_timeline(bot, env, msg).await?;
        }
        Commands::Status => cmd_status(bot, env, msg).await?,
        Commands::Version => {
            bot.reply_message(&msg, crate::version()).await?;
        }
        Commands::Topics => cmd_topics(bot, env, msg).await?,
    }
    Ok(())
}

async fn cmd_help(bot: Bot, msg: Message) -> Result<()> {
    let mut text = String::new();
    text.push_str("Available commands:\n\n");
    text.push_str(&commands_help::<crate::modules::basic::Commands>());
    text.push_str(&commands_help::<crate::modules::needs::Commands>());
    text.push_str(&commands_help::<crate::modules::userctl::Commands>());
    if false {
        // Not much of use for now.
        text.push_str(&commands_help::<crate::modules::debates::Commands>());
        // "..., and with ** are available only to bot technicians."
    }
    text.push_str("\nCommands marked with * are available only to residents.");
    bot.reply_message(&msg, text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await?;
    Ok(())
}

fn commands_help<T: HasCommandRulesTrait + BotCommands>() -> String {
    let descriptions = T::descriptions().to_string();
    let global_description =
        descriptions.find("\n\n/").map(|i| &descriptions[..i]);

    let mut result = String::new();
    if let Some(global_description) = global_description {
        result.push_str(global_description);
        result.push('\n');
    }
    for (cmd, rules) in std::iter::zip(&T::bot_commands(), T::COMMAND_RULES) {
        result.push_str(&cmd.command);
        result.push_str(match (rules.admin, rules.resident) {
            (true, _) => "**",
            (false, true) => "*",
            (false, false) => "",
        });
        result.push_str(match (rules.in_private, rules.in_group) {
            (true, true) => "",
            (true, false) => " (in private)",
            (false, true) => " (not in private)",
            (false, false) => " (disabled?)",
        });
        result.push_str(" — ");
        result.push_str(&cmd.description);
        result.push('\n');
    }

    result
}

async fn cmd_list_residents<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let residents: Vec<(DbUserId, Option<models::TgUser>)> =
        schema::residents::table
            .filter(schema::residents::end_date.is_null())
            .left_join(
                schema::tg_users::table
                    .on(schema::residents::tg_id.eq(schema::tg_users::id)),
            )
            .select((
                schema::residents::tg_id,
                schema::tg_users::all_columns.nullable(),
            ))
            .order(schema::residents::begin_date.desc())
            .load(&mut *env.conn())?;
    let mut text = String::new();

    text.push_str("Residents: ");
    format_users(&mut text, residents.iter().map(|(r, u)| (*r, u)));
    text.push('.');
    bot.reply_message(&msg, text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true)
        .await?;
    Ok(())
}

async fn cmd_show_residents_timeline(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let db = env.config.db.as_str();
    let db = db.strip_prefix("sqlite://").unwrap_or(db);
    let svg = Command::new("f0-residents-timeline")
        .arg("-sqlite")
        .arg(db)
        .output()?;
    if !svg.status.success() || !svg.stdout.starts_with(b"<svg") {
        bot.reply_message(&msg, "Failed to generate timeline (svg).").await?;
        return Ok(());
    }
    let mut png = Command::new("convert")
        .arg("svg:-")
        .arg("png:-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    png.stdin.take().unwrap().write_all(&svg.stdout)?;
    let png = png.wait_with_output()?;
    if !png.status.success() || !png.stdout.starts_with(b"\x89PNG") {
        bot.reply_message(&msg, "Failed to generate timeline (png).").await?;
        return Ok(());
    }
    bot.reply_photo(&msg, InputFile::memory(png.stdout)).await?;
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
    let leases = async {
        env.reqwest_client
            .post(format!(
                "https://{}/rest/ip/dhcp-server/lease/print",
                conf.host
            ))
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
            .await
    }
    .await;

    crate::metrics::update_service("mikrotik", leases.is_ok());

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
            format_users(&mut text, data.iter().map(|(id, u)| (*id, u)));
        }
        Err(e) => {
            log::error!("Failed to get leases: {e}");
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

    let topic_emojis = TopicEmojis::fetch(&bot, topics.iter()).await?;

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
            render_topic_link(&mut text, &topic_emojis, topic);
        }
        text.push('\n');
    }

    for lines in text.lines().collect_vec().chunks(100) {
        let text = lines.join("\n");
        bot.reply_message(&msg, text)
            .parse_mode(teloxide::types::ParseMode::Html)
            .disable_web_page_preview(true)
            .await?;
    }

    Ok(())
}

fn render_topic_link(
    out: &mut String,
    emojis: &TopicEmojis,
    topic: &models::TgChatTopic,
) {
    write_message_link(out, topic.chat_id, ThreadId::from(topic.topic_id).0);
    out.push_str(emojis.get(topic));
    out.push(' ');
    if let Some(name) = &topic.name {
        out.push_str(&html::escape(name));
    } else {
        write!(out, "Topic #{}", ThreadId::from(topic.topic_id)).unwrap();
    }
    out.push_str("</a>\n");
}
