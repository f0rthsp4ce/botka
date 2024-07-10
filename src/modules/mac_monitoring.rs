use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl, SqliteConnection};
use teloxide::payloads::SendMessageSetters;
use teloxide::requests::Requester;
use teloxide::types::UserId;
use teloxide::Bot;
use tokio::sync::RwLock;
use tokio::time::sleep;

use crate::common::{format_users, BotEnv};
use crate::config::Mikrotik;
use crate::db::DbUserId;
use crate::metrics::update_user_online;
use crate::utils::mikrotik::get_leases;
use crate::utils::ThreadIdPair;
use crate::{models, schema};

/// State contains a set of active MAC addresses.
#[derive(Clone, Debug, Default)]
pub struct State(Option<HashSet<UserId>>);

impl State {
    pub const fn active_users(&self) -> Option<&HashSet<UserId>> {
        self.0.as_ref()
    }
}

pub fn state() -> Arc<RwLock<State>> {
    Arc::new(RwLock::new(State::default()))
}

async fn mac_monitoring(
    reqwest_client: &reqwest::Client,
    mikrotik_conf: &Mikrotik,
    mac_monitoring_thread: &ThreadIdPair,
    conn: &Mutex<SqliteConnection>,
    state: Arc<RwLock<State>>,
    bot: Arc<Bot>,
) -> Result<()> {
    let leases = get_leases(reqwest_client, mikrotik_conf).await?;

    let active_mac_addrs = leases
        .into_iter()
        .filter(|l| l.last_seen < Duration::from_secs(11 * 60))
        .map(|l| l.mac_address)
        .collect::<Vec<_>>();

    let data: HashSet<UserId> = {
        let db_result: Vec<DbUserId> = schema::user_macs::table
            .filter(schema::user_macs::mac.eq_any(&active_mac_addrs))
            .select(schema::user_macs::tg_id)
            .load(&mut *conn.lock().unwrap())?;
        db_result.into_iter().map(UserId::from).collect()
    };

    let prev_data = state.write().await.0.replace(data.clone());

    let mut deleted_users: HashSet<UserId> = HashSet::new();
    let mut added_users: HashSet<UserId> = HashSet::new();
    if let Some(prev_data) = prev_data {
        added_users = data.difference(&prev_data).copied().collect();
        deleted_users = prev_data.difference(&data).copied().collect();
    }

    for tg_id in &added_users {
        update_user_online(*tg_id, true);
    }
    for tg_id in &deleted_users {
        update_user_online(*tg_id, false);
    }

    let changed_users: HashSet<&UserId> =
        added_users.union(&deleted_users).collect();
    if changed_users.is_empty() {
        return Ok(());
    }

    let tg_data: Vec<models::TgUser> = schema::tg_users::table
        .filter(
            schema::tg_users::id
                .eq_any(changed_users.iter().map(|id| DbUserId::from(**id))),
        )
        .select(schema::tg_users::all_columns)
        .load(&mut *conn.lock().unwrap())?;
    let id_to_user_map: HashMap<&UserId, Option<&models::TgUser>> =
        changed_users
            .into_iter()
            .map(|id| {
                (id, tg_data.iter().find(|u| u.id == DbUserId::from(*id)))
            })
            .collect();

    let mut text = String::new();
    if !deleted_users.is_empty() {
        text.push_str("Left space:\n");
        format_users(
            &mut text,
            deleted_users
                .iter()
                .map(|id| (*id, *id_to_user_map.get(id).unwrap())),
        );
    }
    if !added_users.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("Joined space:\n");
        format_users(
            &mut text,
            added_users
                .iter()
                .map(|id| (*id, *id_to_user_map.get(id).unwrap())),
        );
    }
    if !text.is_empty() {
        bot.send_message(mac_monitoring_thread.chat, text)
            .message_thread_id(mac_monitoring_thread.thread)
            .parse_mode(teloxide::types::ParseMode::Html)
            .disable_web_page_preview(true)
            .await?;
    }

    Ok(())
}

pub async fn watch_loop(
    env: Arc<BotEnv>,
    state: Arc<RwLock<State>>,
    bot: Arc<Bot>,
) {
    loop {
        log::debug!("Executing mac_monitoring");
        if let Err(e) = mac_monitoring(
            &env.reqwest_client,
            &env.config.services.mikrotik,
            &env.config.telegram.chats.mac_monitoring,
            &env.conn,
            Arc::clone(&state),
            Arc::clone(&bot),
        )
        .await
        {
            log::error!("Failed to get leases: {e}");
        };
        sleep(Duration::from_secs(60)).await;
    }
}
