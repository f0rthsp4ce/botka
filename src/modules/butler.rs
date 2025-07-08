//! Butler module for door opening functionality.

use std::convert::TryFrom;
use std::sync::Arc;

use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl};
use macro_rules_attribute::derive;
use rand::RngCore;
use teloxide::payloads::SendMessageSetters;
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, ParseMode,
    ThreadId, User,
};
use teloxide::utils::command::BotCommands;
use tokio::sync::RwLock;

use crate::common::{filter_command, BotCommandsExt, BotEnv, UpdateHandler};
use crate::models::NewTempOpenToken;
use crate::utils::{BotExt, MessageExt};
use crate::modules::mac_monitoring::State;

#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(description = "open the door")]
    #[custom(resident = true)]
    Open,
    #[command(description = "generate a temporary guest door access link")]
    #[custom(resident = true)]
    TempOpen,
}

pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(
        |bot: Bot, env: Arc<BotEnv>, msg: Message, mac_monitoring_state: Arc<RwLock<State>>, cmd: Commands| async move {
            match cmd {
                Commands::Open => cmd_open(bot, env, msg, mac_monitoring_state).await,
                Commands::TempOpen => cmd_temp_open(bot, env, msg, mac_monitoring_state).await,
            }
        },
    )
}

pub fn callback_handler() -> UpdateHandler {
    dptree::filter_map(filter_callbacks).endpoint(
        |bot: Bot, env: Arc<BotEnv>, callback: CallbackQuery, mac_monitoring_state: Arc<RwLock<State>>, action: CallbackAction| async move {
            handle_callback(bot, env, callback, mac_monitoring_state, action).await
        }
    )
}

fn user_id_to_i64(user_id: teloxide::types::UserId) -> Option<i64> {
    i64::try_from(user_id.0).ok()
}
fn i64_to_user_id(id: i64) -> Option<teloxide::types::UserId> {
    u64::try_from(id).ok().map(teloxide::types::UserId)
}

/// Check if a user is a guest with a valid `temp_open` token
fn guest_can_open(env: &Arc<BotEnv>, user_id: i64) -> Option<i64> {
    use crate::schema::temp_open_tokens::dsl as t;
    let now = chrono::Utc::now().naive_utc();
    t::temp_open_tokens
        .filter(t::guest_tg_id.eq(user_id))
        .filter(t::expires_at.gt(now))
        .first::<crate::models::TempOpenToken>(&mut *env.conn())
        .optional()
        .ok()
        .flatten()
        .map(|r| r.resident_tg_id)
}

/// Process door open command (resident or guest)
async fn cmd_open(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    mac_monitoring_state: Arc<RwLock<State>>,
) -> anyhow::Result<()> {
    if env.config.services.butler.is_none() {
        bot.reply_message(&msg, "Door opening is not configured.").await?;
        return Ok(());
    }
    let Some(user) = msg.from.clone() else { return Ok(()) };
    let user_id = user_id_to_i64(user.id)
        .ok_or_else(|| anyhow::anyhow!("User ID out of range"))?;
    let is_resident = crate::common::is_resident(&mut env.conn(), &user);
    if !is_resident {
        // Check if guest with valid token
        if let Some(resident_id) = guest_can_open(&env, user_id) {
            // Check if inviter is on Wi-Fi
            if let Some(inviter_uid) = i64_to_user_id(resident_id) {
                if !mac_monitoring_state
                    .read()
                    .await
                    .active_users()
                    .is_some_and(|set| set.contains(&inviter_uid))
                {
                    log::debug!("Guest {} ({}) blocked: inviter {} not on Wi-Fi. Active users: {:?}", 
                        user.full_name(), user.id, inviter_uid, 
                        mac_monitoring_state.read().await.active_users().map(|set| set.len()));
                    bot.reply_message(&msg, "The resident who invited you is not currently on Wi-Fi. Door cannot be opened.").await?;
                    return Ok(());
                }
            } else {
                bot.reply_message(&msg, "Inviter ID is invalid.").await?;
                return Ok(());
            }
            log::info!(
                "Guest {} ({}) used temp_open (inviter: {})",
                user.full_name(),
                user.id,
                resident_id
            );
        } else {
            bot.reply_message(&msg, "Only residents or guests with a valid access link can open the door.").await?;
            return Ok(());
        }
    }
    if is_resident {
        log::info!(
            "Resident {} ({}) opened the door",
            user.full_name(),
            user.id
        );
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

/// Handle /`temp_open` command
async fn cmd_temp_open(
    bot: Bot,
    _env: Arc<BotEnv>,
    msg: Message,
    mac_monitoring_state: Arc<RwLock<State>>,
) -> anyhow::Result<()> {
    let Some(user) = msg.from.clone() else { return Ok(()) };
    // Check Wi-Fi presence (active MAC)
    let is_online = {
        let guard = mac_monitoring_state.read().await;
        if let Some(set) = guard.active_users() { set.contains(&user.id) } else {
            log::info!("MAC monitoring state not initialized for user {} ({})", 
                user.full_name(), user.id);
            // Don't allow access if monitoring system is not initialized yet
            bot.reply_message(&msg, "MAC monitoring system is initializing. Please try again in a few moments.").await?;
            return Ok(());
        }
    };
    if !is_online {
        log::debug!("User {} ({}) blocked from temp_open: not on Wi-Fi. Active users: {:?}", 
            user.full_name(), user.id, 
            mac_monitoring_state.read().await.active_users().map(|set| set.len()));
        bot.reply_message(&msg, "You must be connected to the hackerspace Wi-Fi to generate a temporary access link.").await?;
        return Ok(());
    }

    // Show duration selection buttons
    let keyboard = InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("5 minutes", "butler:temp_open:5"),
            InlineKeyboardButton::callback("15 minutes", "butler:temp_open:15"),
        ],
        vec![
            InlineKeyboardButton::callback("30 minutes", "butler:temp_open:30"),
            InlineKeyboardButton::callback("1 hour", "butler:temp_open:60"),
        ],
    ]);

    let text = "🕒 Select duration for temporary guest access:";

    bot.reply_message(&msg, text).reply_markup(keyboard).await?;

    Ok(())
}

/// Execute the actual door opening request
async fn execute_door_open(
    url: String,
    token: String,
    user: &User,
) -> anyhow::Result<()> {
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
#[allow(clippy::enum_variant_names)]
enum CallbackAction {
    ConfirmOpen,
    CancelOpen,
    TempOpen(u32), // Duration in minutes
}

/// Filter callback queries for door opening confirmation
fn filter_callbacks(callback: CallbackQuery) -> Option<CallbackAction> {
    let data = callback.data.as_ref()?;

    match data.as_str() {
        "butler:confirm_open" => Some(CallbackAction::ConfirmOpen),
        "butler:cancel_open" => Some(CallbackAction::CancelOpen),
        data if data.starts_with("butler:temp_open:") => data
            .strip_prefix("butler:temp_open:")
            .and_then(|duration_str| duration_str.parse::<u32>().ok())
            .map(CallbackAction::TempOpen),
        _ => None,
    }
}

/// Handle callback queries for door opening confirmation
#[allow(clippy::too_many_lines)]
async fn handle_callback(
    bot: Bot,
    env: Arc<BotEnv>,
    callback: CallbackQuery,
    mac_monitoring_state: Arc<RwLock<State>>,
    action: CallbackAction,
) -> anyhow::Result<()> {
    let Some(msg) = &callback.message else {
        return Ok(());
    };

    let user_id = user_id_to_i64(callback.from.id)
        .ok_or_else(|| anyhow::anyhow!("User ID out of range"))?;
    let is_resident =
        crate::common::is_resident(&mut env.conn(), &callback.from);

    // Check if user is a resident or guest with valid token
    let can_open = if is_resident {
        true
    } else {
        guest_can_open(&env, user_id).is_some()
    };

    if !can_open {
        bot.answer_callback_query(&callback.id)
            .text("Only residents or guests with valid access can interact with this.")
            .await?;
        return Ok(());
    }

    match action {
        CallbackAction::TempOpen(duration_minutes) => {
            // Only residents can generate temp_open links
            if !is_resident {
                bot.answer_callback_query(&callback.id)
                    .text("Only residents can generate temporary access links.")
                    .await?;
                return Ok(());
            }

            // Check Wi-Fi presence (active MAC)
            let is_online = {
                let guard = mac_monitoring_state.read().await;
                if let Some(set) = guard.active_users() { set.contains(&callback.from.id) } else {
                    log::info!("MAC monitoring state not initialized for user {} ({})", 
                        callback.from.full_name(), callback.from.id);
                    bot.answer_callback_query(&callback.id)
                        .text("MAC monitoring system is initializing. Please try again in a few moments.")
                        .show_alert(true)
                        .await?;
                    return Ok(());
                }
            };
            if !is_online {
                log::debug!("User {} ({}) blocked from temp_open callback: not on Wi-Fi. Active users: {:?}", 
                    callback.from.full_name(), callback.from.id, 
                    mac_monitoring_state.read().await.active_users().map(|set| set.len()));
                bot.answer_callback_query(&callback.id)
                    .text("You must be connected to the hackerspace Wi-Fi to generate a temporary access link.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            }

            // Generate token and create link
            let token = generate_token();
            let duration =
                chrono::Duration::minutes(i64::from(duration_minutes));
            let expires_at = chrono::Utc::now().naive_utc() + duration;
            let resident_tg_id = user_id_to_i64(callback.from.id)
                .ok_or_else(|| anyhow::anyhow!("User ID out of range"))?;

            // Store in DB
            env.transaction(|conn| {
                use crate::schema::temp_open_tokens::dsl as t;
                diesel::insert_into(t::temp_open_tokens)
                    .values(&NewTempOpenToken {
                        token: &token,
                        resident_tg_id,
                        expires_at,
                    })
                    .execute(conn)
            })?;

            // Get bot username and create link
            let bot_username =
                bot.get_me().await?.user.username.unwrap_or_default();
            let link =
                format!("https://t.me/{bot_username}?start=temp_open:{token}");
            let url = reqwest::Url::parse(&link).unwrap_or_else(|_| {
                reqwest::Url::parse("https://t.me").unwrap()
            });

            // Update the message with the generated link
            let text = format!("✅ Temporary guest access link (valid for {} min):\n<code>{}</code>", duration.num_minutes(), link);
            let button = InlineKeyboardMarkup::new(vec![vec![
                InlineKeyboardButton::url("Share link", url),
            ]]);

            bot.edit_message_text(msg.chat.id, msg.id, text)
                .parse_mode(ParseMode::Html)
                .reply_markup(button)
                .await?;

            bot.answer_callback_query(&callback.id)
                .text(format!(
                    "Temporary access link generated (valid for {} min)",
                    duration.num_minutes()
                ))
                .await?;

            log::info!(
                "Resident {} ({}) generated temp_open link {} (expires at {})",
                callback.from.full_name(),
                callback.from.id,
                token,
                expires_at
            );
        }
        CallbackAction::ConfirmOpen => {
            // For guests, check if inviter is on Wi-Fi
            if !is_resident {
                if let Some(resident_id) = guest_can_open(&env, user_id) {
                    if let Some(inviter_uid) = i64_to_user_id(resident_id) {
                        if !mac_monitoring_state
                            .read()
                            .await
                            .active_users()
                            .is_some_and(|set| set.contains(&inviter_uid))
                        {
                            log::debug!("Guest {} ({}) blocked: inviter {} not on Wi-Fi. Active users: {:?}", 
                                callback.from.full_name(), callback.from.id, inviter_uid, 
                                mac_monitoring_state.read().await.active_users().map(|set| set.len()));
                            bot.answer_callback_query(&callback.id)
                                .text("The resident who invited you is not currently on Wi-Fi. Door cannot be opened.")
                                .await?;
                            return Ok(());
                        }
                    } else {
                        bot.answer_callback_query(&callback.id)
                            .text("Inviter ID is invalid.")
                            .await?;
                        return Ok(());
                    }
                    log::info!(
                        "Guest {} ({}) used temp_open via button (inviter: {})",
                        callback.from.full_name(),
                        callback.from.id,
                        resident_id
                    );
                }
            }
            if is_resident {
                log::info!(
                    "Resident {} ({}) opened the door via button",
                    callback.from.full_name(),
                    callback.from.id
                );
            }

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
) -> anyhow::Result<()> {
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

/// Generate a random token for temporary access links
fn generate_token() -> String {
    const CHARSET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..32)
        .map(|_| {
            let idx = (rng.next_u32() as usize) % CHARSET.len();
            CHARSET[idx] as char
        })
        .collect()
}

/// Handler for guest activation via /start `temp_open`:<token>
pub fn guest_token_handler() -> crate::common::UpdateHandler {
    use std::sync::Arc;

    use teloxide::prelude::*;
    use teloxide::types::Message;

    use crate::common::BotEnv;

    Update::filter_message().filter_map(|msg: Message| {
        let text = msg.text().unwrap_or("");
        text.strip_prefix("/start temp_open:").map(|token| token.trim().to_string())
    }).endpoint(|bot: Bot, env: Arc<BotEnv>, msg: Message, token: String| async move {
        handle_guest_token_activation(bot, env, msg, token).await
    })
}

async fn handle_guest_token_activation(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    token: String,
) -> anyhow::Result<()> {
    let Some(guest) = msg.from.clone() else { return Ok(()) };
    let guest_id = user_id_to_i64(guest.id)
        .ok_or_else(|| anyhow::anyhow!("User ID out of range"))?;
    // DB access block
    use crate::schema::temp_open_tokens::dsl as t;
    let now = chrono::Utc::now().naive_utc();
    let (token_row, already_used_by_other_guest);
    {
        let row = t::temp_open_tokens
            .filter(t::token.eq(&token))
            .filter(t::expires_at.gt(now))
            .select((
                t::id,
                t::token,
                t::resident_tg_id,
                t::guest_tg_id,
                t::created_at,
                t::expires_at,
                t::used_at,
            ))
            .first::<crate::models::TempOpenToken>(&mut *env.conn())
            .optional()?;
        if let Some(row) = row {
            let guest_db_id = row.guest_tg_id;
            already_used_by_other_guest =
                guest_db_id.is_some() && guest_db_id != Some(guest_id);
            token_row = Some(row);
        } else {
            already_used_by_other_guest = false;
            token_row = None;
        }
    }
    // Now safe to await
    let Some(token_row) = token_row else {
        bot.reply_message(&msg, "Invalid or expired guest access link.")
            .await?;
        return Ok(());
    };
    if already_used_by_other_guest {
        bot.reply_message(
            &msg,
            "This guest link has already been used by another user.",
        )
        .await?;
        return Ok(());
    }
    // Mark token as used by this guest (do not hold conn across await)
    {
        let mut conn = env.conn();
        diesel::update(t::temp_open_tokens.filter(t::id.eq(token_row.id)))
            .set(t::guest_tg_id.eq(guest_id))
            .execute(&mut *conn)?;
    }
    // Create "Open the door" button
    let keyboard =
        InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
            "🚪 Open the door",
            "butler:confirm_open",
        )]]);

    let response_text = "✅ Guest access activated! You can now use /open to temporarily open the door or click the button below. This action will be logged.";

    bot.reply_message(&msg, response_text).reply_markup(keyboard).await?;
    log::info!(
        "Guest {} ({}) activated temp_open (inviter: {})",
        guest.full_name(),
        guest.id,
        token_row.resident_tg_id
    );
    Ok(())
}
