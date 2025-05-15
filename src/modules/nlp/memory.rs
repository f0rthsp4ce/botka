//! Memory management for NLP

use std::sync::Arc;

use anyhow::Result;
use chrono::{Duration, Utc};
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use teloxide::prelude::*;
use teloxide::types::{Message, ThreadId};

use crate::common::{is_resident, BotEnv};
use crate::db::{DbChatId, DbThreadId, DbUserId};
use crate::models::{ChatHistoryEntry, Memory, NewChatHistoryEntry, NewMemory};
use crate::modules::nlp::types::{NlpDebug, SaveMemoryArgs};
use crate::utils::GENERAL_THREAD_ID;

/// Retrieve chat history
pub fn get_chat_history(
    env: &Arc<BotEnv>,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
) -> Result<Vec<ChatHistoryEntry>> {
    let thread_id = thread_id.unwrap_or(GENERAL_THREAD_ID);
    let max_history = env.config.nlp.max_history;

    // Calculate timestamp from 24 hours ago
    let day_ago = (Utc::now() - Duration::hours(24)).naive_utc();

    let history = env.transaction(|conn| {
        crate::schema::chat_history::table
            .filter(
                crate::schema::chat_history::chat_id
                    .eq(DbChatId::from(chat_id)),
            )
            .filter(
                crate::schema::chat_history::thread_id
                    .eq(DbThreadId::from(thread_id)),
            )
            .filter(crate::schema::chat_history::timestamp.ge(day_ago))
            .order(crate::schema::chat_history::timestamp.desc())
            .limit(i64::from(max_history))
            .load::<ChatHistoryEntry>(conn)
    })?;

    Ok(history)
}

/// Get relevant memories (active and recently expired)
pub fn get_relevant_memories(
    env: &Arc<BotEnv>,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    user_id: UserId,
) -> Result<Vec<Memory>> {
    let thread_id = thread_id.unwrap_or(GENERAL_THREAD_ID);
    let yesterday = (Utc::now() - Duration::days(1)).naive_utc();

    // Fetch all memories that are either active, have null expiration, or expired within the last day
    let all_memories = env.transaction(|conn| {
        use diesel::prelude::*;

        use crate::schema::memories;

        // Get memories with null expiration OR expiration > yesterday
        memories::table
            .filter(
                memories::expiration_date
                    .is_null()
                    .or(memories::expiration_date.gt(yesterday)),
            )
            .load::<Memory>(conn)
    })?;

    // Now filter the results in Rust code
    let filtered_memories = all_memories
        .into_iter()
        .filter(|memory| {
            (memory.chat_id.is_none() || memory.chat_id == Some(chat_id.into()))
                && (memory.thread_id.is_none()
                    || memory.thread_id == Some(thread_id.into()))
                && (memory.user_id.is_none()
                    || memory.user_id == Some(user_id.into()))
        })
        .collect();

    Ok(filtered_memories)
}

/// Store a new message in chat history
pub async fn store_message(env: Arc<BotEnv>, msg: Message) -> Result<()> {
    let Some(text) = msg.text().or_else(|| msg.caption()) else {
        return Ok(());
    };

    // Skip if message is a command
    if text.starts_with('/') {
        return Ok(());
    }

    // Skip if message is prefixed with "--"
    if text.starts_with("--") {
        return Ok(());
    }

    let thread_id = msg.thread_id.unwrap_or(GENERAL_THREAD_ID);

    let new_entry = NewChatHistoryEntry {
        chat_id: msg.chat.id.into(),
        thread_id: thread_id.into(),
        message_id: msg.id.into(),
        from_user_id: msg.from.as_ref().map(|u| u.id.into()),
        timestamp: Utc::now().naive_utc(),
        message_text: msg.text().unwrap_or(""),
        classification_result: None,
        used_model: None,
    };

    env.transaction(|conn| {
        // Insert new message
        diesel::insert_into(crate::schema::chat_history::table)
            .values(&new_entry)
            .execute(conn)?;

        Ok(())
    })?;

    Ok(())
}

/// Store bot's response in chat history
pub fn store_bot_response(
    env: &Arc<BotEnv>,
    original_msg: &Message,
    sent_msg: &Message,
    content: &str,
    nlp_debug: &NlpDebug,
) -> Result<()> {
    let thread_id = original_msg.thread_id.unwrap_or(GENERAL_THREAD_ID);

    let classification_str = nlp_debug.classification_result.as_str();
    let new_entry = NewChatHistoryEntry {
        chat_id: original_msg.chat.id.into(),
        thread_id: thread_id.into(),
        message_id: sent_msg.id.into(),
        from_user_id: None, // From bot
        timestamp: Utc::now().naive_utc(),
        message_text: content,
        classification_result: Some(&classification_str),
        used_model: nlp_debug.used_model.as_deref(),
    };

    env.transaction(|conn| {
        diesel::insert_into(crate::schema::chat_history::table)
            .values(&new_entry)
            .execute(conn)
    })?;

    Ok(())
}

/// Handle `save_memory` function call
pub fn handle_save_memory(
    env: &Arc<BotEnv>,
    msg: &Message,
    arguments: &str,
) -> Result<String> {
    let args: SaveMemoryArgs = serde_json::from_str(arguments)?;

    // Non-resident users can only save short-term user-specific memories
    if !is_resident(
        &mut env.conn(),
        &msg.from.clone().expect("empty from user"),
    ) {
        if !args.user_specific {
            return Err(anyhow::anyhow!(
                "Non-resident users can only save user-specific memories."
            ));
        }
        if args.duration_hours.is_none() {
            return Err(anyhow::anyhow!(
                "Non-resident users can only save short-term memories (up to {}).", 
                env.config.nlp.memory_limit
            ));
        }
    }

    let now = Utc::now().naive_utc();
    let expiration = args.duration_hours.map(|hours| {
        let memory_limit = env.config.nlp.memory_limit;
        now + Duration::hours(i64::from(hours).min(memory_limit))
    });

    let chat_id = args.chat_specific.then(|| DbChatId::from(msg.chat.id));

    let thread_id = (args.thread_specific && args.chat_specific)
        .then(|| DbThreadId::from(msg.thread_id.unwrap_or(GENERAL_THREAD_ID)));

    let user_id = args
        .user_specific
        .then(|| DbUserId::from(msg.from.clone().expect("empty from user").id));

    let new_memory = NewMemory {
        memory_text: &args.memory_text,
        creation_date: now,
        expiration_date: expiration,
        chat_id,
        thread_id,
        user_id,
    };

    env.transaction(|conn| {
        diesel::insert_into(crate::schema::memories::table)
            .values(&new_memory)
            .execute(conn)
    })?;

    log::info!(
        "Saved memory: {} ({:?})",
        args.memory_text,
        args.duration_hours
    );

    Ok("Memory saved successfully.".to_string())
}

/// Handle `remove_memory` function call
pub fn handle_remove_memory(
    env: &Arc<BotEnv>,
    msg: &Message,
    memory_id: i32,
) -> Result<String> {
    // Non-residents cannot remove memories
    if !is_resident(
        &mut env.conn(),
        &msg.from.clone().expect("empty from user"),
    ) {
        return Err(anyhow::anyhow!(
            "Non-resident users cannot remove memories."
        ));
    }

    // Check if memory exists first
    let exists = env.transaction(|conn| {
        let count: i64 = crate::schema::memories::table
            .filter(crate::schema::memories::rowid.eq(memory_id))
            .count()
            .get_result(conn)?;

        Ok(count > 0)
    })?;

    if !exists {
        return Err(anyhow::anyhow!("Memory with ID {} not found", memory_id));
    }

    // Delete the memory
    env.transaction(|conn| {
        diesel::delete(crate::schema::memories::table)
            .filter(crate::schema::memories::rowid.eq(memory_id))
            .execute(conn)
    })?;

    log::info!("Removed memory with ID: {memory_id}");

    Ok("Memory removed successfully.".to_string())
}
