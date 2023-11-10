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

/// Add or remove users from residency when they join or leave residential
/// chats.
pub fn handle_update(env: Arc<BotEnv>, upd: Update) {
    let UpdateKind::ChatMember(cm) = upd.kind else { return };

    let is_joined = match (
        cm.old_chat_member.kind.is_present(),
        cm.new_chat_member.kind.is_present(),
    ) {
        (false, true) => true,
        (true, false) => false,
        // Ignore promotion/demotion.
        _ => return,
    };

    if !env.config.telegram.chats.residential.contains(&cm.chat.id) {
        // Ignore non-residential chats.
        return;
    }

    let user_id = DbUserId::from(cm.new_chat_member.user.id);

    env.transaction(|conn| {
        let residential_chats = env
            .config
            .telegram
            .chats
            .residential
            .iter()
            .map(|&i| DbChatId::from(i));

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

        match (is_resident, is_seen, is_joined) {
            (true, false, false) => {
                use schema::residents::dsl as r;
                log::info!(
                    "User {} left residential chat {}, removing from residency",
                    user_text(&cm.new_chat_member.user),
                    chat_text(&cm.chat),
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
                    user_text(&cm.new_chat_member.user),
                    chat_text(&cm.chat),
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
    })
    .log_error("resident_tracker::handle_update");
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
