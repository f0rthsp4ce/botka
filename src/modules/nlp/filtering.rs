//! Filtering messages for NLP processing

use std::sync::Arc;

use teloxide::prelude::*;
use teloxide::types::{Message, MessageEntityKind};
use tokio::sync::RwLock;

use crate::common::{BotEnv, UpdateHandler};
use crate::modules::mac_monitoring;

use super::memory::get_chat_history;
use super::classification::classify_random_request;

/// Main message handler for natural language processing
pub fn message_handler() -> UpdateHandler {
    dptree::filter_map(filter_nlp_messages).endpoint(|bot: Bot, env: Arc<BotEnv>, mac_state: Arc<RwLock<mac_monitoring::State>>, msg: Message| async move {
        super::processing::handle_nlp_message(bot, env, mac_state, msg).await
    })
}

/// Random message handler for casual interventions
pub fn random_message_handler() -> UpdateHandler {
    dptree::filter_map_async(randomly_filter_nlp_messages)
        .endpoint(|bot: Bot, env: Arc<BotEnv>, mac_state: Arc<RwLock<mac_monitoring::State>>, msg: Message| async move {
            super::processing::handle_nlp_message(bot, env, mac_state, msg).await
        })
}

/// Filter function to identify messages that should be processed with NLP
pub fn filter_nlp_messages(env: Arc<BotEnv>, msg: Message) -> Option<Message> {
    // Skip if NLP is disabled
    if !env.config.nlp.enabled {
        return None;
    }

    // Skip messages without text or without caption
    let text = msg.text().or_else(|| msg.caption())?;

    // Skip if text starts with '--'
    if text.starts_with("--") {
        return None;
    }

    // Skip bot commands (those starting with '/')
    if text.starts_with('/') {
        return None;
    }

    // Always process messages in private chats (DMs with the bot)
    if msg.chat.is_private() {
        return Some(msg);
    }

    // Skip if the message specifically mentions other users but not the bot itself
    if has_mentions_but_not_bot(&msg, &env) {
        return None;
    }

    // Skip messages in passive mode
    if env.config.telegram.passive_mode {
        return None;
    }

    // Process if message is a reply to a bot message
    if let Some(replied_msg) = msg.reply_to_message() {
        if replied_msg.from.as_ref().is_some_and(|user| user.is_bot) {
            return Some(msg);
        }
    }

    // Check for trigger words defined in config
    let trigger_words = &env.config.nlp.trigger_words;

    // If no trigger words defined, then process the message
    if trigger_words.is_empty() {
        return Some(msg);
    }

    // Split text into words and normalize
    let text_words: Vec<String> = text
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
        .map(|word| word.to_lowercase())
        .collect();

    // Check if any trigger word matches a complete word in the message
    if trigger_words.iter().any(|trigger| {
        let trigger_lower = trigger.to_lowercase();
        text_words.contains(&trigger_lower)
    }) {
        return Some(msg);
    }

    None
}

/// Random message filtering based on probability and relevance
pub async fn randomly_filter_nlp_messages(
    env: Arc<BotEnv>,
    msg: Message,
) -> Option<Message> {
    // Skip if NLP is disabled
    if !env.config.nlp.enabled {
        return None;
    }

    // Skip messages in passive mode
    if env.config.telegram.passive_mode {
        return None;
    }

    // Get random chance from config
    let random_chance = env.config.nlp.random_answer_probability;
    if random_chance == 0.0 {
        return None;
    }

    // Roll the dice
    let roll: f64 = rand::random_range(0.0..100.0);
    if roll > random_chance {
        // Skip if the roll is greater than the chance
        return None;
    }

    // Skip messages without text or without caption
    let text = msg.text().or_else(|| msg.caption())?;

    // Skip if text starts with '--'
    if text.starts_with("--") {
        return None;
    }

    // Skip bot commands (those starting with '/')
    if text.starts_with('/') {
        return None;
    }

    // Classify with small model should we participate in the conversation
    // or not
    let text = msg.text().or_else(|| msg.caption())?;

    // Get chat history for context
    let history =
        get_chat_history(&env, msg.chat.id, msg.thread_id).unwrap_or_default();

    // Classify with small model
    let classification_result =
        classify_random_request(&Arc::<BotEnv>::clone(&env), text, &history)
            .await
            .unwrap_or(false);

    if classification_result {
        return Some(msg);
    }

    None
}

/// Checks if the message mentions other users but not the bot
pub fn has_mentions_but_not_bot(msg: &Message, env: &Arc<BotEnv>) -> bool {
    let msg_entities = msg.entities();
    let Some(entities) = &msg_entities else { return false };

    let bot_username =
        env.config.telegram.token.split(':').next().unwrap_or("");
    let has_mentions = entities.iter().any(|entity| {
        match entity.kind {
            MessageEntityKind::Mention => {
                if let Some(text) = msg.text() {
                    if let Some(mention) =
                        text.get(entity.offset..entity.offset + entity.length)
                    {
                        // Check if the mention is the bot
                        let mention = mention.trim_start_matches('@');
                        if mention.eq_ignore_ascii_case(bot_username) {
                            return false;
                        }
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    });

    has_mentions
}