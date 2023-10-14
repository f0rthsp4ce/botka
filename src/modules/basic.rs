use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use diesel::prelude::*;
use macro_rules_attribute::derive;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::common::{
    filter_command, format_users, format_users2, BotEnv, CommandHandler,
    MyDialogue, State,
};
use crate::db::DbUserId;
use crate::utils::BotExt;
use crate::{models, schema, HasCommandRules};

#[derive(BotCommands, Clone, HasCommandRules!)]
#[command(
    rename_rule = "snake_case",
    description = "These commands are supported:"
)]
enum Command {
    #[command(description = "display this text.")]
    Help,

    #[command(description = "list residents.")]
    Residents,

    #[command(description = "show status.")]
    Status,

    #[command(description = "show bot version.")]
    Version,
}

pub fn command_handler() -> CommandHandler<Result<()>> {
    filter_command::<Command, _>().endpoint(start)
}

async fn start<'a>(
    bot: Bot,
    dialogue: MyDialogue,
    env: Arc<BotEnv>,
    msg: Message,
    command: Command,
) -> Result<()> {
    dialogue.update(State::Start).await?;
    match command {
        Command::Help => {
            bot.reply_message(&msg, Command::descriptions().to_string())
                .await?;
        }
        Command::Residents => cmd_list_residents(bot, env, msg).await?,
        Command::Status => cmd_status(bot, env, msg).await?,
        Command::Version => {
            bot.reply_message(&msg, crate::VERSION).await?;
        }
    }
    Ok(())
}

async fn cmd_list_residents<'a>(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let residents = schema::residents::table
        .filter(schema::residents::is_resident.eq(true))
        .left_join(
            schema::tg_users::table
                .on(schema::residents::tg_id.eq(schema::tg_users::id)),
        )
        .order(schema::residents::tg_id.asc())
        .load::<(models::Resident, Option<models::TgUser>)>(&mut *env.conn())
        .unwrap();
    let mut text = String::new();

    text.push_str("Residents: ");
    text.push_str(&format_users(residents.iter().map(|(r, u)| (r, u))));
    text.push('.');
    bot.reply_message(&msg, text).await?;
    Ok(())
}

async fn cmd_status(bot: Bot, env: Arc<BotEnv>, msg: Message) -> Result<()> {
    #[derive(serde::Deserialize, Debug)]
    #[serde(rename_all = "kebab-case")]
    struct Lease {
        mac_address: String,
        #[serde(deserialize_with = "crate::utils::deserealize_duration")]
        last_seen: Duration,
    }

    let conf = &env.config.services.mikrotik;
    let leases = env
        .reqwest_client
        .post(format!("https://{}/rest/ip/dhcp-server/lease/print", conf.host))
        .timeout(Duration::from_secs(5))
        .basic_auth(&conf.username, Some(&conf.password))
        .json(&serde_json::json!({
            ".proplist": [
                "mac-address",
                "last-seen",
            ]
        }))
        .send()
        .await?
        .json::<Vec<Lease>>()
        .await;

    let mut text = String::new();
    match leases {
        Ok(leases) => {
            let active_mac_addrs = leases
                .into_iter()
                .filter(|l| l.last_seen < Duration::from_secs(11 * 60))
                .map(|l| l.mac_address)
                .collect::<Vec<_>>();
            let data: Vec<(DbUserId, Option<models::TgUser>)> =
                schema::user_macs::table
                    .left_join(
                        schema::tg_users::table
                            .on(schema::user_macs::tg_id
                                .eq(schema::tg_users::id)),
                    )
                    .filter(schema::user_macs::mac.eq_any(&active_mac_addrs))
                    .select((
                        schema::user_macs::tg_id,
                        schema::tg_users::all_columns.nullable(),
                    ))
                    .distinct()
                    .load(&mut *env.conn())?;
            writeln!(&mut text, "Currently in space: ").unwrap();
            format_users2(&mut text, data.iter().map(|(id, u)| (*id, u)));
        }
        Err(e) => {
            log::error!("Failed to get leases: {}", e);
            writeln!(text, "Failed to get leases.").unwrap();
        }
    }
    bot.reply_message(&msg, text)
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true)
        .await?;

    Ok(())
}
