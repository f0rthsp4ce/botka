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
use crate::utils::get_wikijs_updates;

pub fn register_metrics() {
    metrics::register_gauge!("wikijs_update_success");
    metrics::describe_gauge!(
        "wikijs_update_success",
        "1 if the last update was successful, 0 otherwise, 0.5 if not yet run"
    );
    metrics::gauge!("wikijs_update_success", 0.5);

    metrics::register_gauge!("wikijs_update_last");
    metrics::describe_gauge!(
        "wikijs_update_last",
        "Timestamp of the last successful update"
    );
}

pub async fn task(env: Arc<BotEnv>, bot: Bot, shutdown: CancellationToken) {
    loop {
        select! {
            () = shutdown.cancelled() => {
                break;
            }
            () = sleep(Duration::from_secs(60)) => {}
        }

        match check_wikijs_updates(env.clone(), bot.clone()).await {
            Ok(()) => {
                metrics::gauge!("wikijs_update_success", 1.0);
                metrics::gauge!("wikijs_update_last", crate::now_f64());
            }
            Err(e) => {
                metrics::gauge!("wikijs_update_success", 0.0);
                log::error!("check_wikijs_updates: {}", e);
            }
        }
    }
}

async fn check_wikijs_updates(env: Arc<BotEnv>, bot: Bot) -> Result<()> {
    let old_update_state = models::wikijs_update_state.get(&mut env.conn())?;
    let (text, new_update_state) = get_wikijs_updates(
        &env.config.services.wikijs.url,
        &env.config.services.wikijs.token,
        old_update_state.clone(),
    )
    .await?;

    if let Some(text) = text {
        bot.send_message(env.config.telegram.chats.wikijs_updates.chat, text)
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
