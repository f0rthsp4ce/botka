use std::fmt::Write;
use std::sync::Arc;

use anyhow::Result;
use diesel::prelude::*;
use macro_rules_attribute::derive;
use teloxide::macros::BotCommands;
use teloxide::prelude::*;
use teloxide::utils::html;
use teloxide::RequestError;

use crate::common::{
    filter_command, format_users, BotEnv, CommandHandler, MyDialogue, State,
};
use crate::db::DbUserId;
use crate::utils::BotExt;
use crate::HasCommandRules;

#[derive(BotCommands, Clone, HasCommandRules!)]
#[command(rename_rule = "snake_case")]
#[allow(clippy::enum_variant_names)]
enum DebateCommand {
    #[command(description = "send debate message.")]
    #[custom(in_group = false, resident = true)]
    DebateSend,

    #[command(description = "debate status.")]
    #[custom(resident = true)]
    DebateStatus,

    #[command(description = "start debate.")]
    #[custom(in_private = false, admin = true)]
    DebateStart(String),

    #[command(description = "end debate.")]
    #[custom(in_private = false, admin = true)]
    DebateEnd,
}

pub fn command_handler() -> CommandHandler<Result<()>> {
    filter_command::<DebateCommand, _>().endpoint(handle_debate_command)
}

async fn handle_debate_command<'a>(
    bot: Bot,
    dialogue: MyDialogue,
    env: Arc<BotEnv>,
    msg: Message,
    command: DebateCommand,
) -> Result<()> {
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
) -> Result<()> {
    let was_started = env.transaction(|conn| {
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
            Ok(false)
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
) -> Result<()> {
    let result = env.transaction(|conn| {
        if let Some(debate) = crate::models::debate.get(conn)? {
            let messages: Vec<(
                DbUserId,
                Option<crate::models::Forward>,
                Option<crate::models::TgUser>,
            )> = crate::schema::residents::table
                .filter(crate::schema::residents::end_date.is_null())
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
                .select((
                    crate::schema::residents::tg_id,
                    crate::schema::forwards::all_columns.nullable(),
                    crate::schema::tg_users::all_columns.nullable(),
                ))
                .load(conn)?;
            Ok(Some((debate, messages)))
        } else {
            Ok(None)
        }
    })?;

    fn mk_iter(
        messages: &[(
            DbUserId,
            Option<crate::models::Forward>,
            Option<crate::models::TgUser>,
        )],
        has_forward: bool,
    ) -> impl Iterator<Item = (DbUserId, &Option<crate::models::TgUser>)> {
        messages.iter().filter_map(move |(resident_id, forward, tg_user)| {
            if forward.is_some() == has_forward {
                Some((*resident_id, tg_user))
            } else {
                None
            }
        })
    }

    let text = if let Some((debate, messages)) = result {
        let mut text = String::new();
        writeln!(text, "Debate started at {}.", debate.started_at).unwrap();
        write!(text, "Description: {}.\n\n", html::escape(&debate.description))
            .unwrap();

        text.push_str("🗳 Sent message: ");
        format_users(&mut text, mk_iter(&messages, true));
        text.push_str(".\n\n");

        text.push_str("🕐 Not yet sent message: ");
        format_users(&mut text, mk_iter(&messages, false));
        text.push('.');
        text
    } else {
        "Debate not started yet.".to_string()
    };

    bot.reply_message(&msg, text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true)
        .await?;

    Ok(())
}

async fn cmd_debate_end<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let result = env.transaction(|conn| {
        if crate::models::debate.get(conn)?.is_some() {
            let messages = crate::schema::forwards::table
                .load::<crate::models::Forward>(conn)?;
            diesel::delete(crate::schema::forwards::table).execute(conn)?;
            crate::models::debate.unset(conn)?;
            Ok(Some(messages))
        } else {
            Ok(None)
        }
    })?;

    match result {
        Some(messages) => {
            bot.reply_message(&msg, "Debate ended.").await?;
            for debate_msg in messages {
                match bot
                    .forward_message(
                        msg.chat.id,
                        debate_msg.orig_chat_id,
                        debate_msg.orig_msg_id.into(),
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
                        debate_msg.backup_chat_id,
                        debate_msg.backup_msg_id.into(),
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
) -> Result<()> {
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

    let previous = env.transaction(|conn| {
        let previous = crate::schema::forwards::table
            .filter(crate::schema::forwards::orig_chat_id.eq(msg.chat.id.0))
            .first::<crate::models::Forward>(conn)
            .optional()?;
        diesel::replace_into(crate::schema::forwards::table)
            .values(crate::models::Forward {
                orig_chat_id: msg.chat.id.into(),
                orig_msg_id: msg.id.into(),

                backup_chat_id: msg_backup.chat.id.into(),
                backup_msg_id: msg_backup.id.into(),

                // TODO: properly store text
                backup_text: msg_backup.text().unwrap_or_default().to_string(),
            })
            .execute(conn)?;
        Ok(previous)
    })?;

    if let Some(previous) = previous {
        if let Err(e) = bot
            .delete_message(
                previous.backup_chat_id,
                previous.backup_msg_id.into(),
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
