//! Forward pinned messages from specified source chats to the target channel.
//!
//! **Scope**: chats listed in [`telegram.chats.forward_pins`] config option.
//!
//! [`telegram.chats.forward_pins`]: crate::config::TelegramChats::forward_pins

use std::collections::HashSet;
use std::iter::once;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use diesel::prelude::*;
use reqwest::Url;
use teloxide::prelude::*;
use teloxide::types::{
    InlineKeyboardButton, MessageKind, ReplyMarkup, ThreadId,
};
use teloxide::utils::html;

use crate::common::{BotEnv, TopicEmojis};
use crate::db::{DbChatId, DbThreadId};
use crate::models;
use crate::utils::{format_to, ChatIdExt as _, MessageExt as _};

/// State contains a set of newly created topics.
#[derive(Clone, Debug, Default)]
pub struct State(HashSet<(ChatId, ThreadId)>);

pub fn state() -> Arc<Mutex<State>> {
    Arc::new(Mutex::new(State::default()))
}

pub async fn inspect_message<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    state: Arc<Mutex<State>>,
    msg: Message,
) -> Result<()> {
    if let (MessageKind::ForumTopicCreated(_), Some(thread_id)) =
        (&msg.kind, msg.thread_id)
    {
        let mut state = state.lock().unwrap();
        state.0.insert((msg.chat.id, thread_id));
    } else if let (Some(reply), Some(thread_id)) =
        (msg.reply_to_message(), msg.thread_id)
    {
        if state.lock().unwrap().0.remove(&(reply.chat.id, thread_id)) {
            // This is the first message in newly created topic. It is pinned
            // implicitly.
            forward_message(&bot, &env, &msg, false).await?;
        }
    } else if let Some(pin) = msg.pinned_message() {
        forward_message(&bot, &env, pin, true).await?;
    }
    Ok(())
}

async fn forward_message(
    bot: &Bot,
    env: &BotEnv,
    msg: &Message,
    is_pin: bool,
) -> Result<()> {
    let forward_to = env.config.telegram.chats.forward_pins.iter().find(|f| {
        f.from == msg.chat.id
            && msg.thread_id.map_or(true, |t| !f.ignore_threads.contains(&t))
    });
    let Some(forward_to) = forward_to else { return Ok(()) };

    let (link_url, topic_name) = make_message_link(bot, env, msg).await?;

    let mut buttons = vec![[InlineKeyboardButton::url(
        if is_pin {
            format!("📌 in {topic_name}")
        } else {
            format!("➕ {topic_name}")
        },
        Url::parse(&link_url)?,
    )]];
    if let Some(from) = msg.forward_from_user().or(msg.from.as_ref()) {
        buttons.push([InlineKeyboardButton::url(
            format!("👤 {}", from.full_name()),
            Url::parse(&format!("tg://user?id={}", from.id))?,
        )]);
    }

    if let Some(poll) = msg.poll().filter(|p| !p.is_anonymous) {
        // Polls with visible voters can't be forwarded to channels.
        bot.send_message(forward_to.to, render_poll(poll))
            .parse_mode(teloxide::types::ParseMode::Html)
            .reply_markup(ReplyMarkup::inline_kb(buttons))
            .disable_web_page_preview(true)
            .await?;
    } else {
        bot.copy_message(forward_to.to, msg.chat.id, msg.id)
            .reply_markup(ReplyMarkup::inline_kb(buttons))
            .send()
            .await?;
    }

    Ok(())
}

async fn make_message_link(
    bot: &Bot,
    env: &BotEnv,
    msg: &Message,
) -> Result<(String, String)> {
    let Some(chat_id) = msg.chat.id.channel_t_me_id() else {
        let text = msg.chat.title().unwrap_or("Unknown chat");
        return Ok(("https://t.me/".to_string(), text.to_string()));
    };

    #[allow(clippy::option_if_let_else)]
    let url = if let Some(thread_id) = msg.thread_id_ext() {
        format!("https://t.me/c/{chat_id}/{}/{}", thread_id.0, msg.id)
    } else {
        format!("https://t.me/c/{chat_id}/{}", msg.id)
    };

    let mut text = String::new();
    let mut has_text = false;

    if let Some(thread_id) = msg.thread_id_ext() {
        has_text = true;
        use crate::schema::tg_chat_topics::dsl as t;
        let topic: Option<models::TgChatTopic> = t::tg_chat_topics
            .filter(t::chat_id.eq(DbChatId::from(msg.chat.id)))
            .filter(t::topic_id.eq(DbThreadId::from(thread_id)))
            .select(t::tg_chat_topics::all_columns())
            .first(&mut *env.conn())
            .optional()?;
        if let Some(topic) = topic {
            let emojis = TopicEmojis::fetch(bot, once(&topic)).await?;
            format_to!(
                text,
                "{} {}",
                emojis.get(&topic),
                topic.name.as_deref().unwrap_or("Unknown topic"),
            );
        }
    }

    if let Some(title) = msg.chat.title() {
        if has_text {
            format_to!(text, " @ ");
        }
        has_text = true;
        format_to!(text, "{}", title);
    }

    if !has_text {
        format_to!(text, "Unknown chat");
    }

    Ok((url, text))
}

fn render_poll(poll: &Poll) -> String {
    let mut text = html::escape(&poll.question);
    format_to!(text, "\n\n<u>");
    text.push_str(if poll.is_closed { "Closed poll" } else { "Poll" });
    format_to!(text, "</u>\n");
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
