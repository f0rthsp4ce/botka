use std::sync::Arc;

use anyhow::Result;
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use futures::StreamExt;
use itertools::{EitherOrBoth, Itertools};
use teloxide::prelude::*;
use teloxide::types::MessageId;
use teloxide::{ApiError, RequestError};

use crate::common::BotEnv;
use crate::db::{DbChatId, DbMessageId, DbThreadId};
use crate::models::NewDashboardMessage;
use crate::utils::{ResultExt, ThreadIdPair};

pub async fn update(
    bot: &Bot,
    env: Arc<BotEnv>,
    thread: ThreadIdPair,
    new_messages: &[&str],
) -> Result<()> {
    if update_once(bot, Arc::clone(&env), thread, new_messages).await? {
        log::info!("Resetting dashboard");
        reset(bot, Arc::clone(&env), thread).await?;
        if update_once(bot, Arc::clone(&env), thread, new_messages).await? {
            anyhow::bail!("Dashboard reset failed");
        }
    }
    Ok(())
}

async fn update_once(
    bot: &Bot,
    env: Arc<BotEnv>,
    thread: ThreadIdPair,
    new_messages: &[&str],
) -> Result<bool> {
    let old_messages: Vec<(DbMessageId, String)> = {
        use crate::schema::dashboard_messages::dsl as d;
        d::dashboard_messages
            .filter(d::chat_id.eq(DbChatId::from(thread.chat)))
            .filter(d::thread_id.eq(DbThreadId::from(thread.thread)))
            .order(d::message_id.asc())
            .select((d::message_id, d::text))
            .load(&mut *env.conn.lock().unwrap())?
    };

    let (unordered, ordered) = old_messages
        .iter()
        .zip_longest(new_messages.iter().copied())
        .partition::<Vec<_>, _>(|it| !matches!(it, EitherOrBoth::Right(_)));

    let mut errors = Vec::new();

    let unordered = futures::future::join_all(
        unordered
            .into_iter()
            .map(|it| handle_pair(bot, Arc::clone(&env), thread, it)),
    )
    .await
    .into_iter();

    let ordered = futures::stream::iter(ordered)
        .then(|it| handle_pair(bot, Arc::clone(&env), thread, it))
        .collect::<Vec<_>>()
        .await
        .into_iter();

    let mut need_reset = false;
    for it in unordered.chain(ordered) {
        match it {
            Ok(true) => need_reset = true,
            Ok(false) => (),
            Err(e) => errors.push(e),
        }
    }

    if !errors.is_empty() {
        log::error!("Dashboard update errors: {errors:#?}");
        anyhow::bail!("Dashboard update errors");
    }

    Ok(need_reset)
}

async fn reset(
    bot: &Bot,
    env: Arc<BotEnv>,
    thread: ThreadIdPair,
) -> Result<()> {
    let old_messages: Vec<DbMessageId> = {
        use crate::schema::dashboard_messages::dsl as d;
        d::dashboard_messages
            .filter(d::chat_id.eq(DbChatId::from(thread.chat)))
            .filter(d::thread_id.eq(DbThreadId::from(thread.thread)))
            .select(d::message_id)
            .load(&mut *env.conn.lock().unwrap())?
    };

    let has_errors = old_messages
        .into_iter()
        .map(|it| {
            let env = Arc::clone(&env);
            async move {
                bot.delete_message(thread.chat, MessageId::from(it))
                    .await
                    .log_error("Failed to delete message");
                use crate::schema::dashboard_messages::dsl as d;
                let err = diesel::delete(
                    d::dashboard_messages
                        .filter(d::chat_id.eq(DbChatId::from(thread.chat)))
                        .filter(
                            d::thread_id.eq(DbThreadId::from(thread.thread)),
                        )
                        .filter(d::message_id.eq(it)),
                )
                .execute(&mut *env.conn.lock().unwrap())
                .err();
                if let Some(ref err) = err {
                    log::error!("Failed to delete dashboard item: {err}");
                }
                err.is_some()
            }
        })
        .collect::<futures::stream::FuturesUnordered<_>>()
        .any(|it| async move { it })
        .await;

    if has_errors {
        anyhow::bail!("Dashboard reset errors");
    }

    Ok(())
}

async fn handle_pair(
    bot: &Bot,
    env: Arc<BotEnv>,
    thread: ThreadIdPair,
    it: EitherOrBoth<&(DbMessageId, String), &str>,
) -> Result<bool> {
    match it {
        EitherOrBoth::Both((_, old_text), new_text) if old_text == new_text => {
            // Nothing to do
        }
        EitherOrBoth::Both((msg_id, _), new_text) => {
            match bot
                .edit_message_text(
                    thread.chat,
                    MessageId::from(*msg_id),
                    new_text,
                )
                .parse_mode(teloxide::types::ParseMode::Html)
                .disable_web_page_preview(true)
                .await
            {
                Err(RequestError::Api(ApiError::MessageNotModified)) => (),
                Err(RequestError::Api(_)) => return Ok(true),
                a => {
                    a?;
                }
            }

            use crate::schema::dashboard_messages::dsl as d;
            diesel::replace_into(d::dashboard_messages)
                .values(NewDashboardMessage {
                    chat_id: DbChatId::from(thread.chat),
                    thread_id: DbThreadId::from(thread.thread),
                    message_id: *msg_id,
                    text: new_text,
                })
                .execute(&mut *env.conn.lock().unwrap())?;
        }
        EitherOrBoth::Left((msg_id, _)) => {
            match bot
                .delete_message(thread.chat, MessageId::from(*msg_id))
                .await
            {
                Err(RequestError::Api(ApiError::MessageToDeleteNotFound)) => (),
                Err(RequestError::Api(_)) => return Ok(true),
                a => {
                    a?;
                }
            }
            use crate::schema::dashboard_messages::dsl as d;
            diesel::delete(
                d::dashboard_messages
                    .filter(d::chat_id.eq(DbChatId::from(thread.chat)))
                    .filter(d::thread_id.eq(DbThreadId::from(thread.thread)))
                    .filter(d::message_id.eq(msg_id)),
            )
            .execute(&mut *env.conn.lock().unwrap())?;
        }
        EitherOrBoth::Right(new_text) => {
            let msg = bot
                .send_message(thread.chat, new_text)
                .message_thread_id(thread.thread)
                .parse_mode(teloxide::types::ParseMode::Html)
                .disable_web_page_preview(true)
                .disable_notification(true)
                .await?;
            use crate::schema::dashboard_messages::dsl as d;
            diesel::replace_into(d::dashboard_messages)
                .values(NewDashboardMessage {
                    chat_id: DbChatId::from(thread.chat),
                    thread_id: DbThreadId::from(thread.thread),
                    message_id: DbMessageId::from(msg.id),
                    text: new_text,
                })
                .execute(&mut *env.conn.lock().unwrap())?;
        }
    }
    Ok(false)
}
