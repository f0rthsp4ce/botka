//! Send a welcome message to new residents. The message text is taken from
//! the wikijs page specified in the config.
//!
//! **Scope**: the first chat listed in the [`telegram.chats.residential`]
//! config option.
//!
//! [`telegram.chats.residential`]: crate::config::TelegramChats::residential

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{Duration, Utc};
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use itertools::Itertools as _;
use reqwest::Url;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, ParseMode, ReplyMarkup, User};

use crate::common::{BotEnv, UpdateHandler};
use crate::db::DbUserId;
use crate::utils::UserExt as _;

/// State contains the set of users who have already been welcomed.
#[derive(Clone, Debug, Default)]
pub struct State(HashSet<UserId>);

pub fn state() -> Arc<Mutex<State>> {
    Arc::new(Mutex::new(State::default()))
}

pub fn message_handler() -> UpdateHandler {
    Update::filter_message().filter_map(filter_joins).endpoint(handle_join)
}

#[derive(Debug, Clone)]
struct Newcomers(Vec<User>);

fn filter_joins(
    env: Arc<BotEnv>,
    state: Arc<Mutex<State>>,
    msg: Message,
) -> Option<Newcomers> {
    if *env.config.telegram.chats.residential.first()? != msg.chat.id {
        return None;
    }
    let new_members = msg.new_chat_members()?;
    let newcomer_ids: Vec<DbUserId> = {
        use crate::schema::residents::dsl as r;
        r::residents
            .filter(
                r::begin_date.gt(Utc::now().naive_utc() - Duration::hours(1)),
            )
            .filter(r::end_date.is_null())
            .filter(r::tg_id.eq_any({
                let state = state.lock().unwrap();
                new_members
                    .iter()
                    .filter(|m| !state.0.contains(&m.id))
                    .map(|m| DbUserId::from(m.id))
                    .collect_vec()
            }))
            .select(r::tg_id)
            .load(&mut *env.conn())
            .ok()?
    };
    if newcomer_ids.is_empty() {
        return None;
    }
    Some(Newcomers(
        new_members
            .iter()
            .filter(|m| newcomer_ids.contains(&DbUserId::from(m.id)))
            .cloned()
            .collect(),
    ))
}

async fn handle_join(
    bot: Bot,
    env: Arc<BotEnv>,
    state: Arc<Mutex<State>>,
    msg: Message,
    newcomers: Newcomers,
) -> Result<()> {
    let page = crate::utils::get_wikijs_page(
        &env.config.services.wikijs.url,
        &env.config.services.wikijs.token,
        &env.config.services.wikijs.welcome_message_page,
    )
    .await?;

    let text_template = extract_message(&page)
        .ok_or_else(|| anyhow::anyhow!("No fenced block in welcome message"))?;

    let text = text_template.replace(
        "%newcomer%",
        &newcomers.0.iter().map(|m| m.html_link()).join(", "),
    );

    let edit_button = InlineKeyboardButton::url(
        "✏️ Edit this message",
        Url::parse(&format!(
            "{}/{}",
            env.config.services.wikijs.url,
            env.config
                .services
                .wikijs
                .welcome_message_page
                .trim_start_matches('/'),
        ))?,
    );

    bot.send_message(msg.chat.id, text)
        .parse_mode(ParseMode::Html)
        .disable_web_page_preview(true)
        .reply_markup(ReplyMarkup::inline_kb([[edit_button]]))
        .await?;

    state.lock().unwrap().0.extend(newcomers.0.iter().map(|m| m.id));

    Ok(())
}

/// Get text within `> BEGIN` and `> END` markers.
/// TODO: move somewhere else.
pub fn extract_message(text: &str) -> Option<&str> {
    let begin_tag = "\n> BEGIN\n";
    let text = text
        .strip_prefix(&begin_tag[1..])
        .or_else(|| Some(&text[text.find(begin_tag)? + begin_tag.len()..]))?
        .trim_start();
    let end_tag = "\n> END\n";
    let text = text
        .strip_suffix(&end_tag[0..end_tag.len() - 1])
        .or_else(|| Some(&text[..text.rfind(end_tag)?]))?
        .trim_end();
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_message() {
        assert_eq!(extract_message(""), None);
        assert_eq!(extract_message("foo"), None);
        assert_eq!(extract_message("foo\n> BEGIN\n"), None);
        assert_eq!(extract_message("foo\n> BEGIN\nbar\n> END\n"), Some("bar"));
        assert_eq!(
            extract_message("foo\n> BEGIN\nbar\nbaz\n> END\n"),
            Some("bar\nbaz")
        );
        assert_eq!(extract_message("> BEGIN\nbar\n> END\n"), Some("bar"));
        assert_eq!(extract_message("foo\n> BEGIN\nbar\n> END"), Some("bar"));
    }
}
