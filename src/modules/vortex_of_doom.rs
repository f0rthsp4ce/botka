use std::str::FromStr;
use std::sync::Arc;

use anyhow::Context;
use chrono::Utc;
use cron::Schedule;
use log::debug;
use teloxide::payloads::{SendMessageSetters, SendPhotoSetters};
use teloxide::requests::Requester;
use teloxide::Bot;
use tokio::time::sleep;

use crate::config::{Config, EspCam, VortexOfDoom};
use crate::utils::{read_camera_image, ResultExt};

async fn vortex_of_doom_internal(
    bot: Bot,
    client: reqwest::Client,
    chat_config: &VortexOfDoom,
    camera_config: &EspCam,
) -> anyhow::Result<()> {
    let schedule = Schedule::from_str(&chat_config.schedule)
        .context("failed to parse schedule")?;

    loop {
        let next_run = schedule
            .upcoming(Utc)
            .next()
            .ok_or_else(|| anyhow::anyhow!("failed to get next schedule"))?;
        debug!("Next execution time {next_run}");

        let now = Utc::now();
        let diff = next_run - now;
        debug!("Waiting for next schedule {diff}");

        sleep(diff.to_std()?).await;

        let image = read_camera_image(client.clone(), camera_config)
            .await
            .log_ok(module_path!(), "failed to fetch espcam image");

        let mut text = "It's vortex of doom time! Please move the boxes, and throw away the last one and send a picture.".to_string();
        if let Some(additional_text) = &chat_config.additional_text {
            text.push_str("\n\n");
            text.push_str(additional_text);
        }
        if let Some(image) = image {
            bot.send_photo(chat_config.chat.chat, image)
                .caption(&text)
                .message_thread_id(chat_config.chat.thread)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        } else {
            text.push_str(
                "\n\nFailed to fetch camera image, please check the logs.",
            );
            bot.send_message(chat_config.chat.chat, &text)
                .message_thread_id(chat_config.chat.thread)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        }
    }
}

pub async fn vortex_of_doom(
    bot: Bot,
    client: reqwest::Client,
    config: Arc<Config>,
) {
    vortex_of_doom_internal(
        bot,
        client,
        &config.telegram.chats.vortex_of_doom,
        &config.services.vortex_of_doom_cam,
    )
    .await
    .log_error(module_path!(), "Vortex of doom error");
}
