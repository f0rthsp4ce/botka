//! Commands related to cameras.

use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use macro_rules_attribute::derive;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::common::{filter_command, BotCommandsExt, BotEnv, UpdateHandler};
use crate::config::{Config, EspCam};
use crate::utils::{read_camera_image, BotExt};

#[derive(Clone)]
enum CameraId {
    VortexOfDoom,
    Racovina,
}

impl CameraId {
    const fn get_config<'a>(&self, config: &'a Config) -> &'a EspCam {
        match self {
            Self::VortexOfDoom => &config.services.vortex_of_doom_cam,
            Self::Racovina => &config.services.racovina_cam,
        }
    }
}

impl FromStr for CameraId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "vortex_of_doom" => Ok(Self::VortexOfDoom),
            "racovina" => Ok(Self::Racovina),
            _ => Err(anyhow::anyhow!("unknown camera id")),
        }
    }
}

#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
enum Commands {
    #[command(description = "show camera image.")]
    #[custom(in_resident_chat = true)]
    Camera(CameraId),
}

pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(start)
}

async fn start<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    command: Commands,
) -> Result<()> {
    match command {
        Commands::Camera(camera_id) => camera(bot, env, msg, camera_id).await?,
    }
    Ok(())
}

async fn camera(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    camera_id: CameraId,
) -> Result<()> {
    let image = match read_camera_image(
        env.reqwest_client.clone(),
        camera_id.get_config(&env.config),
    )
    .await
    {
        Ok(image) => image,
        Err(e) => {
            bot.send_message(
                msg.chat.id,
                format!("Failed to fetch camera image: {e}"),
            )
            .await?;
            return Ok(());
        }
    };

    bot.reply_photo(&msg, image).await?;

    Ok(())
}
