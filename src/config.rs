//! Bot configuration.
//!
//! For documentation on each field, see comments in the `config.example.yaml`
//! file in the repository root. Here its contents:
//!
//! ```yaml
#![doc = include_str!("../config.example.yaml")]
//! ```

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use teloxide::types::{ChatId, ThreadId, UserId};

use crate::utils::ThreadIdPair;

/// The root configuration structure for the bot.
#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    pub telegram: Telegram,
    pub server_addr: SocketAddr,
    pub services: Services,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Telegram {
    pub token: String,
    pub admins: Vec<UserId>,
    pub passive_mode: bool,
    pub chats: TelegramChats,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TelegramChats {
    pub residential: Vec<ChatId>,
    pub borrowed_items: Vec<ThreadIdPair>,
    pub forward_channel: ChatId,
    pub forward_pins: Vec<FowardPins>,
    pub needs: ThreadIdPair,
    pub resident_owned: Vec<ResidentOwned>,
    pub wikijs_updates: ThreadIdPair,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ResidentOwned {
    pub id: ChatId,
    pub internal: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FowardPins {
    pub from: ChatId,
    pub to: ChatId,
    pub ignore_threads: Vec<ThreadId>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Services {
    pub mikrotik: Microtik,
    pub home_assistant: HomeAssistant,
    pub wikijs: WikiJs,
    pub openai: OpenAI,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Microtik {
    pub host: String,
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct HomeAssistant {
    pub host: String,
    pub token: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct WikiJs {
    pub url: String,
    pub token: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct OpenAI {
    pub api_key: String,
    #[serde(default)]
    pub disable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_example_config() -> anyhow::Result<()> {
        let config_text = std::fs::read_to_string("config.example.yaml")?;
        let config: Config = serde_yaml::from_str(&config_text)?;

        similar_asserts::assert_serde_eq!(
            serde_yaml::to_value(config)?,
            serde_yaml::from_str::<serde_yaml::Value>(&config_text)?,
            "Extra fields in config.example.yaml?",
        );

        Ok(())
    }
}
