use std::fmt::Write;
use std::sync::Arc;

use anyhow::Result;
use diesel::{
    ExpressionMethods, JoinOnDsl, NullableExpressionMethods, OptionalExtension,
    QueryDsl, RunQueryDsl,
};
use itertools::Itertools;
use macro_rules_attribute::derive;
use teloxide::macros::BotCommands;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, Message};
use teloxide::utils::html;

use crate::common::{
    filter_command, format_user, BotEnv, CommandHandler, HasCommandRules,
};
use crate::db::DbUserId;
use crate::utils::{write_message_link, BotExt};
use crate::{models, schema};

#[derive(Debug, BotCommands, Clone, HasCommandRules!)]
#[command(rename_rule = "snake_case")]
enum NeedsCommand {
    #[custom(resident = true)]
    Needs,
}

pub fn message_handler() -> CommandHandler<Result<()>> {
    dptree::entry()
        .branch(filter_command::<NeedsCommand, _>().endpoint(handle_command))
        .branch(
            dptree::filter(|env: Arc<BotEnv>, msg: Message| {
                env.config.telegram.chats.needs.has_message(&msg)
            })
            .endpoint(handle_message),
        )
}

pub fn callback_handler() -> CommandHandler<Result<()>> {
    dptree::filter_map(filter_callbacks).endpoint(handle_callback)
}

async fn handle_message(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let Some(user) = &msg.from else {
        return Ok(());
    };
    let Some(text) = msg.text() else {
        return Ok(());
    };

    let list_items = text
        .lines()
        .filter_map(|l| Some(l.trim().strip_prefix('-')?.trim()))
        .collect_vec();
    if list_items.is_empty() {
        return Ok(());
    }

    diesel::insert_into(schema::needed_items::table)
        .values(
            list_items
                .iter()
                .map(|&item| models::NewNeededItem {
                    request_chat_id: msg.chat.id.into(),
                    request_message_id: msg.id.into(),
                    request_user_id: user.id.into(),
                    pinned_chat_id: msg.chat.id.into(),
                    pinned_message_id: msg.id.into(),
                    buyer_user_id: None,
                    item,
                })
                .collect_vec(),
        )
        .execute(&mut *env.conn())?;

    bot.pin_chat_message(msg.chat.id, msg.id).await?;

    Ok(())
}

async fn handle_command(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    command: NeedsCommand,
) -> Result<()> {
    match command {
        NeedsCommand::Needs => command_needs(bot, env, msg).await,
    }
}

async fn command_needs(bot: Bot, env: Arc<BotEnv>, msg: Message) -> Result<()> {
    let (text, buttons) = command_needs_message_and_buttons(env)?;
    bot.reply_message(&msg, text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true)
        .reply_markup(InlineKeyboardMarkup::new(buttons))
        .await?;
    Ok(())
}

fn command_needs_message_and_buttons(
    env: Arc<BotEnv>,
) -> Result<(String, Vec<Vec<InlineKeyboardButton>>)> {
    let items: Vec<(models::NeededItem2, Option<models::TgUser>)> =
        schema::needed_items::table
            .left_join(
                schema::tg_users::table.on(schema::tg_users::columns::id
                    .eq(schema::needed_items::columns::request_user_id)),
            )
            .filter(schema::needed_items::columns::buyer_user_id.is_null())
            .order_by((
                schema::needed_items::columns::request_chat_id,
                schema::needed_items::columns::request_message_id,
                schema::needed_items::columns::rowid,
            ))
            .select((
                schema::needed_items::all_columns,
                schema::tg_users::all_columns.nullable(),
            ))
            .load(&mut *env.conn())?;

    if items.is_empty() {
        return Ok(("No items needed.".to_string(), Vec::new()));
    }

    let mut text = String::new();
    let mut buttons = Vec::new();

    for (idx1, idx2, (item, user)) in
        subnumerate(items.into_iter(), |(i, _)| {
            (i.request_chat_id, i.request_message_id)
        })
    {
        let mut button_text = String::new();
        write!(text, "{}", idx1 + 1).unwrap();
        write!(button_text, "{}", idx1 + 1).unwrap();
        if let Some(idx2) = idx2 {
            letter_index(&mut text, idx2);
            letter_index(&mut button_text, idx2);
        }

        write!(text, ". {} (", html::escape(&item.item)).unwrap();
        write_message_link(
            &mut text,
            item.request_chat_id,
            item.request_message_id,
        );
        write!(text, "by ").unwrap();
        format_user(&mut text, item.request_user_id, &user, false);
        text.push_str("</a>)\n");

        write!(button_text, ". {}", item.item).unwrap();

        if buttons.is_empty()
            || idx2.is_none()
            || idx2 == Some(0)
            || buttons.last().map_or(false, |row: &Vec<_>| row.len() >= 3)
        {
            buttons.push(vec![]);
        }

        buttons.last_mut().unwrap().push(InlineKeyboardButton::callback(
            button_text,
            format!("n:bought:{}", item.rowid),
        ));
    }

    text.push_str("\nPress a button to mark an item as bought.");

    Ok((text, buttons))
}

#[derive(Debug, Copy, Clone)]
enum CallbackData {
    Bought(i32),
    Undo(i32),
}

fn filter_callbacks(callback: CallbackQuery) -> Option<CallbackData> {
    let data = callback.data.as_ref()?.strip_prefix("n:")?;
    let (prefix, data) = data.split_once(':')?;
    let data = data.parse().ok()?;
    match prefix {
        "bought" => Some(CallbackData::Bought(data)),
        "undo" => Some(CallbackData::Undo(data)),
        _ => None,
    }
}

async fn handle_callback(
    bot: Bot,
    env: Arc<BotEnv>,
    callback: CallbackQuery,
    data: CallbackData,
) -> Result<()> {
    match data {
        CallbackData::Bought(rowid) => {
            handle_callback_bought(bot, env, callback, rowid).await
        }
        CallbackData::Undo(rowid) => {
            handle_callback_undo(bot, env, callback, rowid).await
        }
    }
}

async fn handle_callback_bought(
    bot: Bot,
    env: Arc<BotEnv>,
    callback: CallbackQuery,
    rowid_: i32,
) -> Result<()> {
    let result = env.transaction(|conn| {
        #[allow(clippy::wildcard_imports)]
        use schema::needed_items::dsl::*;

        let item_: Option<models::NeededItem2> = schema::needed_items::table
            .filter(rowid.eq(rowid_))
            .get_result(conn)
            .optional()?;
        let item_ = match item_ {
            None => return Ok(Err("Could not find item.")),
            Some(item_) if item_.buyer_user_id.is_some() => {
                return Ok(Err("Item already bought"))
            }
            Some(item_) => item_,
        };

        diesel::update(schema::needed_items::table)
            .filter(rowid.eq(rowid_))
            .set(buyer_user_id.eq(Some(item_.request_user_id)))
            .execute(conn)?;

        let remaining: i64 = schema::needed_items::table
            .filter(request_chat_id.eq(item_.request_chat_id))
            .filter(request_message_id.eq(item_.request_message_id))
            .filter(buyer_user_id.is_null())
            .count()
            .get_result(conn)?;

        Ok(Ok((item_, remaining > 0)))
    })?;

    let (item, has_more) = match result {
        Ok((item, has_more)) => (item, has_more),
        Err(error) => {
            bot.answer_callback_query(&callback.id).text(error).await?;
            return Ok(());
        }
    };

    bot.answer_callback_query(&callback.id).text("Done!").await?;
    if !has_more {
        bot.unpin_chat_message(item.pinned_chat_id)
            .message_id(item.pinned_message_id.into())
            .await?;
    }

    bot.send_message(
        env.config.telegram.chats.needs.chat,
        format!("{} bought an item {:?}.", callback.from.first_name, item.item),
    )
    .message_thread_id(env.config.telegram.chats.needs.thread)
    .reply_markup(InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("Undo", format!("n:undo:{rowid_}")),
    ]]))
    .await?;

    let (text, buttons) = command_needs_message_and_buttons(env)?;

    if let Some(message) = callback.message {
        bot.edit_message_text(message.chat.id, message.id, text)
            .parse_mode(teloxide::types::ParseMode::Html)
            .disable_web_page_preview(true)
            .reply_markup(InlineKeyboardMarkup::new(buttons))
            .await?;
    }
    Ok(())
}

async fn handle_callback_undo(
    bot: Bot,
    env: Arc<BotEnv>,
    callback: CallbackQuery,
    rowid_: i32,
) -> Result<()> {
    let result = env.transaction(|conn| {
        #[allow(clippy::wildcard_imports)]
        use schema::needed_items::dsl::*;

        let item_: Option<models::NeededItem2> = schema::needed_items::table
            .filter(rowid.eq(rowid_))
            .get_result(conn)
            .optional()?;
        let item_ = match item_ {
            None => return Ok(Err("Could not find item.")),
            Some(models::NeededItem2 { buyer_user_id: None, .. }) => {
                return Ok(Err("Item already undone."))
            }
            Some(models::NeededItem2 { buyer_user_id: Some(id), .. })
                if UserId::from(id) != callback.from.id =>
            {
                return Ok(Err("You did not buy this item."))
            }
            Some(item_) => item_,
        };

        let remaining_before_undoing: i64 = schema::needed_items::table
            .filter(request_chat_id.eq(item_.request_chat_id))
            .filter(request_message_id.eq(item_.request_message_id))
            .filter(buyer_user_id.is_null())
            .count()
            .get_result(conn)?;

        diesel::update(schema::needed_items::table)
            .filter(rowid.eq(rowid_))
            .set(buyer_user_id.eq(None::<DbUserId>))
            .execute(conn)?;

        Ok(Ok((item_, remaining_before_undoing == 0)))
    })?;

    let (item, was_all_bought) = match result {
        Ok((item, was_all_bought)) => (item, was_all_bought),
        Err(error) => {
            bot.answer_callback_query(&callback.id).text(error).await?;
            return Ok(());
        }
    };

    if was_all_bought {
        bot.pin_chat_message(
            item.pinned_chat_id,
            item.pinned_message_id.into(),
        )
        .await?;
    }

    if let Some(cb_message) = callback.message {
        bot.delete_message(cb_message.chat.id, cb_message.id).await?;
    }

    Ok(())
}

/// Enumerate items, but sub-enumerate items with the same id.
/// For stray items, the second index is None.
fn subnumerate<'a, T: Clone + 'a, I: Copy + PartialEq + 'a>(
    items: impl Iterator<Item = T> + 'a,
    mut to_id: impl FnMut(T) -> I + Copy + 'a,
) -> impl Iterator<Item = (usize, Option<usize>, T)> + 'a {
    let mut index1 = 0;
    let mut index2 = 0;
    let mut id_prev = None;
    let mut items_peekable = items.peekable();
    std::iter::from_fn(move || {
        items_peekable.next().map(|item| {
            let id_this = to_id(item.clone());
            let id_next = items_peekable.peek().cloned().map(to_id);
            if id_prev == Some(id_this) {
                index2 += 1;
            } else {
                index1 += 1;
                index2 = usize::from(Some(id_this) == id_next);
            }
            id_prev = Some(id_this);
            (
                index1 - 1,
                if index2 == 0 { None } else { Some(index2 - 1) },
                item,
            )
        })
    })
}

fn letter_index(out: &mut String, index: usize) {
    let out_len = out.len();
    let mut r = index;
    loop {
        let (r1, d) = (r / 26, r % 26);
        let c = std::char::from_u32(u32::try_from(d).unwrap() + 'a' as u32)
            .unwrap();
        out.push(c);
        if r1 == 0 {
            unsafe { out[out_len..].as_bytes_mut() }.reverse();
            return;
        }
        r = r1 - 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subnumerate() {
        let to_id = |i: &str| i.chars().next().unwrap();

        assert_eq!(subnumerate([].into_iter(), to_id).collect_vec(), vec![]);

        assert_eq!(
            subnumerate(["a0"].into_iter(), to_id).collect_vec(),
            vec![(0, None, "a0")]
        );

        assert_eq!(
            subnumerate(["a0", "b0"].into_iter(), to_id).collect_vec(),
            vec![(0, None, "a0"), (1, None, "b0")]
        );

        assert_eq!(
            subnumerate(["a0", "b0", "c0"].into_iter(), to_id).collect_vec(),
            vec![(0, None, "a0"), (1, None, "b0"), (2, None, "c0")]
        );

        assert_eq!(
            subnumerate(["a0", "b0", "b1", "b2", "c0"].into_iter(), to_id)
                .collect_vec(),
            vec![
                (0, None, "a0"),
                (1, Some(0), "b0"),
                (1, Some(1), "b1"),
                (1, Some(2), "b2"),
                (2, None, "c0")
            ]
        );

        assert_eq!(
            subnumerate(["a0", "a1", "b0"].into_iter(), to_id).collect_vec(),
            vec![(0, Some(0), "a0"), (0, Some(1), "a1"), (1, None, "b0"),]
        );

        assert_eq!(
            subnumerate(["a0", "b0", "b1"].into_iter(), to_id).collect_vec(),
            vec![(0, None, "a0"), (1, Some(0), "b0"), (1, Some(1), "b1"),]
        );
    }

    #[test]
    fn test_letter_index() {
        let mut str = ".".to_string();
        letter_index(&mut str, 0);
        assert_eq!(str, ".a");

        let mut str = ".".to_string();
        letter_index(&mut str, 1);
        assert_eq!(str, ".b");

        let mut str = ".".to_string();
        letter_index(&mut str, 25);
        assert_eq!(str, ".z");

        let mut str = ".".to_string();
        letter_index(&mut str, 26);
        assert_eq!(str, ".aa");
    }
}
