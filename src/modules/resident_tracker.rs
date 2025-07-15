//! Add or remove users from residency when they join or leave residential
//! chats.
//!
//! **Scope**: chats listed in [`telegram.chats.residential`] config option.
//!
//! [`telegram.chats.residential`]: crate::config::TelegramChats::residential

use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::Result;
use diesel::dsl::count_star;
use diesel::prelude::*;
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, Chat, ChatId, ChatKind, ChatPublic, InlineKeyboardButton,
    InlineKeyboardMarkup, UpdateKind, User,
};

use crate::common::BotEnv;
use crate::db::{DbChatId, DbUserId};
use crate::schema;
use crate::utils::ResultExt;

/// Returns an update handler that listens for `ChatMemberUpdated` events and
/// notifies bot admins when a resident leaves **all** residential chats.
#[allow(clippy::missing_panics_doc)]
pub fn chat_member_handler() -> crate::common::UpdateHandler {
    Update::filter_chat_member()
        // We do filtering inside the async endpoint, so no extra filter here.
        .endpoint(handle_chat_member)
}

/// Returns an update handler that processes callback presses coming from the
/// "Stop residentship" button.
pub fn callback_handler() -> crate::common::UpdateHandler {
    dptree::filter_map(filter_callbacks).endpoint(handle_callback)
}

/// Data extracted from callback query.
#[derive(Debug, Clone)]
struct StopResidencyQuery {
    user_id: UserId,
}

fn filter_callbacks(callback: CallbackQuery) -> Option<StopResidencyQuery> {
    let data = callback.data.as_ref()?;
    let data = data.strip_prefix("res_stop:")?;
    let user_id = data.parse::<u64>().ok()?;
    Some(StopResidencyQuery { user_id: UserId(user_id) })
}

async fn handle_chat_member(
    bot: Bot,
    env: Arc<BotEnv>,
    cm: ChatMemberUpdated,
) -> Result<()> {
    // Ignore bot users.
    if cm.new_chat_member.user.is_bot {
        return Ok(());
    }

    // Check this chat is residential.
    if !env.config.telegram.chats.residential.contains(&cm.chat.id) {
        return Ok(());
    }

    // Determine join/leave event.
    let is_joined = match (
        cm.old_chat_member.kind.is_present(),
        cm.new_chat_member.kind.is_present(),
    ) {
        (false, true) => true,
        (true, false) => false,
        _ => return Ok(()), // promotion/demotion or no change
    };

    // We only care about leave events.
    if is_joined {
        return Ok(());
    }

    let user_id_db = DbUserId::from(cm.new_chat_member.user.id);
    let residential_chat_ids = env.config.telegram.chats.residential.clone();

    // Determine if the user is still seen in any residential chat and if they
    // are currently a resident.
    let (is_resident, is_seen) = env.transaction(|conn| {
        let is_resident = {
            use schema::residents::dsl as r;
            r::residents
                .filter(r::tg_id.eq(user_id_db))
                .filter(r::end_date.is_null())
                .select(count_star())
                .get_result::<i64>(conn)?
                > 0
        };

        let is_seen = {
            use schema::tg_users_in_chats::dsl as t;
            t::tg_users_in_chats
                .filter(t::user_id.eq(user_id_db))
                .filter(t::chat_id.eq_any(
                    residential_chat_ids.iter().map(|&c| DbChatId::from(c)),
                ))
                .filter(t::seen.eq(true))
                .select(count_star())
                .get_result::<i64>(conn)?
                > 0
        };

        Ok::<_, diesel::result::Error>((is_resident, is_seen))
    })?;

    // We care only about residents that are not seen in any residential chat
    // anymore.
    if !is_resident || is_seen {
        return Ok(());
    }

    log::info!(
        "User {} left all residential chats, notifying admins",
        user_text(&cm.new_chat_member.user)
    );

    // Build notification text.
    let text = format!(
        "Пользователь {} вышел из всех резидентских чатов.",
        user_text(&cm.new_chat_member.user)
    );

    let keyboard =
        InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
            "Stop residency",
            format!("res_stop:{}", cm.new_chat_member.user.id),
        )]]);

    // Send to each admin.
    for admin in &env.config.telegram.admins {
        bot.send_message(ChatId::from(*admin), &text)
            .reply_markup(keyboard.clone())
            .await?;
    }

    Ok(())
}

async fn handle_callback(
    bot: Bot,
    env: Arc<BotEnv>,
    query: StopResidencyQuery,
    callback: CallbackQuery,
) -> Result<()> {
    // Check that the user pressing the button is an admin.
    if !env.config.telegram.admins.contains(&callback.from.id) {
        bot.answer_callback_query(&callback.id)
            .text("You are not allowed to perform this action.")
            .await?;
        return Ok(());
    }

    // Close residency in DB.
    let user_db_id = DbUserId::from(query.user_id);

    let _ = env.transaction(|conn| {
        use schema::residents::dsl as r;
        diesel::update(r::residents)
            .filter(r::tg_id.eq(user_db_id))
            .filter(r::end_date.is_null())
            .set(r::end_date.eq(diesel::dsl::now))
            .execute(conn)
    })?;

    // Disable LDAP access (remove from residents group) if LDAP is configured.
    if let Ok(ldap_config) = env.ldap_config() {
        // Acquire LDAP connection.
        let mut ldap_state = env.ldap_client().await;
        if let Ok(ldap_conn) = ldap_state.get() {
            // Attempt to fetch user.
            if let Ok(Some(ldap_user)) = crate::utils::ldap::get_user(
                ldap_conn,
                ldap_config,
                query.user_id,
            )
            .await
            {
                use crate::utils::ldap::{remove_user_from_group, Group};
                let group = Group {
                    dn: format!(
                        "cn={},{},{}",
                        ldap_config.attributes.resident_group,
                        ldap_config.groups_dn,
                        ldap_config.base_dn
                    ),
                    cn: ldap_config.attributes.resident_group.clone(),
                };
                if let Err(e) = remove_user_from_group(
                    ldap_conn,
                    ldap_config,
                    &ldap_user,
                    &group,
                )
                .await
                {
                    log::error!("Failed to remove LDAP user from group: {e}");
                }
            }
        }
    }

    // Notify admin via popup and edit markup.
    bot.answer_callback_query(&callback.id)
        .text("Residency stopped and LDAP access disabled.")
        .await?;

    if let Some(msg) = &callback.message {
        bot.edit_message_reply_markup(msg.chat.id, msg.id).await?;
    }

    Ok(())
}

struct Filtered<'a> {
    cm: &'a ChatMemberUpdated,
    is_joined: bool,
}

/// Wrapper around [scrape].
pub fn inspect_update(env: Arc<BotEnv>, upd: Update) {
    let residential_chats = env.config.telegram.chats.residential.as_slice();
    let Some(filtered) = filter(&upd, residential_chats) else { return };
    env.transaction(|conn| {
        handle_update_transaction(conn, residential_chats, filtered)
    })
    .log_error(module_path!(), "resident_tracker::handle_update");
}

/// Scrape an update for residential chat joins/leaves and update the
/// `residents` table accordingly.
pub fn scrape(
    conn: &mut SqliteConnection,
    upd: &Update,
    residential_chats: &[ChatId],
) -> Result<(), diesel::result::Error> {
    let Some(filtered) = filter(upd, residential_chats) else { return Ok(()) };
    handle_update_transaction(conn, residential_chats, filtered)
}

fn filter<'a>(
    upd: &'a Update,
    residential_chats: &[ChatId],
) -> Option<Filtered<'a>> {
    let UpdateKind::ChatMember(ref cm) = upd.kind else { return None };

    if cm.new_chat_member.user.is_bot {
        return None;
    }

    let is_joined = match (
        cm.old_chat_member.kind.is_present(),
        cm.new_chat_member.kind.is_present(),
    ) {
        (false, true) => true,
        (true, false) => false,
        // Ignore promotion/demotion.
        _ => return None,
    };

    if !residential_chats.contains(&cm.chat.id) {
        // Ignore non-residential chats.
        return None;
    }

    Some(Filtered { cm, is_joined })
}

fn handle_update_transaction(
    conn: &mut SqliteConnection,
    residential_chats: &[ChatId],
    f: Filtered<'_>,
) -> Result<(), diesel::result::Error> {
    let user_id = DbUserId::from(f.cm.new_chat_member.user.id);

    let residential_chats =
        residential_chats.iter().map(|&i| DbChatId::from(i));

    let is_resident = {
        use schema::residents::dsl as r;
        r::residents
            .filter(r::tg_id.eq(user_id))
            .filter(r::end_date.is_null())
            .select(count_star())
            .get_result::<i64>(conn)?
            > 0
    };

    let is_seen = {
        use schema::tg_users_in_chats::dsl as t;
        t::tg_users_in_chats
            .filter(t::user_id.eq(user_id))
            .filter(t::chat_id.eq_any(residential_chats))
            .filter(t::seen.eq(true))
            .select(count_star())
            .get_result::<i64>(conn)?
            > 0
    };

    match (is_resident, is_seen, f.is_joined) {
        (true, false, false) => {
            // Previously: mark residency ended immediately.
            // Now we defer the final decision to bot admins via a notification.
            log::info!(
                "User {} left residential chat {}, awaiting admin confirmation to stop residency",
                user_text(&f.cm.new_chat_member.user),
                chat_text(&f.cm.chat),
            );
            // No DB changes here. A separate async handler will notify admins
            // and apply the changes once the "Stop residentship" button is
            // pressed.
        }
        (false, true, true) => {
            // Add to residency
            use schema::residents::dsl as r;
            log::info!(
                "User {} joined residential chat {}, adding to residency",
                user_text(&f.cm.new_chat_member.user),
                chat_text(&f.cm.chat),
            );
            diesel::insert_into(r::residents)
                .values((
                    r::tg_id.eq(user_id),
                    r::begin_date.eq(diesel::dsl::now),
                ))
                .execute(conn)?;
        }
        // Do not make any unintuitive changes. E.g. if a non-resident left
        // a residential chat, do not add them to residency, even if they
        // are still seen in other residential chats.
        _ => (),
    }

    Ok(())
}

fn user_text(user: &User) -> String {
    let mut text = format!("id={} ", user.id);
    if let Some(username) = &user.username {
        write!(text, "@{username} ").unwrap();
    }
    write!(text, "{:?}", user.full_name()).unwrap();
    text
}

fn chat_text(chat: &Chat) -> String {
    let mut text = format!("id={}", chat.id);
    if let ChatKind::Public(ChatPublic { title: Some(t), .. }) = &chat.kind {
        write!(text, " {t:?}").unwrap();
    }
    text
}
