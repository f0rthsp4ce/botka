//! Command handling for the NLP module

use std::fmt::Write;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use macro_rules_attribute::derive;
use teloxide::prelude::*;
use teloxide::types::Message;
use teloxide::utils::command::BotCommands;
use tokio::sync::RwLock;

use crate::common::{
    filter_command, is_resident, BotCommandsExt, BotEnv, UpdateHandler,
};
use crate::db::DbChatId;
use crate::models::ChatHistoryEntry;
use crate::modules::basic::cmd_status_text;
use crate::modules::needs::{add_items_text, command_needs_text};
use crate::modules::nlp::types::{AddNeedArgs, NothingArgs};
use crate::modules::{butler, mac_monitoring};

/// Commands for natural language processing
#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(description = "show NLP debug info.")]
    #[custom(
        resident = false,
        admin = false,
        in_private = true,
        in_group = true,
        in_resident_chat = false
    )]
    NlpDebugInfo,
}

/// Command handler for natural language processing debugging
pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(
        |bot: Bot, env: Arc<BotEnv>, msg: Message, cmd: Commands| async move {
            handle_command(bot, env, msg, cmd).await
        },
    )
}

/// Main command handler
pub async fn handle_command(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    command: Commands,
) -> Result<()> {
    match command {
        Commands::NlpDebugInfo => {
            handle_nlp_debug_info(&bot, env, &msg)
                .await
                .context("Failed to handle NLP debug info")?;
        }
    }
    Ok(())
}

/// Handle the NLP debug info command
async fn handle_nlp_debug_info(
    bot: &Bot,
    env: Arc<BotEnv>,
    msg: &Message,
) -> Result<()> {
    // Get replied message info
    let Some(replied_msg) = msg.reply_to_message() else {
        bot.send_message(
            msg.chat.id,
            "Please reply to a message to get debug info.",
        )
        .reply_to_message_id(msg.id)
        .send()
        .await?;
        return Ok(());
    };

    // Load message from database
    let stored_message = {
        match env.transaction(|conn| {
            crate::schema::chat_history::table
                .filter(
                    crate::schema::chat_history::message_id
                        .eq::<i32>(replied_msg.id.0),
                )
                .filter(
                    crate::schema::chat_history::chat_id
                        .eq(DbChatId::from(msg.chat.id)),
                )
                .first::<ChatHistoryEntry>(conn)
        }) {
            Ok(entry) => entry,
            Err(diesel::result::Error::NotFound) => {
                bot.send_message(msg.chat.id, "Message not found in database.")
                    .reply_to_message_id(msg.id)
                    .send()
                    .await?;
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
    };

    // Send debug info
    let mut response = format!("Debug info for message {}:\n", replied_msg.id);
    writeln!(
        response,
        "Classification result: {}",
        stored_message.classification_result.map_or_else(
            || "None".to_string(),
            |classification| classification
        )
    )?;
    writeln!(
        response,
        "Used model: {}",
        stored_message
            .used_model
            .map_or_else(|| "None".to_string(), |model| model)
    )?;

    bot.send_message(msg.chat.id, response)
        .reply_to_message_id(msg.id)
        .send()
        .await?;

    Ok(())
}

/// Handle execution of the "status" command
pub async fn handle_status_command(
    env: &Arc<BotEnv>,
    mac_state: &Arc<RwLock<mac_monitoring::State>>,
    _args: &NothingArgs,
) -> Result<String> {
    log::debug!("Executing command: status");

    match cmd_status_text(env, mac_state).await {
        Ok(text) => Ok(text),
        Err(e) => {
            log::error!("Error executing status command: {e}");
            Err(anyhow::anyhow!("Error executing status command: {e}"))
        }
    }
}

/// Handle execution of the "needs" command
pub fn handle_needs_command(
    env: &Arc<BotEnv>,
    msg: &Message,
    _args: &NothingArgs,
) -> Result<String> {
    log::debug!("Executing command: needs");

    // Check if user is a resident
    if !is_resident(&mut env.conn(), &msg.from.as_ref().unwrap().clone()) {
        return Err(anyhow::anyhow!(
            "Non-resident users cannot use the needs command."
        ));
    }

    // Handle needs command
    match command_needs_text(env) {
        Ok(text) => Ok(text),
        Err(e) => {
            log::error!("Error executing needs command: {e}");
            Err(anyhow::anyhow!("Error executing needs command: {e}"))
        }
    }
}

/// Handle execution of the "need" command
pub async fn handle_add_need_command(
    bot: &Bot,
    env: &Arc<BotEnv>,
    msg: &Message,
    args: &AddNeedArgs,
) -> Result<String> {
    log::debug!("Executing command: add_need");

    // Check if user is a resident
    if !is_resident(&mut env.conn(), &msg.from.as_ref().unwrap().clone()) {
        return Err(anyhow::anyhow!(
            "Non-resident users cannot add items to the shopping list."
        ));
    }

    // Handle need command
    match add_items_text(
        bot,
        env,
        &[&args.item],
        &msg.from.as_ref().unwrap().clone(),
    )
    .await
    {
        Ok(text) => Ok(text),
        Err(e) => {
            log::error!("Error executing need command: {e}");
            Err(anyhow::anyhow!("Error executing need command: {e}"))
        }
    }
}

/// Handle execution of the "open" command
pub async fn handle_open_door_command(
    bot: &Bot,
    env: &Arc<BotEnv>,
    msg: &Message,
    _args: &NothingArgs,
) -> Result<String> {
    log::debug!("Executing command: open");

    // Check if user is a resident
    if !is_resident(&mut env.conn(), &msg.from.as_ref().unwrap().clone()) {
        return Err(anyhow::anyhow!("Only residents can open the door."));
    }
    // Request door opening with confirmation
    match butler::request_door_open_with_confirmation(
        bot,
        Arc::<BotEnv>::clone(env),
        msg.chat.id,
        msg.thread_id,
        &msg.from.as_ref().unwrap().clone(),
    )
    .await
    {
        Ok(()) => Ok("I've sent a confirmation request to open the door. Please confirm using the buttons.".to_string()),
        Err(e) => {
            log::error!("Error requesting door open: {e}");
            Err(anyhow::anyhow!("Failed to request door opening: {}", e))
        }
    }
}
