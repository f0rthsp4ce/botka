//! Forward pinned messages from specified source chats to the target channel.

use std::sync::Arc;

use anyhow::Result;
use diesel::prelude::*;
use teloxide::prelude::*;
use teloxide::types::MessageKind;
use teloxide::utils::html;

use crate::common::BotEnv;
use crate::db::{DbChatId, DbThreadId};

pub async fn inspect_message<'a>(bot: Bot, env: Arc<BotEnv>, msg: Message) {
    if let Err(e) = inspect_message_result(bot, env, msg).await {
        log::error!("Error handling message: {}", e);
    }
}

async fn inspect_message_result<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let MessageKind::Pinned(pin) = &msg.kind else { return Ok(()) };

    let forward_to = env.config.telegram.chats.forward_pins.iter().find(|f| {
        f.from == msg.chat.id
            && msg.thread_id.map_or(true, |t| !f.ignore_threads.contains(&t))
    });
    let Some(forward_to) = forward_to else { return Ok(()) };

    let text =
        format!("<b>📌 in {}</b>", link_to_message(&mut env.conn(), &msg)?);

    bot.send_message(forward_to.to, text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true)
        .await?;

    // NOTE: might fail on some messages
    bot.forward_message(forward_to.to, pin.pinned.chat.id, pin.pinned.id)
        .await?;

    Ok(())
}

fn link_to_message(
    conn: &mut SqliteConnection,
    msg: &Message,
) -> Result<String> {
    let chat_id = -msg.chat.id.0 - 1_000_000_000_000;
    Ok(match msg.thread_id {
        Some(thread_id) if msg.is_topic_message => {
            use crate::schema::tg_chat_topics::dsl as t;
            let (topic_name, topic_emoji): (Option<String>, Option<String>) =
                t::tg_chat_topics
                    .filter(t::chat_id.eq(DbChatId::from(msg.chat.id)))
                    .filter(t::topic_id.eq(DbThreadId::from(thread_id)))
                    .select((t::name, t::icon_emoji))
                    .first::<(Option<String>, Option<String>)>(conn)
                    .optional()?
                    .unwrap_or_default();
            let topic_emoji = topic_emoji.unwrap_or_else(|| "💬".to_owned());
            #[allow(clippy::single_match_else, clippy::option_if_let_else)]
            match topic_name {
                Some(topic_name) => format!(
                    "<a href=\"https://t.me/c/{chat_id}/{}/{}\">{topic_emoji} {}</a>",
                    thread_id.0,
                    msg.id,
                    html::escape(&topic_name),
                ),
                None => format!(
                    "https://t.me/c/{chat_id}/{}/{}", thread_id.0, msg.id,
                ),
            }
        }
        _ => format!("https://t.me/c/{}/{}", chat_id, msg.id),
    })
}
