use std::sync::Arc;

use anyhow::Result;
use async_openai::types::{
    ChatCompletionRequestMessageArgs, CreateChatCompletionRequestArgs,
};
use chrono::DateTime;
use diesel::prelude::*;
use itertools::Itertools;
use teloxide::prelude::*;
use teloxide::types::{
    InlineKeyboardButton, InlineKeyboardMarkup, MediaKind, MessageId,
    MessageKind, ParseMode, ReplyMarkup, User,
};
use teloxide::utils::html;

use crate::common::{BotEnv, CommandHandler};
use crate::utils::Sqlizer;
use crate::{models, schema};

pub fn command_handler() -> CommandHandler<Result<()>> {
    dptree::filter(filter_messages_in_topic).endpoint(handle_message)
}

pub fn callback_handler() -> CommandHandler<Result<()>> {
    dptree::filter_map(filter_callbacks).endpoint(handle_callback)
}

fn filter_messages_in_topic(env: Arc<BotEnv>, msg: Message) -> bool {
    env.config
        .telegram
        .borrowed_items_threads
        .iter()
        .any(|c| msg.chat.id == c.chat && msg.thread_id == Some(c.thread))
}

async fn handle_message(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let Some(user) = msg.from() else { return Ok(()) };
    let Some(text) = textify_message(&msg) else { return Ok(()) };
    let item_names = match classify(env.clone(), &text).await? {
        ClassificationResult::Took(items) => items,
        ClassificationResult::Returned => return Ok(()),
        ClassificationResult::Unknown => return Ok(()),
    };

    if item_names.is_empty() {
        return Ok(());
    }

    let items = item_names
        .into_iter()
        .map(|i| models::BorrowedItem { name: i, returned: None })
        .collect_vec();

    let bot_message = bot
        .send_message(msg.chat.id, make_text(user, &items))
        .message_thread_id(msg.thread_id.unwrap())
        .parse_mode(ParseMode::Html)
        .reply_markup(ReplyMarkup::InlineKeyboard(make_keyboard(
            msg.chat.id,
            msg.id,
            &items,
        )))
        .disable_notification(true)
        .await?;

    env.transaction(|conn| {
        diesel::insert_into(schema::borrowed_items::table)
            .values(models::BorrowedItems {
                chat_id: msg.chat.id.into(),
                thread_id: msg.thread_id.unwrap(),
                user_message_id: msg.id.into(),
                bot_message_id: bot_message.id.into(),
                user_id: msg.from().unwrap().id.into(),
                items: Sqlizer::new(items).unwrap(),
            })
            .execute(conn)?;
        Ok(())
    })?;

    bot.pin_chat_message(msg.chat.id, msg.id)
        .disable_notification(true)
        .await?;

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct CallbackData {
    chat_id: ChatId,
    user_message_id: MessageId,
    item_index: usize,
}

fn filter_callbacks(callback: CallbackQuery) -> Option<CallbackData> {
    let data = callback.data.as_ref()?.strip_prefix("b:")?;
    let mut split = data.split(':');
    let chat_id = split.next()?.parse::<i64>().ok()?;
    let user_message_id = split.next()?.parse::<i32>().ok()?;
    let item_index = split.next()?.parse::<usize>().ok()?;
    if split.next().is_some() {
        return None;
    }
    Some(CallbackData {
        chat_id: ChatId(chat_id),
        user_message_id: MessageId(user_message_id),
        item_index,
    })
}

enum CallbackResponse {
    NotYourMessage,
    AlreadyReturned,
    Update(models::BorrowedItems),
}

async fn handle_callback(
    bot: Bot,
    env: Arc<BotEnv>,
    cd: CallbackData,
    callback: CallbackQuery,
) -> Result<()> {
    let resp = env.transaction(|conn| {
        let mut bi: models::BorrowedItems = schema::borrowed_items::table
            .filter(schema::borrowed_items::chat_id.eq(cd.chat_id.0))
            .filter(
                schema::borrowed_items::user_message_id
                    .eq(cd.user_message_id.0),
            )
            .first(conn)?;

        if callback.from.id != bi.user_id.into() {
            return Ok(CallbackResponse::NotYourMessage);
        }

        if (bi.items.as_ref())[cd.item_index].returned.is_some() {
            return Ok(CallbackResponse::AlreadyReturned);
        }
        bi.items = bi
            .items
            .map(|items| {
                let mut items = items.clone();
                items[cd.item_index].returned = Some(chrono::Utc::now());
                items
            })
            .expect("Failed to serialize borrowed items");

        diesel::update(schema::borrowed_items::table)
            .filter(schema::borrowed_items::chat_id.eq(cd.chat_id.0))
            .filter(
                schema::borrowed_items::user_message_id
                    .eq(cd.user_message_id.0),
            )
            .set(schema::borrowed_items::items.eq(&bi.items))
            .execute(conn)?;

        Ok(CallbackResponse::Update(bi))
    });

    match resp {
        Ok(CallbackResponse::NotYourMessage) => {
            bot.answer_callback_query(callback.id)
                .text("This is not your message.")
                .await?;
            Ok(())
        }
        Ok(CallbackResponse::AlreadyReturned) => {
            bot.answer_callback_query(callback.id)
                .text("This item is already returned.")
                .await?;
            Ok(())
        }
        Ok(CallbackResponse::Update(bi)) => {
            bot.answer_callback_query(callback.id).await?;
            let all_returned = bi.items.iter().all(|i| i.returned.is_some());
            let mut edit = bot
                .edit_message_text(
                    cd.chat_id,
                    bi.bot_message_id.into(),
                    make_text(&callback.from, &bi.items),
                )
                .parse_mode(ParseMode::Html);
            if !all_returned {
                edit = edit.reply_markup(make_keyboard(
                    cd.chat_id,
                    cd.user_message_id,
                    &bi.items,
                ));
            }
            edit.await.ok();
            if all_returned {
                bot.unpin_chat_message(cd.chat_id)
                    .message_id(cd.user_message_id)
                    .await?;
            }
            Ok(())
        }
        Err(e) => {
            bot.answer_callback_query(callback.id)
                .text("Internal error")
                .await?;
            Err(e.into())
        }
    }
}

#[derive(Clone, Debug)]
enum ClassificationResult {
    Took(Vec<String>),
    Returned,
    Unknown,
}

async fn classify(
    env: Arc<BotEnv>,
    text: &str,
) -> Result<ClassificationResult> {
    if env.config.services.openai.disable {
        classify_dumb(text)
    } else {
        classify_openai(env, text).await
    }
}

#[allow(clippy::unnecessary_wraps)] // for consistency
fn classify_dumb(text: &str) -> Result<ClassificationResult> {
    let items: Vec<_> = match text.strip_prefix("took") {
        Some(text) => text.trim().split(' ').map(|s| s.to_string()).collect(),
        None => return Ok(ClassificationResult::Unknown),
    };
    if items.is_empty() {
        return Ok(ClassificationResult::Unknown);
    }
    Ok(ClassificationResult::Took(items))
}

async fn classify_openai(
    env: Arc<BotEnv>,
    text: &str,
) -> Result<ClassificationResult> {
    let request = CreateChatCompletionRequestArgs::default()
        .max_tokens(256u16)
        .model("gpt-4")
        .messages([
            ChatCompletionRequestMessageArgs::default()
                .role(async_openai::types::Role::System)
                .content(PROMPT.trim())
                .build()?,
            ChatCompletionRequestMessageArgs::default()
                .role(async_openai::types::Role::User)
                .content(text)
                .build()?,
        ])
        .build()?;
    let response = env.openai_client.chat().create(request).await?;
    let response_text = response
        .choices
        .first()
        .ok_or_else(|| anyhow::anyhow!("Empty list of choices"))?
        .message
        .content
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No content in response"))?
        .as_str();
    if response_text == "\"R\"" {
        return Ok(ClassificationResult::Returned);
    }
    if response_text == "null" {
        return Ok(ClassificationResult::Unknown);
    }
    if let Ok(items) = serde_json::from_str::<Vec<String>>(response_text) {
        if !items.is_empty() {
            return Ok(ClassificationResult::Took(items));
        }
    }
    Ok(ClassificationResult::Unknown)
}

/// Convert a message into a text suitable for `OpenAI` API.
fn textify_message(msg: &Message) -> Option<String> {
    let mut result = String::new();
    match &msg.kind {
        MessageKind::Common(msg_common) => match msg_common.media_kind {
            MediaKind::Photo(_) => result.push_str("[photo]\n"),
            MediaKind::Text(_) => (),
            _ => result.push_str("[media]\n"),
        },
        _ => return None,
    }
    if let Some(text) = msg.text() {
        result.push_str(text);
    }
    if let Some(caption) = msg.caption() {
        result.push_str(caption);
    }
    if result.is_empty() {
        return None;
    }
    Some(result)
}

fn make_text(user: &User, items: &[models::BorrowedItem]) -> String {
    let mut text = String::new();
    let mut prev_date: Option<DateTime<_>> = None;
    for (name, returned) in items
        .iter()
        .filter_map(|i| Some((i.name.as_str(), i.returned?)))
        .sorted_by_key(|(_, r)| *r)
    {
        match prev_date {
            Some(p) if returned - p < chrono::Duration::minutes(10) => {
                text.push_str(", ");
            }
            _ => {
                text.push_str(&returned.format("%Y-%m-%d %H:%M").to_string());
                text.push_str(": returned ");
                prev_date = Some(returned);
            }
        }
        text.push_str(&html::escape(name));
    }
    if text.is_empty() {
        text.push_str(&html::user_mention(user.id.0 as i64, &user.full_name()));
        text.push_str(", press a button to mark an item as returned.");
    }
    text
}

fn make_keyboard(
    chat_id: ChatId,
    user_message_id: MessageId,
    items: &[models::BorrowedItem],
) -> InlineKeyboardMarkup {
    let buttons = items.iter().enumerate().map(|(i, item)| {
        InlineKeyboardButton::callback(
            format!(
                "{} {}",
                if item.returned.is_some() { "✅" } else { "🕐" },
                item.name
            ),
            format!("b:{}:{}:{}", chat_id.0, user_message_id.0, i),
        )
    });
    InlineKeyboardMarkup { inline_keyboard: balance_columns(3, buttons) }
}

fn balance_columns<T>(
    max_columns: usize,
    mut it: impl ExactSizeIterator<Item = T>,
) -> Vec<Vec<T>> {
    let rows = (it.len() + max_columns - 1) / max_columns;
    let columns = it.len() / rows;
    let rows_with_extra_columns = it.len() % rows;

    let mut result = vec![];
    for irow in 0..rows {
        let mut row = Vec::new();
        for _ in 0..(columns + (irow < rows_with_extra_columns) as usize) {
            row.push(it.next().expect(""));
        }
        result.push(row);
    }
    result
}

const PROMPT: &str = r#"""
Classify messages in a thread about taking and returning items.
Respond in a JSON format.

If an user took an item or items, respond with an array of item names, in a nominative case (именительный падеж), e.g. `["hammer","screwdriver"]`.
Do not put an array into an object.
Similar items could be grouped.
Make it concise as possible.
If item name is not clear from the message, use empty string.
Generic item names like "a thing" or "an item" is acceptable.

If an user attaches a photo (denoted by `[photo]`), it is likely that it contains a borrowed item.

If an user returned an item, respond with string `"R"`, but only if an user did not took any.

If a message does not contain any information about taking or returning items, respond with `null`.
"""#;
