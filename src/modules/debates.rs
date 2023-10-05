use std::sync::Arc;

use diesel::prelude::*;
use macro_rules_attribute::derive;
use teloxide::macros::BotCommands;
use teloxide::prelude::*;
use teloxide::types::MessageId;
use teloxide::RequestError;

use crate::common::{
    filter_command, format_users, BotEnv, CommandHandler, HandlerResult,
    MyDialogue, Role, State,
};
use crate::utils::BotExt;
use crate::HasCommandRules;

#[derive(BotCommands, Clone, HasCommandRules!)]
#[command(rename_rule = "snake_case")]
enum DebateCommand {
    #[command(description = "send debate message.")]
    #[custom(in_group = false, role = Role::Resident)]
    DebateSend,

    #[command(description = "debate status.")]
    #[custom(role = Role::Resident)]
    DebateStatus,

    #[command(description = "start debate.")]
    #[custom(in_private = false, role = Role::Admin)]
    DebateStart(String),

    #[command(description = "end debate.")]
    #[custom(in_private = false, role = Role::Admin)]
    DebateEnd,
}

pub fn command_handler() -> CommandHandler<HandlerResult> {
    filter_command::<DebateCommand, _>().endpoint(handle_debate_command)
}

async fn handle_debate_command<'a>(
    bot: Bot,
    dialogue: MyDialogue,
    env: Arc<BotEnv>,
    msg: Message,
    command: DebateCommand,
) -> HandlerResult {
    dialogue.update(State::Start).await?;
    match command {
        DebateCommand::DebateSend => {
            bot.reply_message(&msg, "Now send me a message to forward.")
                .await?;
            dialogue.update(State::Forward).await?;
        }
        DebateCommand::DebateStatus => cmd_debate_status(bot, env, msg).await?,
        DebateCommand::DebateStart(description) => {
            cmd_debate_start(bot, env, msg, description).await?;
        }
        DebateCommand::DebateEnd => cmd_debate_end(bot, env, msg).await?,
    }
    Ok(())
}

async fn cmd_debate_start<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    description: String,
) -> HandlerResult {
    let was_started = env.conn().transaction(|conn| {
        if crate::models::debate.get(conn)?.is_some() {
            Ok(true)
        } else {
            diesel::delete(crate::schema::forwards::table).execute(conn)?;
            crate::models::debate.set(
                conn,
                &crate::models::Debate {
                    description,
                    started_at: chrono::Utc::now(),
                },
            )?;
            QueryResult::Ok(false)
        }
    })?;

    if was_started {
        bot.reply_message(&msg, "Debate already started.").await?;
        return Ok(());
    }

    bot.reply_message(&msg,
        "Debate started. Each participant is expected to send /debate_send command.",
    )
    .await?;
    Ok(())
}

async fn cmd_debate_status<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> HandlerResult {
    let result = env.conn().transaction(|conn| {
        if let Some(debate) = crate::models::debate.get(conn)? {
            let messages = crate::schema::residents::table
                .left_join(
                    crate::schema::forwards::table
                        .on(crate::schema::forwards::columns::orig_chat_id
                            .eq(crate::schema::residents::columns::tg_id)),
                )
                .left_join(
                    crate::schema::tg_users::table
                        .on(crate::schema::tg_users::columns::id
                            .eq(crate::schema::residents::columns::tg_id)),
                )
                .load::<(
                    crate::models::Resident,
                    Option<crate::models::Forward>,
                    Option<crate::models::TgUser>,
                )>(conn)?;
            QueryResult::Ok(Some((debate, messages)))
        } else {
            QueryResult::Ok(None)
        }
    })?;

    fn mk_iter(
        messages: &Vec<(
            crate::models::Resident,
            Option<crate::models::Forward>,
            Option<crate::models::TgUser>,
        )>,
        has_forward: bool,
    ) -> impl Iterator<
        Item = (&crate::models::Resident, &Option<crate::models::TgUser>),
    > {
        messages.into_iter().filter_map(move |(resident, forward, tg_user)| {
            if forward.is_some() == has_forward {
                Some((resident, tg_user))
            } else {
                None
            }
        })
    }

    let text = if let Some((debate, messages)) = result {
        let mut text = String::new();
        text.push_str(
            format!("Debate started at {}.\n", debate.started_at).as_str(),
        );
        text.push_str(
            format!("Description: {}.\n\n", debate.description).as_str(),
        );

        text.push_str("🗳 Sent message: ");
        text.push_str(format_users(mk_iter(&messages, true)).as_str());
        text.push_str(".\n\n");

        text.push_str("🕐 Not yet sent message: ");
        text.push_str(format_users(mk_iter(&messages, false)).as_str());
        text.push_str(".");
        text
    } else {
        "Debate not started yet.".to_string()
    };

    bot.reply_message(&msg, text).await?;

    Ok(())
}

async fn cmd_debate_end<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> HandlerResult {
    let result = env.conn().transaction(|conn| {
        if crate::models::debate.get(conn)?.is_some() {
            let messages = crate::schema::forwards::table
                .load::<crate::models::Forward>(conn)?;
            diesel::delete(crate::schema::forwards::table).execute(conn)?;
            crate::models::debate.unset(conn)?;
            QueryResult::Ok(Some(messages))
        } else {
            QueryResult::Ok(None)
        }
    })?;

    match result {
        Some(messages) => {
            bot.reply_message(&msg, "Debate ended.").await?;
            for debate_msg in messages {
                match bot
                    .forward_message(
                        msg.chat.id,
                        ChatId(debate_msg.orig_chat_id),
                        MessageId(debate_msg.orig_msg_id),
                    )
                    .await
                {
                    Ok(_) => continue,
                    Err(RequestError::Api(
                        teloxide::ApiError::MessageIdInvalid
                        | teloxide::ApiError::MessageToForwardNotFound,
                    )) => { /* ignore */ }
                    Err(e) => {
                        log::error!("Failed to forward message: {}", e);
                    }
                }

                match bot
                    .forward_message(
                        msg.chat.id,
                        ChatId(debate_msg.backup_chat_id),
                        MessageId(debate_msg.backup_msg_id),
                    )
                    .await
                {
                    Ok(_) => continue,
                    Err(RequestError::Api(
                        teloxide::ApiError::MessageIdInvalid
                        | teloxide::ApiError::MessageToForwardNotFound,
                    )) => { /* ignore */ }
                    Err(e) => {
                        log::error!("Failed to forward message: {}", e);
                    }
                }

                // TODO: text
            }
        }
        None => {
            bot.reply_message(&msg, "Debate are not started.").await?;
        }
    }

    Ok(())
}

pub async fn debate_send<'a>(
    bot: Bot,
    dialogue: MyDialogue,
    env: Arc<BotEnv>,
    msg: Message,
) -> HandlerResult {
    if msg.forward().is_some() {
        bot.reply_message(
            &msg,
            "🛑 Error: please, send own message, not a forward.",
        )
        .await?;
        return Ok(());
    }

    let msg_backup = bot
        .forward_message(
            env.config.telegram.forward_channel,
            msg.chat.id,
            msg.id,
        )
        .await?;

    let previous = env.conn().transaction(|conn| {
        let previous = crate::schema::forwards::table
            .filter(crate::schema::forwards::orig_chat_id.eq(msg.chat.id.0))
            .first::<crate::models::Forward>(conn)
            .optional()?;
        diesel::replace_into(crate::schema::forwards::table)
            .values(crate::models::Forward {
                orig_chat_id: msg.chat.id.0,
                orig_msg_id: msg.id.0,

                backup_chat_id: msg_backup.chat.id.0,
                backup_msg_id: msg_backup.id.0,

                // TODO: properly store text
                backup_text: msg_backup.text().unwrap_or_default().to_string(),
            })
            .execute(conn)?;
        QueryResult::Ok(previous)
    })?;

    if let Some(previous) = previous {
        if let Err(e) = bot
            .delete_message(
                ChatId(previous.backup_chat_id),
                MessageId(previous.backup_msg_id),
            )
            .await
        {
            log::warn!("Failed to delete message: {}", e);
        }
        bot.reply_message(&msg, "Forward updated.").await?;
    } else {
        bot.reply_message(&msg, "Saved.").await?;
    }
    dialogue.exit().await?;
    Ok(())
}
