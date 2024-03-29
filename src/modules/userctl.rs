//! `/userctl` command to add or remove MAC addresses.

use std::sync::Arc;

use anyhow::Result;
use argh::FromArgs;
use diesel::prelude::*;
use macro_rules_attribute::derive;
use teloxide::macros::BotCommands;
use teloxide::prelude::*;

use crate::common::{filter_command, BotCommandsExt, BotEnv, UpdateHandler};
use crate::db::DbUserId;
use crate::utils::BotExt;

#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(
        description = "control personal configuration, see <code>/userctl --help</code>."
    )]
    Userctl(String),
}

/// Control personal configuration.
#[derive(argh::FromArgs, Debug)]
struct UserctlArgs {
    /// add mac address
    #[argh(option)]
    add_mac: Vec<macaddr::MacAddr6>,

    /// remove mac address
    #[argh(option)]
    remove_mac: Vec<macaddr::MacAddr6>,
}

pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(cmd_userctl)
}

async fn cmd_userctl(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    Commands::Userctl(args): Commands,
) -> Result<()> {
    let Some(from) = &msg.from else { return Ok(()) };
    let args = args.split_whitespace().collect::<Vec<_>>();
    let args = match UserctlArgs::from_args(&["/userctl"], &args) {
        Ok(args) => args,
        Err(ee) => {
            bot.reply_message(&msg, ee.output).await?;
            return Ok(());
        }
    };

    let tg_id = DbUserId::from(from.id);
    let updated_macs = env.transaction(|conn| {
        diesel::delete(crate::schema::user_macs::table)
            .filter(crate::schema::user_macs::tg_id.eq(tg_id))
            .filter(
                crate::schema::user_macs::mac
                    .eq_any(args.remove_mac.iter().map(|m| m.to_string())),
            )
            .execute(conn)?;
        diesel::insert_into(crate::schema::user_macs::table)
            .values(
                args.add_mac
                    .iter()
                    .map(|m| {
                        (
                            crate::schema::user_macs::tg_id.eq(tg_id),
                            crate::schema::user_macs::mac.eq(m.to_string()),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .execute(conn)?;

        let macs = crate::schema::user_macs::table
            .filter(crate::schema::user_macs::tg_id.eq(tg_id))
            .select(crate::schema::user_macs::mac)
            .load::<String>(conn)?;

        Ok(macs)
    })?;

    bot.reply_message(&msg, format!("Updated: {updated_macs:?}")).await?;

    Ok(())
}
