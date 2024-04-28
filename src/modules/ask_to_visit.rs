use std::sync::Arc;

use anyhow::Result;
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use teloxide::requests::Requester;
use teloxide::types::{Message, UserId};
use teloxide::Bot;

use crate::common::{BotEnv, UpdateHandler};
use crate::db::DbUserId;
use crate::schema;
use crate::utils::ResultExt;

pub fn message_handler() -> UpdateHandler {
    dptree::entry().branch(
        dptree::filter(|env: Arc<BotEnv>, msg: Message| {
            env.config.telegram.chats.ask_to_visit.has_message(&msg)
        })
        .endpoint(handle_message),
    )
}

async fn handle_message(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let Some(text) = msg.text() else {
        return Ok(());
    };
    if text.starts_with("//") {
        return Ok(());
    };
    let Some(from) = msg.from else { return Ok(()) };

    let Some(data) = &*env.active_macs.read().await else { return Ok(()) };

    let active_ids: Vec<DbUserId> =
        data.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let residents: Vec<DbUserId> = schema::residents::table
        .filter(schema::residents::tg_id.eq_any(&active_ids))
        .select(schema::residents::tg_id)
        .load(&mut *env.conn())?;

    log::debug!("Found {} residents", residents.len());

    // Check if this message was sent by a resident
    if residents.contains(&DbUserId::from(from.id)) {
        return Ok(());
    }

    for resident in residents {
        bot.forward_message(UserId::from(resident), msg.chat.id, msg.id)
            .await
            .log_error("Failed to forward message to resident");
    }

    Ok(())
}
