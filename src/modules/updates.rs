//! Watch for updates in Wiki.js and send a notification to the specified
//! thread.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use tokio::select;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::common::BotEnv;
use crate::models;
use crate::utils::{get_wikijs_updates, ResultExt as _, StatusChangeDetector};

pub async fn task(env: Arc<BotEnv>, bot: Bot, shutdown: CancellationToken) {
    let mut initial = true;
    let mut ed = StatusChangeDetector::new();
    loop {
        select! {
            () = shutdown.cancelled() => {
                break;
            }
            () = sleep(Duration::from_secs(60)) => {}
        }

        let res = check_wikijs_updates(&env, &bot, initial).await;
        crate::metrics::update_service("wikijs", res.is_ok());
        ed.log_on_change("Wiki.js", res);

        initial = false;
    }
}

async fn check_wikijs_updates(
    env: &Arc<BotEnv>,
    bot: &Bot,
    initial: bool,
) -> Result<()> {
    let old_update_state = models::wikijs_update_state.get(&mut env.conn())?;
    let (updates, new_update_state) = get_wikijs_updates(
        &env.config.services.wikijs.url,
        &env.config.services.wikijs.token,
        old_update_state.clone(),
    )
    .await?;

    if initial
        || updates
            .iter()
            .flat_map(|x| x.paths())
            .any(|p| p == env.config.services.wikijs.dashboard_page)
    {
        crate::modules::dashboard::update(bot, env)
            .await
            .log_error(module_path!(), "Failed to update dashboard");
    }

    if let Some(updates) = updates {
        bot.send_message(
            env.config.telegram.chats.wikijs_updates.chat,
            updates.to_html(),
        )
        .message_thread_id(env.config.telegram.chats.wikijs_updates.thread)
        .parse_mode(ParseMode::Html)
        .disable_web_page_preview(true)
        .await?;
    }

    // XXX: Not sure if this check makes sense.  I want to avoid spurious
    // disk writes.
    if old_update_state.as_ref() != Some(&new_update_state) {
        models::wikijs_update_state.set(&mut env.conn(), &new_update_state)?;
    }

    Ok(())
}
