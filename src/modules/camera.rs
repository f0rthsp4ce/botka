//! Commands related to cameras.

use std::sync::Arc;

use anyhow::Result;
use macro_rules_attribute::derive;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::common::{filter_command, BotCommandsExt, BotEnv, UpdateHandler};
use crate::utils::{read_camera_image, BotExt};

#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(description = "show racovina camera image.")]
    #[custom(in_resident_chat = true)]
    Racovina,
    #[command(description = "show hlam camera image.")]
    #[custom(in_resident_chat = true)]
    Hlam,
}

pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(camera)
}

async fn camera(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    command: Commands,
) -> Result<()> {
    let camera_config = match command {
        Commands::Racovina => &env.config.services.racovina_cam,
        Commands::Hlam => &env.config.services.vortex_of_doom_cam,
    };

    let image = match read_camera_image(
        env.reqwest_client.clone(),
        camera_config,
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
