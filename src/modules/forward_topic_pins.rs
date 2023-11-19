//! Forward pinned messages from specified source chats to the target channel.
//!
//! **Scope**: chats listed in [`telegram.chats.forward_pins`] config option.
//!
//! [`telegram.chats.forward_pins`]: crate::config::TelegramChats::forward_pins

use std::iter::once;
use std::sync::Arc;

use anyhow::Result;
use diesel::prelude::*;
use teloxide::prelude::*;
use teloxide::utils::html;

use crate::common::{BotEnv, TopicEmojis};
use crate::db::{DbChatId, DbThreadId};
use crate::models;
use crate::utils::{format_to, UserExt};

pub async fn inspect_message<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    pin: Message,
) -> Result<()> {
    let Some(msg) = pin.pinned_message() else { return Ok(()) };

    let forward_to = env.config.telegram.chats.forward_pins.iter().find(|f| {
        f.from == msg.chat.id
            && msg.thread_id.map_or(true, |t| !f.ignore_threads.contains(&t))
    });
    let Some(forward_to) = forward_to else { return Ok(()) };

    let topic_link = render_topic_link(&bot, &env, msg).await?;

    if let Some(poll) = msg.poll().filter(|p| !p.is_anonymous) {
        // Polls with visible voters can't be forwarded to channels.
        bot.send_message(forward_to.to, render_poll(msg, poll, &topic_link))
            .parse_mode(teloxide::types::ParseMode::Html)
            .disable_web_page_preview(true)
            .await?;
        return Ok(());
    }

    bot.send_message(forward_to.to, format!("<b>📌 in {topic_link}</b>"))
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true)
        .await?;

    bot.forward_message(forward_to.to, msg.chat.id, msg.id).await?;

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

fn render_poll(msg: &Message, poll: &Poll, topic_link: &str) -> String {
    let mut text = "<b>📌 Poll ".to_string();
    if poll.is_closed {
        format_to!(text, "results ");
    }
    format_to!(text, "in {topic_link}");
    if let Some(from) = &msg.from {
        format_to!(text, " by {}", from.html_link());
    }
    if let Some(forwarded_from) = &msg.forward_from_user() {
        format_to!(text, " (forwarded from {})", forwarded_from.html_link());
    }
    format_to!(text, "</b>\n\n{}\n\n", html::escape(&poll.question));
    for opt in &poll.options {
        if poll.is_closed {
            let percent = opt.voter_count * 100 / poll.total_voter_count;
            format_to!(text, "<code>{percent:>3}%</code> ");
        } else {
            format_to!(text, "◯ ");
        }
        format_to!(text, "{}\n", html::escape(&opt.text));
    }
    text
}
