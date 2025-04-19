//! Butler module for door opening functionality.

use std::sync::Arc;

use anyhow::Result;
use macro_rules_attribute::derive;
use teloxide::payloads::SendMessageSetters;
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, ParseMode,
    ThreadId, User,
};
use teloxide::utils::command::BotCommands;

use crate::common::{filter_command, BotCommandsExt, BotEnv, UpdateHandler};
use crate::utils::{BotExt, MessageExt};

#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(description = "open the door")]
    #[custom(resident = true)]
    Open,
}

pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(handle_command)
}

pub fn callback_handler() -> UpdateHandler {
    dptree::filter_map(filter_callbacks).endpoint(handle_callback)
}

async fn handle_command(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    command: Commands,
) -> Result<()> {
    match command {
        Commands::Open => cmd_open(bot, env, msg).await,
    }
}

/// Process door open command
async fn cmd_open(bot: Bot, env: Arc<BotEnv>, msg: Message) -> Result<()> {
    if env.config.services.butler.is_none() {
        bot.reply_message(&msg, "Door opening is not configured.").await?;
        return Ok(());
    }

    let user = msg.from.clone().expect("from user");

    // Check if user is a resident
    if !crate::common::is_resident(&mut env.conn(), &user) {
        bot.reply_message(&msg, "Only residents can open the door.").await?;
        return Ok(());
    }

    request_door_open_with_confirmation(
        &bot,
        Arc::<BotEnv>::clone(&env),
        msg.chat.id,
        msg.thread_id_ext(),
        &user,
    )
    .await?;

    Ok(())
}

/// Execute the actual door opening request
async fn execute_door_open(
    url: String,
    token: String,
    user: &User,
) -> Result<()> {
    let client = reqwest::Client::new();

    // Get the username or fallback to full name if username is not available
    let username = user.username.clone().unwrap_or_else(|| user.full_name());

    let response = client
        .post(url)
        .header("Cookie", format!("ses={token}"))
        .form(&[("username", username)]) // Add username as a POST parameter
        .send()
        .await?;

    if !response.status().is_success() {
        anyhow::bail!("Request failed with status: {}", response.status());
    }

    Ok(())
}

/// Format for the confirmation callback data
#[derive(Debug, Clone)]
enum CallbackAction {
    ConfirmOpen,
    CancelOpen,
}

/// Filter callback queries for door opening confirmation
fn filter_callbacks(callback: CallbackQuery) -> Option<CallbackAction> {
    let data = callback.data.as_ref()?;

    match data.as_str() {
        "butler:confirm_open" => Some(CallbackAction::ConfirmOpen),
        "butler:cancel_open" => Some(CallbackAction::CancelOpen),
        _ => None,
    }
}

/// Handle callback queries for door opening confirmation
async fn handle_callback(
    bot: Bot,
    env: Arc<BotEnv>,
    callback: CallbackQuery,
    action: CallbackAction,
) -> Result<()> {
    let Some(msg) = &callback.message else {
        return Ok(());
    };

    // Check if user is a resident
    if !crate::common::is_resident(&mut env.conn(), &callback.from) {
        bot.answer_callback_query(&callback.id)
            .text("Only residents can interact with this.")
            .await?;
        return Ok(());
    }

    match action {
        CallbackAction::ConfirmOpen => {
            // Execute the door opening
            let Some(butler_config) = &env.config.services.butler else {
                bot.answer_callback_query(&callback.id)
                    .text("Door opening is not configured.")
                    .await?;
                return Ok(());
            };

            log::warn!(
                "Opening door for {} ({}, @{})",
                callback.from.full_name(),
                callback.from.id,
                callback.from.username.clone().unwrap_or_default()
            );

            match execute_door_open(
                butler_config.url.clone(),
                butler_config.token.clone(),
                &callback.from, // Pass the user reference to execute_door_open
            )
            .await
            {
                Ok(()) => {
                    // Update the message
                    bot.edit_message_text(
                        msg.chat.id,
                        msg.id,
                        "✅ Door opened successfully!",
                    )
                    .await?;

                    bot.answer_callback_query(&callback.id)
                        .text("Door opened successfully!")
                        .await?;
                }
                Err(e) => {
                    log::error!("Failed to open door: {e}");

                    bot.answer_callback_query(&callback.id)
                        .text("Failed to open door. Please try again.")
                        .await?;
                }
            }
        }
        CallbackAction::CancelOpen => {
            // Update the message to show cancellation
            bot.edit_message_text(
                msg.chat.id,
                msg.id,
                "❌ Door opening cancelled.",
            )
            .await?;

            bot.answer_callback_query(&callback.id)
                .text("Door opening cancelled.")
                .await?;
        }
    }

    Ok(())
}

/// Request a door opening with confirmation
pub async fn request_door_open_with_confirmation(
    bot: &Bot,
    env: Arc<BotEnv>,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    from_user: &User,
) -> Result<()> {
    // Check if user is a resident
    if !crate::common::is_resident(&mut env.conn(), from_user) {
        bot.send_message(chat_id, "Only residents can open the door.").await?;
        return Ok(());
    }

    let keyboard = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("✅ Confirm", "butler:confirm_open"),
        InlineKeyboardButton::callback("❌ Cancel", "butler:cancel_open"),
    ]]);

    let text = format!(
        "🚪 <b>Door Opening Request</b>\n\nDo you want to open the door? This action will be logged.\n\nRequested by: {}",
        from_user.full_name()
    );

    let mut msg_builder = bot
        .send_message(chat_id, text)
        .parse_mode(ParseMode::Html)
        .reply_markup(keyboard);

    if let Some(thread) = thread_id {
        msg_builder = msg_builder.message_thread_id(thread);
    }

    msg_builder.await?;

    Ok(())
}
