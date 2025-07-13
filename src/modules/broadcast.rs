use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use diesel::prelude::*;
use macro_rules_attribute::derive;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::common::{filter_command, BotCommandsExt, BotEnv, UpdateHandler};
use crate::db::DbUserId;
use crate::schema;
use crate::utils::BotExt;

/// Commands available in this module.
#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(
        description = "broadcast a message to all residents. Use as a reply to the message you want to send."
    )]
    #[custom(admin = true)]
    Broadcast,
}

/// Return an update handler that filters and routes broadcast commands.
pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(handle_command)
}

async fn handle_command(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    command: Commands,
) -> Result<()> {
    match command {
        Commands::Broadcast => cmd_broadcast(bot, env, msg).await?,
    }
    Ok(())
}

/// Handle `/broadcast` command.
async fn cmd_broadcast(bot: Bot, env: Arc<BotEnv>, msg: Message) -> Result<()> {
    // The command must be a reply to the message to broadcast.
    let Some(orig_msg) = msg.reply_to_message() else {
        bot.reply_message(&msg, "Please reply to the message you want to broadcast with this command.")
            .await?;
        return Ok(());
    };
    let src_chat_id = orig_msg.chat.id;
    let src_message_id = orig_msg.id;

    // Fetch all active residents from the database.
    let residents: Vec<DbUserId> = schema::residents::table
        .filter(schema::residents::end_date.is_null())
        .select(schema::residents::tg_id)
        .load(&mut *env.conn())?;

    if residents.is_empty() {
        bot.reply_message(&msg, "Resident list is empty, broadcast canceled.")
            .await?;
        return Ok(());
    }

    // Notify admin that broadcast has started.
    bot.reply_message(
        &msg,
        format!("Starting broadcast to {} residents…", residents.len()),
    )
    .await?;

    // Spawn a task so that we don't block the dispatcher.
    let bot_clone = bot.clone();
    let admin_chat_id = msg.chat.id;

    tokio::spawn(async move {
        let mut sent_ok = 0usize;
        let mut failed = 0usize;

        for tg_id in residents {
            let recipient = UserId::from(tg_id);
            let send_res = bot_clone
                .copy_message(recipient, src_chat_id, src_message_id)
                .await;
            match send_res {
                Ok(_) => sent_ok += 1,
                Err(e) => {
                    failed += 1;
                    log::warn!(
                        "Broadcast: failed to send to {recipient:?}: {e}"
                    );
                }
            }
            // Respect Telegram rate limits (~30 msgs/sec). 40 ms ~ 25 msgs/sec.
            tokio::time::sleep(Duration::from_millis(40)).await;
        }

        let summary = format!(
            "Broadcast finished.\nSuccessfully sent: {sent_ok}\nFailed: {failed}"
        );
        let _ = bot_clone.send_message(admin_chat_id, summary).await;
    });

    Ok(())
}
