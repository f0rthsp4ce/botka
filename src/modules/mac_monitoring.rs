use std::{sync::Arc, time::Duration};

use anyhow::Result;
use diesel::{
    ExpressionMethods, JoinOnDsl, NullableExpressionMethods, QueryDsl,
    RunQueryDsl,
};
use teloxide::{payloads::SendMessageSetters, requests::Requester, Bot};
use tokio::time::sleep;

use crate::{
    common::{format_users, BotEnv},
    db::DbUserId,
    models, schema,
    utils::mikrotik::get_leases,
};

pub async fn mac_monitoring(env: Arc<BotEnv>, bot: Bot) -> Result<()> {
    let leases =
        get_leases(&env.reqwest_client, &env.config.services.mikrotik).await?;

    let active_mac_addrs = leases
        .into_iter()
        .filter(|l| l.last_seen < Duration::from_secs(11 * 60))
        .map(|l| l.mac_address)
        .collect::<Vec<_>>();
    let data: Vec<(DbUserId, Option<models::TgUser>)> =
        schema::user_macs::table
            .left_join(
                schema::tg_users::table
                    .on(schema::user_macs::tg_id.eq(schema::tg_users::id)),
            )
            .filter(schema::user_macs::mac.eq_any(&active_mac_addrs))
            .select((
                schema::user_macs::tg_id,
                schema::tg_users::all_columns.nullable(),
            ))
            .distinct()
            .load(&mut *env.conn())?;

    let prev_data;
    {
        let mut active_macs_guard = env.active_macs.write().await;
        prev_data = active_macs_guard.take();
        *active_macs_guard = Some(data.clone());
    }

    let mut deleted_users: Vec<(DbUserId, Option<models::TgUser>)> = Vec::new();
    let mut added_users: Vec<(DbUserId, Option<models::TgUser>)> = Vec::new();
    if let Some(prev_data) = prev_data {
        // Find diff
        for &(tg_id, ref user) in &data {
            if !prev_data
                .iter()
                .any(|&(prev_tg_id, ref _prev_user)| prev_tg_id == tg_id)
            {
                added_users.push((tg_id, user.clone())); // Add to added_users if not found in prev_data
            }
        }

        for &(prev_tg_id, ref prev_user) in &prev_data {
            if !data.iter().any(|&(tg_id, ref _user)| tg_id == prev_tg_id) {
                deleted_users.push((prev_tg_id, prev_user.clone())); // Add to deleted_users if not found in data
            }
        }
    }

    let mut text = String::new();
    if !deleted_users.is_empty() {
        text.push_str("Left:\n");
        format_users(&mut text, deleted_users.iter().map(|(id, u)| (*id, u)));
    }
    if !added_users.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("Joined:\n");
        format_users(&mut text, added_users.iter().map(|(id, u)| (*id, u)));
    }
    bot.send_message(env.config.telegram.chats.mac_monitoring.chat, text)
        .message_thread_id(env.config.telegram.chats.mac_monitoring.thread)
        .await?;

    Ok(())
}

pub async fn watch_loop(env: Arc<BotEnv>, bot: Bot) {
    loop {
        if let Err(e) = mac_monitoring(Arc::<BotEnv>::clone(&env), bot.clone()).await {
            log::error!("Failed to get leases: {e}");
        };
        sleep(Duration::from_secs(60)).await;
    }
}
