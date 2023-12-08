//! Add or remove users from residency when they join or leave residential
//! chats.
//!
//! **Scope**: chats listed in [`telegram.chats.residential`] config option.
//!
//! [`telegram.chats.residential`]: crate::config::TelegramChats::residential

use std::fmt::Write as _;
use std::sync::Arc;

use diesel::dsl::count_star;
use diesel::prelude::*;
use teloxide::prelude::*;
use teloxide::types::{Chat, ChatKind, ChatPublic, UpdateKind, User};

use crate::common::BotEnv;
use crate::db::{DbChatId, DbUserId};
use crate::schema;
use crate::utils::ResultExt;

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
    .log_error("resident_tracker::handle_update");
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
            use schema::residents::dsl as r;
            log::info!(
                "User {} left residential chat {}, removing from residency",
                user_text(&f.cm.new_chat_member.user),
                chat_text(&f.cm.chat),
            );
            diesel::update(r::residents)
                .filter(r::tg_id.eq(user_id))
                .filter(r::end_date.is_null())
                .set(r::end_date.eq(diesel::dsl::now))
                .execute(conn)?;
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
