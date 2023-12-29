use std::sync::Arc;

use anyhow::{Context as _, Result};
use itertools::Itertools;
use macro_rules_attribute::derive;
use teloxide::macros::BotCommands;
use teloxide::prelude::*;

use crate::common::{filter_command, BotCommandsExt, BotEnv, UpdateHandler};
use crate::utils::{get_wikijs_page, parse_tg_thread_link, BotExt as _};

mod raw;

#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[custom(admin = true)]
    #[command()]
    DebugUpdateDashboard(String),
}

pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(handle_command)
}

async fn handle_command(
    bot: Bot,
    env: Arc<BotEnv>,
    command: Commands,
    msg: Message,
) -> Result<()> {
    let Commands::DebugUpdateDashboard(text) = command;
    let mut it = text.split('\n');
    let Some(topic) = it.next().and_then(parse_tg_thread_link) else {
        bot.reply_message(&msg, "Invalid thread link").await?;
        return Ok(());
    };
    let new_messages = it.collect_vec();
    raw::update(&bot, Arc::clone(&env), topic, &new_messages).await?;
    Ok(())
}

pub async fn update(bot: &Bot, env: &Arc<BotEnv>) -> Result<()> {
    let page = get_wikijs_page(
        &env.config.services.wikijs.url,
        &env.config.services.wikijs.token,
        &env.config.services.wikijs.dashboard_page,
    )
    .await?;

    let page = crate::modules::welcome::extract_message(&page)
        .context("Failed to extract message from Wiki.js page")?;

    raw::update(
        bot,
        Arc::clone(env),
        env.config.telegram.chats.dashboard,
        &[page],
    )
    .await?;

    Ok(())
}
