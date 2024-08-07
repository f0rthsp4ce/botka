use std::str::FromStr;
use std::sync::Arc;

use anyhow::Context;
use chrono::Utc;
use cron::Schedule;
use log::debug;
use teloxide::payloads::SendMessageSetters;
use teloxide::requests::Requester;
use teloxide::Bot;
use tokio::time::sleep;

use crate::config::{Config, VortexOfDoom};
use crate::utils::ResultExt;

async fn vortex_of_doom_internal(
    bot: Bot,
    config: &VortexOfDoom,
) -> anyhow::Result<()> {
    let schedule = Schedule::from_str(&config.schedule)
        .context("failed to parse schedule")?;

    loop {
        let next_run = schedule
            .upcoming(Utc)
            .next()
            .ok_or_else(|| anyhow::anyhow!("failed to get next schedule"))?;
        debug!("Next execution time {}", next_run);

        let now = Utc::now();
        let diff = next_run - now;
        debug!("Waiting for next schedule {}", diff);

        sleep(diff.to_std()?).await;

        let mut text = "It's vortex of doom time! Please move the boxes, and throw away the last one and send a picture.".to_string();
        if let Some(additional_text) = &config.additional_text {
            text.push_str("\n\n");
            text.push_str(additional_text);
        }
        bot.send_message(config.chat.chat, &text)
            .message_thread_id(config.chat.thread)
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
    }
}

pub async fn vortex_of_doom(bot: Bot, config: Arc<Config>) {
    vortex_of_doom_internal(bot, &config.telegram.chats.vortex_of_doom)
        .await
        .log_error("Vortex of doom error");
}
