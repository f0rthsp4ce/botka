//! Forward pinned messages from specified source chats to the target channel.

use std::iter::once;
use std::sync::Arc;

use anyhow::Result;
use diesel::prelude::*;
use teloxide::prelude::*;
use teloxide::types::MessageKind;
use teloxide::utils::html;

use crate::common::{BotEnv, TopicEmojis};
use crate::db::{DbChatId, DbThreadId};
use crate::models;

pub async fn inspect_message<'a>(
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

    let topic_link = render_topic_link(&bot, &env, &msg).await?;

    bot.send_message(forward_to.to, format!("<b>📌 in {topic_link}</b>"))
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true)
        .await?;

    // NOTE: might fail on some messages
    bot.forward_message(forward_to.to, pin.pinned.chat.id, pin.pinned.id)
        .await?;

    Ok(())
}

async fn render_topic_link(
    bot: &Bot,
    env: &BotEnv,
    msg: &Message,
) -> Result<String> {
    let chat_id = -msg.chat.id.0 - 1_000_000_000_000;
    Ok(match msg.thread_id {
        Some(thread_id) if msg.is_topic_message => {
            use crate::schema::tg_chat_topics::dsl as t;
            let topic: Option<models::TgChatTopic> = t::tg_chat_topics
                .filter(t::chat_id.eq(DbChatId::from(msg.chat.id)))
                .filter(t::topic_id.eq(DbThreadId::from(thread_id)))
                .select(t::tg_chat_topics::all_columns())
                .first(&mut *env.conn())
                .optional()?;
            if let Some(topic) = topic {
                let emojis = TopicEmojis::fetch(bot, once(&topic)).await?;
                format!(
                    "<a href=\"https://t.me/c/{chat_id}/{}/{}\">{} {}</a>",
                    thread_id.0,
                    msg.id,
                    emojis.get(&topic),
                    html::escape(topic.name.as_deref().unwrap_or("???")),
                )
            } else {
                format!("https://t.me/c/{chat_id}/{}/{}", thread_id.0, msg.id)
            }
        }
        _ => format!("https://t.me/c/{}/{}", chat_id, msg.id),
    })
}
