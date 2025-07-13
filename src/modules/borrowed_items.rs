use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestMessageContentPartImage,
    ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs,
    ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart,
    CreateChatCompletionRequestArgs, ImageDetail, ImageUrl,
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use itertools::Itertools;
use tap::Tap as _;
use teloxide::prelude::*;
use teloxide::types::{
    InlineKeyboardButton, InlineKeyboardMarkup, MessageId, ParseMode,
    ReplyMarkup, ThreadId, User,
};
use teloxide::utils::html;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::common::{BotEnv, UpdateHandler};
use crate::db::{DbChatId, DbUserId};
use crate::utils::{BotExt, Sqlizer};
use crate::{models, schema};

// Maximum number of images that can be sent in a single request to OpenAI
// GPT-4 supports up to 16 images, but we limit the number to save tokens
const MAX_IMAGES_PER_REQUEST: usize = 4;

/// An entry in the media messages cache
struct MediaCacheEntry {
    /// Messages belonging to the same media group
    messages: Vec<Message>,
    /// Creation time of the entry for TTL purposes
    created_at: Instant,
}

/// Cache of media messages, indexed by (`chat_id`, `media_group_id`)
struct MediaGroupCache {
    /// Map of (`chat_id`, `media_group_id`) -> `MediaCacheEntry`
    cache: HashMap<(ChatId, String), MediaCacheEntry>,
    /// Time-to-live for cache entries (in seconds)
    ttl: Duration,
}

impl MediaGroupCache {
    /// Creates a new cache with the specified TTL
    fn new(ttl_seconds: u64) -> Self {
        Self { cache: HashMap::new(), ttl: Duration::from_secs(ttl_seconds) }
    }

    /// Adds a message to the cache if it belongs to a media group
    fn add_message(&mut self, msg: Message) {
        // If the message belongs to a media group
        if let Some(media_group_id) = msg.media_group_id() {
            let key = (msg.chat.id, media_group_id.to_string());

            // Clean old entries
            self.clean_old_entries();

            // Add message to an existing entry or create a new one
            match self.cache.get_mut(&key) {
                Some(entry) => {
                    // Check if this message already exists (by message_id)
                    if !entry.messages.iter().any(|m| m.id == msg.id) {
                        entry.messages.push(msg);
                    }
                }
                None => {
                    self.cache.insert(
                        key,
                        MediaCacheEntry {
                            messages: vec![msg],
                            created_at: Instant::now(),
                        },
                    );
                }
            }
        }
    }

    /// Gets all messages from the specified media group
    fn get_media_group_messages(
        &mut self,
        chat_id: ChatId,
        media_group_id: &str,
    ) -> Vec<Message> {
        // Clean old entries
        self.clean_old_entries();

        // Get messages from the cache
        let key = (chat_id, media_group_id.to_string());
        self.cache
            .get(&key)
            .map_or_else(Vec::new, |entry| entry.messages.clone())
    }

    /// Cleans old entries from the cache
    fn clean_old_entries(&mut self) {
        let now = Instant::now();
        self.cache
            .retain(|_, entry| now.duration_since(entry.created_at) < self.ttl);
    }
}

static MEDIA_CACHE: LazyLock<Arc<Mutex<MediaGroupCache>>> =
    LazyLock::new(|| Arc::new(Mutex::new(MediaGroupCache::new(300))));

// Set of tokens for tracking media groups being processed
static PROCESSING_TOKENS: LazyLock<Arc<Mutex<HashSet<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashSet::new())));

// Wait time for collecting all messages in a media group (in milliseconds)
const MEDIA_GROUP_WAIT_TIME_MS: u64 = 500;

/// Adds a message to the global media message cache
pub fn add_message_to_cache(msg: &Message) {
    if msg.media_group_id().is_some() {
        if let Ok(mut cache) = MEDIA_CACHE.lock() {
            cache.add_message(msg.clone());
        } else {
            log::error!("Failed to acquire lock on media cache");
        }
    }
}

/// Gets all messages from the specified media group from the global cache
pub fn get_media_group_messages(
    chat_id: ChatId,
    media_group_id: &str,
) -> Vec<Message> {
    match MEDIA_CACHE.lock() {
        Ok(mut cache) => {
            cache.get_media_group_messages(chat_id, media_group_id)
        }
        Err(e) => {
            log::error!("Failed to acquire lock on media cache: {e}");
            Vec::new()
        }
    }
}

/// Schedules delayed processing of a media group
pub fn schedule_media_group_processing(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> bool {
    if let Some(media_group_id) = msg.media_group_id() {
        // Add the message to the cache
        add_message_to_cache(&msg);

        // Create a unique token for this media group
        let token = format!("{}:{}", msg.chat.id.0, media_group_id);

        // Check if processing is already scheduled
        let should_schedule = {
            match PROCESSING_TOKENS.lock() {
                Ok(mut tokens) => {
                    if tokens.contains(&token) {
                        false
                    } else {
                        tokens.insert(token.clone());
                        true
                    }
                }
                Err(e) => {
                    log::error!(
                        "Failed to acquire lock on processing tokens: {e}"
                    );
                    false
                }
            }
        };

        if should_schedule {
            let bot_clone = bot;
            let env_clone = Arc::clone(&env);
            let token_clone = token;
            // Clone msg before moving it into the async block
            let msg_clone = msg.clone();
            let media_group_id_clone = media_group_id.to_string(); // Clone media_group_id as well

            tokio::spawn(async move {
                // Wait the specified time to allow all messages in the group to arrive
                sleep(Duration::from_millis(MEDIA_GROUP_WAIT_TIME_MS)).await;

                log::info!(
                    "Processing media group {media_group_id_clone} after waiting"
                );

                // Get all messages from the cache
                let all_messages = get_media_group_messages(
                    msg_clone.chat.id,
                    &media_group_id_clone,
                );

                if all_messages.is_empty() {
                    log::warn!(
                        "No messages found in cache for media group {media_group_id_clone}"
                    );
                    // Use the original message if the cache is empty
                    process_media_group_message(
                        bot_clone.clone(),
                        Arc::<BotEnv>::clone(&env_clone),
                        msg_clone.clone(), // Use the clone here
                    )
                    .await;
                } else {
                    // Sort messages by ID and choose the earliest one
                    // Important: We pass the FIRST message from the group to the handler,
                    // but the handler itself (classify_openai) will need to get ALL messages in the group from the cache again.
                    let first_message = all_messages
                        .into_iter()
                        .min_by_key(|m| m.id.0) // Use the internal numerical value of MessageId
                        .unwrap_or_else(|| msg_clone.clone()); // Use the clone here as fallback

                    process_media_group_message(
                        bot_clone,
                        env_clone,
                        first_message,
                    )
                    .await;
                }

                // Remove the token after processing
                if let Ok(mut tokens) = PROCESSING_TOKENS.lock() {
                    tokens.remove(&token_clone);
                } else {
                    log::error!("Failed to acquire lock on processing tokens after processing");
                }
            });

            return true; // Processing is scheduled
        }

        return false; // Processing was already scheduled earlier
    }

    false // Message is not from a media group
}

/// Processes a message from a media group
async fn process_media_group_message(bot: Bot, env: Arc<BotEnv>, msg: Message) {
    match handle_message(bot, env, msg).await {
        Ok(()) => {
            log::info!("Successfully processed media group message");
        }
        Err(e) => {
            log::error!("Error processing media group message: {e}");
        }
    }
}

/// Extracts JSON from an LLM response, removing markdown code and other wrappers
fn extract_json_from_llm_response(response_text: &str) -> &str {
    let text = response_text.trim();

    // Remove code blocks
    if text.starts_with("```") {
        let without_prefix =
            text.trim_start_matches("```json").trim_start_matches("```");
        if let Some(end_idx) = without_prefix.find("```") {
            return without_prefix[..end_idx].trim();
        }
        return without_prefix.trim();
    }

    // Remove single backticks
    if text.starts_with('`') && text.ends_with('`') {
        return text[1..text.len() - 1].trim();
    }

    text
}

/// Creates and returns an update handler for borrowed items commands
pub fn command_handler() -> UpdateHandler {
    // Use middleware tree, where the first filter checks
    // if the message is in the specified chat/topic
    dptree::filter(filter_messages_in_topic)
        // Then pass the message to the handler
        .endpoint(|msg: Message, bot: Bot, env: Arc<BotEnv>| async move {
            // If the message is part of a media group, schedule its processing
            if msg.media_group_id().is_some() {
                if schedule_media_group_processing(
                    bot.clone(),
                    Arc::<BotEnv>::clone(&env),
                    msg.clone(), // Clone msg here for scheduling
                ) {
                    // Processing is scheduled, nothing more to do
                    return Ok(());
                }
                // If scheduling returned false, it means processing was already scheduled,
                // so we don't need to handle the message here.
                log::debug!(
                    "Media group processing already scheduled for msg {}",
                    msg.id
                );
                return Ok(());
            }

            // If this is a regular message
            handle_message(bot, env, msg).await
        })
}

/// Creates and returns an update handler for callbacks related to borrowed items
pub fn callback_handler() -> UpdateHandler {
    dptree::filter_map(filter_callbacks).endpoint(handle_callback)
}

/// Filters messages in the specified topic
fn filter_messages_in_topic(env: Arc<BotEnv>, msg: Message) -> bool {
    env.config.telegram.chats.borrowed_items.iter().any(|c| c.has_message(&msg))
}

/// Handles incoming messages, classifying them and taking appropriate actions
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
async fn handle_message(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let Some(user) = msg.from.as_ref() else { return Ok(()) };

    let classification_result =
        classify(bot.clone(), Arc::clone(&env), &msg).await?;

    match classification_result {
        ClassificationResult::Took(items) => {
            if items.is_empty() {
                return Ok(());
            }

            let items = items
                .into_iter()
                .map(|i| models::BorrowedItem { name: i, returned: None })
                .collect_vec();

            let bot_message = bot
                .send_message(msg.chat.id, make_text(user, &items))
                .message_thread_id(msg.thread_id.unwrap())
                .parse_mode(ParseMode::Html)
                .reply_markup(ReplyMarkup::InlineKeyboard(make_keyboard(
                    msg.chat.id,
                    msg.id,
                    &items,
                )))
                .disable_notification(true)
                .await?;

            env.transaction(|conn| {
                diesel::insert_into(schema::borrowed_items::table)
                    .values(models::BorrowedItems {
                        chat_id: msg.chat.id.into(),
                        thread_id: msg.thread_id.unwrap().into(),
                        user_message_id: msg.id.into(),
                        bot_message_id: bot_message.id.into(),
                        user_id: msg.from.unwrap().id.into(),
                        items: Sqlizer::new(items).unwrap(),
                        created_at: chrono::Utc::now().naive_utc(),
                    })
                    .execute(conn)?;
                Ok(())
            })?;

            bot.pin_chat_message(msg.chat.id, msg.id)
                .disable_notification(true)
                .await?;
        }
        ClassificationResult::Returned(returned_items) => {
            // Processing returned items
            if returned_items.is_empty() {
                return Ok(());
            }

            // Find items that the user took and hasn't returned yet
            let borrowed_items = env.transaction(
                |conn| -> diesel::QueryResult<Vec<models::BorrowedItems>> {
                    schema::borrowed_items::table
                        .filter(
                            schema::borrowed_items::user_id
                                .eq(DbUserId::from(user.id)),
                        )
                        .filter(
                            schema::borrowed_items::chat_id
                                .eq(DbChatId::from(msg.chat.id)),
                        )
                        .load(conn)
                },
            )?;

            // If the user has unreturned items, use LLM for matching
            if !borrowed_items.is_empty() {
                // Collect all unreturned items from all messages
                let mut unreturned_items = Vec::new();
                for borrowed in &borrowed_items {
                    let items = (*borrowed.items).clone();
                    for item in items {
                        if item.returned.is_none() {
                            unreturned_items.push((borrowed.clone(), item));
                        }
                    }
                }

                if !unreturned_items.is_empty() {
                    // Use LLM to match returned items with unreturned ones
                    let unreturned_names = unreturned_items
                        .iter()
                        .map(|(_, item)| item.name.clone())
                        .collect_vec();
                    let matches = match_returned_items_with_llm(
                        &env,
                        &returned_items,
                        &unreturned_names,
                    )
                    .await?;

                    let mut returned_count = 0;
                    let mut borrowed_to_matched_names: HashMap<
                        String,
                        Vec<String>,
                    > = HashMap::new();

                    // Process each match
                    for (returned_idx, unreturned_idx) in matches {
                        if returned_idx < returned_items.len()
                            && unreturned_idx < unreturned_items.len()
                        {
                            let (borrowed_item, borrowed_subitem) =
                                &unreturned_items[unreturned_idx];

                            // Create a string key from chat_id and message_id
                            let chat_id: ChatId = borrowed_item.chat_id.into();
                            let msg_id: MessageId =
                                borrowed_item.user_message_id.into();
                            let key = format!("{}:{}", chat_id.0, msg_id.0);

                            borrowed_to_matched_names
                                .entry(key)
                                .or_default()
                                .push(borrowed_subitem.name.clone());

                            returned_count += 1;
                        }
                    }

                    // Update all found items
                    for borrowed in &borrowed_items {
                        let chat_id: ChatId = borrowed.chat_id.into();
                        let msg_id: MessageId = borrowed.user_message_id.into();
                        let key = format!("{}:{}", chat_id.0, msg_id.0);

                        if let Some(matched_names) =
                            borrowed_to_matched_names.get(&key)
                        {
                            let mut items = (*borrowed.items).clone();
                            let mut updated = false;

                            // Update item statuses
                            for item in &mut items {
                                if item.returned.is_none()
                                    && matched_names.contains(&item.name)
                                {
                                    item.returned = Some(chrono::Utc::now());
                                    updated = true;
                                }
                            }

                            if updated {
                                // Update record in the database
                                env.transaction(|conn| {
                                    diesel::update(
                                        schema::borrowed_items::table,
                                    )
                                    .filter(
                                        schema::borrowed_items::chat_id
                                            .eq(borrowed.chat_id),
                                    )
                                    .filter(
                                        schema::borrowed_items::user_message_id
                                            .eq(borrowed.user_message_id),
                                    )
                                    .set(schema::borrowed_items::items.eq(
                                        Sqlizer::new(items.clone()).unwrap(),
                                    ))
                                    .execute(conn)
                                })?;

                                // Update the message
                                let all_returned =
                                    items.iter().all(|i| i.returned.is_some());
                                let mut edit = bot
                                    .edit_message_text(
                                        ChatId::from(borrowed.chat_id),
                                        MessageId::from(
                                            borrowed.bot_message_id,
                                        ),
                                        make_text(user, &items),
                                    )
                                    .parse_mode(ParseMode::Html);

                                if !all_returned {
                                    edit = edit.reply_markup(make_keyboard(
                                        ChatId::from(borrowed.chat_id),
                                        MessageId::from(
                                            borrowed.user_message_id,
                                        ),
                                        &items,
                                    ));
                                }
                                edit.await.ok();

                                // If all items are returned, unpin the message
                                if all_returned {
                                    bot.unpin_chat_message(ChatId::from(
                                        borrowed.chat_id,
                                    ))
                                    .message_id(MessageId::from(
                                        borrowed.user_message_id,
                                    ))
                                    .await
                                    .ok();
                                }
                            }
                        }
                    }

                    // Inform the user about the result
                    if returned_count > 0 {
                        bot.reply_message(
                            &msg,
                            format!(
                                "Marked as returned: {returned_count} item(s)"
                            ),
                        )
                        .await?;
                        return Ok(());
                    }
                }
            }

            // Fallback: use simple fuzzy matching if LLM didn't work or there are no items
            let mut returned_count = 0;
            let mut updated_items = Vec::new();

            // For each item that the user wants to return
            for borrowed in borrowed_items {
                let mut items = (*borrowed.items).clone();
                let mut updated = false;

                for item in &mut items {
                    // The item hasn't been returned yet
                    if item.returned.is_none() {
                        // Check if the name matches one of the returned items
                        for returned_name in &returned_items {
                            // Use fuzzy matching: check substring inclusion in both directions
                            // and also look for common words
                            if item
                                .name
                                .to_lowercase()
                                .contains(&returned_name.to_lowercase())
                                || returned_name
                                    .to_lowercase()
                                    .contains(&item.name.to_lowercase())
                                || has_common_words(&item.name, returned_name)
                            {
                                // Mark the item as returned
                                item.returned = Some(chrono::Utc::now());
                                returned_count += 1;
                                updated = true;
                                break;
                            }
                        }
                    }
                }

                // If any items were marked as returned
                if updated {
                    let updated_item = (
                        borrowed.chat_id,
                        borrowed.thread_id,
                        borrowed.user_message_id,
                        borrowed.bot_message_id,
                        borrowed.user_id,
                        Sqlizer::new(items.clone()).unwrap(),
                    );
                    updated_items.push((borrowed, updated_item, items));
                }
            }

            // Update database and messages
            for (borrowed, updated_item, items) in updated_items {
                // Update in database
                env.transaction(|conn| {
                    diesel::update(schema::borrowed_items::table)
                        .filter(
                            schema::borrowed_items::chat_id
                                .eq(borrowed.chat_id),
                        )
                        .filter(
                            schema::borrowed_items::user_message_id
                                .eq(borrowed.user_message_id),
                        )
                        .set(schema::borrowed_items::items.eq(&updated_item.5))
                        .execute(conn)
                })?;

                // Update message
                let all_returned = items.iter().all(|i| i.returned.is_some());
                let mut edit = bot
                    .edit_message_text(
                        ChatId::from(borrowed.chat_id),
                        MessageId::from(borrowed.bot_message_id),
                        make_text(user, &items),
                    )
                    .parse_mode(ParseMode::Html);

                if !all_returned {
                    edit = edit.reply_markup(make_keyboard(
                        ChatId::from(borrowed.chat_id),
                        MessageId::from(borrowed.user_message_id),
                        &items,
                    ));
                }
                edit.await.ok();

                // If all items are returned, unpin the message
                if all_returned {
                    bot.unpin_chat_message(ChatId::from(borrowed.chat_id))
                        .message_id(MessageId::from(borrowed.user_message_id))
                        .await
                        .ok();
                }
            }

            // Inform the user about the result
            if returned_count > 0 {
                bot.reply_message(
                    &msg,
                    format!("Marked as returned: {returned_count} item(s)"),
                )
                .await?;
            } else {
                bot.reply_message(&msg, "No items found to return. Use the buttons under the messages about borrowed items to return them, or specify the exact name of the item being returned.")
                    .await?;
            }
        }
        ClassificationResult::Unknown => return Ok(()),
    }

    Ok(())
}

/// Represents callback data for borrowed items interactions
#[derive(Debug, Clone, Copy)]
struct CallbackData {
    chat_id: ChatId,
    user_message_id: MessageId,
    item_index: usize,
}

/// Filters and extracts callback data from callback queries
fn filter_callbacks(callback: CallbackQuery) -> Option<CallbackData> {
    let data = callback.data.as_ref()?.strip_prefix("b:")?;
    let mut split = data.split(':');
    let chat_id = split.next()?.parse::<i64>().ok()?;
    let user_message_id = split.next()?.parse::<i32>().ok()?;
    let item_index = split.next()?.parse::<usize>().ok()?;
    if split.next().is_some() {
        return None;
    }
    Some(CallbackData {
        chat_id: ChatId(chat_id),
        user_message_id: MessageId(user_message_id),
        item_index,
    })
}

/// Represents possible callback handling results
enum CallbackResponse {
    NotYourMessage,
    AlreadyReturned,
    Update(models::BorrowedItems),
}

/// Handles callback queries for returning borrowed items
async fn handle_callback(
    bot: Bot,
    env: Arc<BotEnv>,
    cd: CallbackData,
    callback: CallbackQuery,
) -> Result<()> {
    let resp = env.transaction(|conn| {
        let mut bi: models::BorrowedItems = schema::borrowed_items::table
            .filter(schema::borrowed_items::chat_id.eq(cd.chat_id.0))
            .filter(
                schema::borrowed_items::user_message_id
                    .eq(cd.user_message_id.0),
            )
            .first(conn)?;

        if callback.from.id != UserId::from(bi.user_id) {
            return Ok(CallbackResponse::NotYourMessage);
        }

        if (bi.items.as_ref())[cd.item_index].returned.is_some() {
            return Ok(CallbackResponse::AlreadyReturned);
        }
        bi.items = bi
            .items
            .map(|items| {
                let mut items = items.clone();
                items[cd.item_index].returned = Some(chrono::Utc::now());
                items
            })
            .expect("Failed to serialize borrowed items");

        diesel::update(schema::borrowed_items::table)
            .filter(schema::borrowed_items::chat_id.eq(cd.chat_id.0))
            .filter(
                schema::borrowed_items::user_message_id
                    .eq(cd.user_message_id.0),
            )
            .set(schema::borrowed_items::items.eq(&bi.items))
            .execute(conn)?;

        Ok(CallbackResponse::Update(bi))
    });

    match resp {
        Ok(CallbackResponse::NotYourMessage) => {
            bot.answer_callback_query(callback.id)
                .text("This is not your message.")
                .await?;
            Ok(())
        }
        Ok(CallbackResponse::AlreadyReturned) => {
            bot.answer_callback_query(callback.id)
                .text("This item is already returned.")
                .await?;
            Ok(())
        }
        Ok(CallbackResponse::Update(bi)) => {
            bot.answer_callback_query(callback.id).await?;
            let all_returned = bi.items.iter().all(|i| i.returned.is_some());
            let mut edit = bot
                .edit_message_text(
                    cd.chat_id,
                    bi.bot_message_id.into(),
                    make_text(&callback.from, &bi.items),
                )
                .parse_mode(ParseMode::Html);
            if !all_returned {
                edit = edit.reply_markup(make_keyboard(
                    cd.chat_id,
                    cd.user_message_id,
                    &bi.items,
                ));
            }
            edit.await.ok();
            if all_returned {
                bot.unpin_chat_message(cd.chat_id)
                    .message_id(cd.user_message_id)
                    .await?;
            }
            Ok(())
        }
        Err(e) => {
            bot.answer_callback_query(callback.id)
                .text("Internal error")
                .await?;
            Err(e.into())
        }
    }
}

/// Represents the result of classifying a message
#[derive(Clone, Debug)]
enum ClassificationResult {
    Took(Vec<String>),
    Returned(Vec<String>),
    Unknown,
}

/// Classifies messages about taking or returning items
async fn classify(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: &Message,
) -> Result<ClassificationResult> {
    // We need text later for fallback classification, so get it here.
    let text_option = textify_message(msg);

    if env.config.services.openai.disable {
        return text_option.map_or_else(
            || Ok(ClassificationResult::Unknown),
            |text| classify_dumb(&text),
        );
    }

    // Check if message has any content (text, photo, or part of media group)
    // If not, classify_openai will handle returning Unknown appropriately.
    classify_openai(bot, env, msg, text_option.as_deref().unwrap_or_default())
        .await
}

/// Simple rule-based classification for when `OpenAI` is disabled
#[allow(clippy::unnecessary_wraps)] // for consistency
fn classify_dumb(text: &str) -> Result<ClassificationResult> {
    if let Some(text) = text.strip_prefix("took") {
        let items: Vec<_> = text
            .trim()
            .split(' ')
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .collect_vec();
        let items = items.into_iter().filter(|s| !s.is_empty()).collect_vec();
        if items.is_empty() {
            return Ok(ClassificationResult::Unknown);
        }
        return Ok(ClassificationResult::Took(items));
    } else if text.starts_with("return") || text.starts_with("returned") {
        // Simple implementation to support commands like "return hammer"
        let text_parts: Vec<_> = text.split_whitespace().collect();
        if text_parts.len() > 1 {
            let items =
                text_parts[1..].iter().map(|s| (*s).to_string()).collect_vec();
            return Ok(ClassificationResult::Returned(items));
        }
    }

    Ok(ClassificationResult::Unknown)
}

const METRIC_NAME: &str = "botka_openai_used_tokens_total";

/// Registers metrics for `OpenAI` API usage
pub fn register_metrics() {
    metrics::register_counter!(
        METRIC_NAME,
        "type" => "prompt",
    );
    metrics::register_counter!(
        METRIC_NAME,
        "type" => "completion",
    );
    metrics::describe_counter!(
        METRIC_NAME,
        "Total number of tokens used by OpenAI API."
    );
}

/// Uses `OpenAI` to classify messages about taking or returning items
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
async fn classify_openai(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: &Message,
    // We accept the original text here mainly for fallback/logging
    original_text: &str,
) -> Result<ClassificationResult> {
    let mut message_parts: Vec<ChatCompletionRequestUserMessageContentPart> =
        Vec::new();
    let effective_text: String; // Text actually sent to OpenAI (could be aggregated)
    let mut image_count = 0;

    // Check if it's a media group
    if let Some(media_group_id) = msg.media_group_id() {
        log::debug!("Processing as media group: {media_group_id}");
        let media_messages =
            get_media_group_messages(msg.chat.id, media_group_id);

        if media_messages.is_empty() {
            // This might happen if the scheduled task runs before all messages are cached
            log::warn!("Media group {media_group_id} cache empty, attempting short wait");
            sleep(Duration::from_millis(100)).await; // Brief wait
            let media_messages =
                get_media_group_messages(msg.chat.id, media_group_id);

            if media_messages.is_empty() {
                log::error!("Media group {media_group_id} cache still empty after wait. Cannot process.");
                return Ok(ClassificationResult::Unknown);
            }
            log::debug!(
                "Found {} messages in media group {} from cache after extra wait",
                media_messages.len(),
                media_group_id
            );
        } else {
            log::debug!(
                "Found {} messages in media group {} from cache",
                media_messages.len(),
                media_group_id
            );
        }

        // Aggregate text/captions from all messages
        let mut aggregated_text = String::new();
        for media_msg in &media_messages {
            if let Some(current_text) = textify_message(media_msg) {
                if !aggregated_text.is_empty() {
                    aggregated_text.push('\n');
                }
                aggregated_text.push_str(&current_text);
            }
        }
        effective_text = aggregated_text; // Store aggregated text for logging

        // Add text part if aggregated text is not empty
        if !effective_text.is_empty() {
            message_parts.push(
                ChatCompletionRequestUserMessageContentPart::Text(
                    ChatCompletionRequestMessageContentPartText {
                        text: effective_text.clone(),
                    },
                ),
            );
        }

        // Add image parts
        for media_msg in media_messages {
            // Iterate again for photos
            if let Some(photos) = media_msg.photo() {
                if image_count >= MAX_IMAGES_PER_REQUEST {
                    log::warn!("Maximum number of images reached ({MAX_IMAGES_PER_REQUEST}), skipping remaining images for media group {media_group_id}");
                    break;
                }
                if let Some(largest_photo) = photos.last() {
                    match bot.get_file(&largest_photo.file.id).await {
                        Ok(file) => {
                            let file_url = format!(
                                "https://api.telegram.org/file/bot{}/{}",
                                bot.token(),
                                file.path
                            );
                            log::debug!("Adding image from media group {media_group_id}, URL: {file_url}");
                            message_parts.push(ChatCompletionRequestUserMessageContentPart::ImageUrl(
                                ChatCompletionRequestMessageContentPartImage {
                                    image_url: ImageUrl { url: file_url, detail: Some(ImageDetail::Auto) },
                                },
                            ));
                            image_count += 1;
                        }
                        Err(e) => {
                            log::error!("Failed to get file for photo in media group {media_group_id}: {e}");
                            // Continue processing other images/messages
                        }
                    }
                }
            }
        }
    } else {
        // Single message case
        log::debug!("Processing as single message: {}", msg.id);
        effective_text = original_text.to_string(); // Use the original text

        // Add text part if not empty
        if !effective_text.is_empty() {
            message_parts.push(
                ChatCompletionRequestUserMessageContentPart::Text(
                    ChatCompletionRequestMessageContentPartText {
                        text: effective_text.clone(),
                    },
                ),
            );
        }

        // Add image part if present
        if let Some(photos) = msg.photo() {
            if let Some(largest_photo) = photos.last() {
                if image_count < MAX_IMAGES_PER_REQUEST {
                    // Should always be 0 here, but check anyway
                    match bot.get_file(&largest_photo.file.id).await {
                        Ok(file) => {
                            let file_url = format!(
                                "https://api.telegram.org/file/bot{}/{}",
                                bot.token(),
                                file.path
                            );
                            log::debug!(
                                "Adding single image from URL: {file_url}"
                            );
                            message_parts.push(ChatCompletionRequestUserMessageContentPart::ImageUrl(
                                ChatCompletionRequestMessageContentPartImage {
                                    image_url: ImageUrl { url: file_url, detail: Some(ImageDetail::Auto) },
                                },
                            ));
                            image_count += 1;
                        }
                        Err(e) => {
                            log::error!(
                                "Failed to get file for single photo: {e}"
                            );
                        }
                    }
                } else {
                    log::warn!("Maximum number of images already reached ({MAX_IMAGES_PER_REQUEST}) processing single message?");
                    // Should not happen
                }
            }
        }
    }

    // Check if we have any content (text or images) to send
    if message_parts.is_empty() {
        log::warn!(
            "No text or images found to send for classification (msg_id: {})",
            msg.id
        );
        return Ok(ClassificationResult::Unknown);
    }

    let model = &env.config.services.openai.model;

    let request = CreateChatCompletionRequestArgs::default()
        .max_tokens(256u16)
        .model(model)
        .messages([
            ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessageArgs::default()
                    .content(PROMPT.trim())
                    .build()?,
            ),
            ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessageArgs::default()
                    .content(ChatCompletionRequestUserMessageContent::Array(
                        message_parts, // Use the constructed parts
                    ))
                    .build()?,
            ),
        ])
        .build()?;

    log::debug!(
        "Sending OpenAI request for classification: text='{effective_text}', image_count={image_count}"
    );

    let response = env
        .openai_client
        .chat()
        .create(request)
        .await
        .tap(|r| crate::metrics::update_service("openai", r.is_ok()))?;

    if let Some(usage) = response.usage {
        log::info!(
            "OpenAI classification usage: prompt_tokens={}, completion_tokens={}, total_tokens={}",
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens
        );

        metrics::counter!(
            METRIC_NAME,
            usage.prompt_tokens.into(),
            "type" => "prompt",
        );
        metrics::counter!(
            METRIC_NAME,
            usage.completion_tokens.into(),
            "type" => "completion",
        );
    }

    let response_text = response
        .choices
        .first()
        .context("Empty list of choices from OpenAI")?
        .message
        .content
        .as_ref()
        .context("No content in OpenAI response")?
        .as_str();

    log::debug!("Received OpenAI response: {response_text}");

    // Extract JSON from the response
    let json_text = extract_json_from_llm_response(response_text);
    log::debug!("Extracted JSON: {json_text}");

    // Parse the response
    if let Ok(json_data) = serde_json::from_str::<serde_json::Value>(json_text)
    {
        log::debug!("Parsed JSON response: {json_data:?}");

        if let Some(action) = json_data["action"].as_str() {
            if action == "taking" {
                if let Some(items_array) = json_data["items"].as_array() {
                    let items = items_array
                        .iter()
                        .filter_map(|item| item.as_str().map(|s| s.to_string()))
                        .filter(|s| !s.is_empty())
                        .collect_vec();

                    log::info!(
                        "Classification result: taking items: {items:?}"
                    );
                    if !items.is_empty() {
                        return Ok(ClassificationResult::Took(items));
                    }
                }
            } else if action == "returning" {
                if let Some(items_array) = json_data["items"].as_array() {
                    let items = items_array
                        .iter()
                        .filter_map(|item| item.as_str().map(|s| s.to_string()))
                        .filter(|s| !s.is_empty())
                        .collect_vec();

                    log::info!(
                        "Classification result: returning items: {items:?}"
                    );
                    if !items.is_empty() {
                        return Ok(ClassificationResult::Returned(items));
                    }
                }
            } else {
                log::info!("Classification result: unknown action '{action}'");
            }
        } else {
            log::warn!("JSON response missing 'action' field");
        }
    }

    log::info!("Classification result: unknown");
    Ok(ClassificationResult::Unknown)
}

/// Converts a message to text suitable for `OpenAI` API.
/// Combines both the message text and caption (if present) into a single string.
/// Returns None if neither text nor caption is present.
fn textify_message(msg: &Message) -> Option<String> {
    let mut result = String::new();
    if let Some(text) = msg.text() {
        result.push_str(text);
    }
    if let Some(caption) = msg.caption() {
        if !result.is_empty() && !caption.is_empty() {
            result.push('\n');
        }
        result.push_str(caption);
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Generates a formatted text message for displaying borrowed items.
/// Includes:
/// - List of currently borrowed (unreturned) items with user mention
/// - Chronological list of returned items with timestamps
/// - Appropriate call-to-action message based on context
fn make_text(user: &User, items: &[models::BorrowedItem]) -> String {
    let mut text = String::new();
    let mut prev_date: Option<DateTime<_>> = None;

    // List of borrowed (unreturned) items
    let unreturned_items: Vec<&str> = items
        .iter()
        .filter(|i| i.returned.is_none())
        .map(|i| i.name.as_str())
        .collect();

    if !unreturned_items.is_empty() {
        text.push_str(&html::user_mention(user.id, &user.full_name()));
        text.push_str(" took: ");
        text.push_str(
            &unreturned_items
                .iter()
                .map(|name| html::escape(name))
                .collect::<Vec<_>>()
                .join(", "),
        );
        text.push_str("\n\n");
    }

    // List of returned items (original logic)
    for (name, returned) in items
        .iter()
        .filter_map(|i| Some((i.name.as_str(), i.returned?)))
        .sorted_by_key(|(_, r)| *r)
    {
        match prev_date {
            Some(p) if returned - p < chrono::Duration::minutes(10) => {
                text.push_str(", ");
            }
            _ => {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&returned.format("%Y-%m-%d %H:%M").to_string());
                text.push_str(": returned ");
                prev_date = Some(returned);
            }
        }
        text.push_str(&html::escape(name));
    }

    if text.is_empty() {
        text.push_str(&html::user_mention(user.id, &user.full_name()));
        text.push_str(", press a button to mark an item as returned.");
    } else if !unreturned_items.is_empty() {
        text.push_str("\nPress the button when you return the item.");
    }

    text
}

/// Creates an inline keyboard markup for borrowed items.
/// Each item is represented by a button showing its name and status (✅ for returned, 🕐 for borrowed).
/// Buttons are arranged in balanced columns for better presentation.
fn make_keyboard(
    chat_id: ChatId,
    user_message_id: MessageId,
    items: &[models::BorrowedItem],
) -> InlineKeyboardMarkup {
    let buttons = items.iter().enumerate().map(|(i, item)| {
        InlineKeyboardButton::callback(
            format!(
                "{} {}",
                if item.returned.is_some() { "✅" } else { "🕐" },
                item.name
            ),
            format!("b:{}:{}:{}", chat_id.0, user_message_id.0, i),
        )
    });
    InlineKeyboardMarkup { inline_keyboard: balance_columns(3, buttons) }
}

/// Arranges items into balanced columns for better visual presentation.
///
/// # Arguments
/// * `max_columns` - Maximum number of columns allowed
/// * `it` - Iterator over items to arrange
///
/// # Returns
/// Vector of rows, where each row is a vector of items
fn balance_columns<T>(
    max_columns: usize,
    mut it: impl ExactSizeIterator<Item = T>,
) -> Vec<Vec<T>> {
    let rows = it.len().div_ceil(max_columns);
    let columns = it.len() / rows;
    let rows_with_extra_columns = it.len() % rows;

    let mut result = vec![];
    for irow in 0..rows {
        let mut row = Vec::new();
        for _ in 0..(columns + usize::from(irow < rows_with_extra_columns)) {
            row.push(it.next().expect(""));
        }
        result.push(row);
    }
    result
}

/// System prompt for `OpenAI` to classify messages about borrowed items.
/// Instructs the model to:
/// - Identify if items are being taken or returned
/// - Extract item names from text and images
/// - Format response as JSON with action and items list
const PROMPT: &str = r#"""
Classify messages (which may include text and/or an image) about taking or returning items.
Respond in JSON format with the following structure:

{
  "action": "taking" | "returning" | "unknown",
  "items": [
    "item name 1",
    "item name 2",
    ...
  ]
}

IMPORTANT: Return ONLY the raw JSON without any markdown formatting, code blocks, or backticks.

If a user is taking item(s), set "action" to "taking" and include all items being taken in the "items" array.
If a user is returning item(s), set "action" to "returning" and include all items being returned in the "items" array.
If the message doesn't clearly indicate taking or returning items, set "action" to "unknown" and leave "items" as an empty array.

Extract item names from the text and/or image provided.
Put item names in their base/nominative form.
Group similar items when appropriate.
Make item names as concise as possible.
If an item name is unclear, use an empty string or a generic term like "thing" or "item".
Use English names for items, even if the message is in another language.
Remove empty strings from the final array if possible.
"""#;

/// Checks if two strings have common words.
/// Words shorter than 3 characters are ignored.
/// Returns true if any significant word is shared between strings.
fn has_common_words(s1: &str, s2: &str) -> bool {
    let s1_lower = s1.to_lowercase();
    let s2_lower = s2.to_lowercase();

    let words1: Vec<&str> = s1_lower.split_whitespace().collect();
    let words2: Vec<&str> = s2_lower.split_whitespace().collect();

    for word1 in &words1 {
        if word1.len() < 3 {
            continue; // Skip short words
        }

        for word2 in &words2 {
            if word2.len() < 3 {
                continue; // Skip short words
            }

            if word1 == word2 || word1.contains(word2) || word2.contains(word1)
            {
                return true;
            }
        }
    }
    false
}

/// Uses Language Model to match returned items with previously borrowed ones.
///
/// # Arguments
/// * `env` - Environment containing `OpenAI` client
/// * `returned_items` - List of items being returned
/// * `unreturned_items` - List of items previously borrowed but not returned
///
/// # Returns
/// Vector of pairs (`returned_idx`, `unreturned_idx`) representing matches between items
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
async fn match_returned_items_with_llm(
    env: &Arc<BotEnv>,
    returned_items: &[String],
    unreturned_items: &[String],
) -> Result<Vec<(usize, usize)>> {
    if returned_items.is_empty() || unreturned_items.is_empty() {
        return Ok(Vec::new());
    }

    let model = &env.config.services.openai.model;

    let prompt = format!(
        "Determine which items a user is returning match with items they previously borrowed.\n\n\
        Items user wants to return: {}\n\n\
        Items user previously borrowed and hasn't returned: {}\n\n\
        Format your response as JSON array of [returned_index, unreturned_index] pairs, \
        where returned_index is the index in the 'returning' list and unreturned_index is the \
        index in the 'unreturned' list. Only include matches that are confident, \
        and return an empty array if no confident matches. The goal is to match even if \
        the spelling or wording is different but refers to the same item.\n\n\
        IMPORTANT: Return ONLY the raw JSON array without any markdown formatting, code blocks, or backticks.",
        serde_json::to_string(returned_items)?,
        serde_json::to_string(unreturned_items)?,
    );

    log::debug!("Sending OpenAI request for item matching:\nReturned items: {returned_items:?}\nUnreturned items: {unreturned_items:?}");

    let request = CreateChatCompletionRequestArgs::default()
        .max_tokens(256u16)
        .model(model)
        .messages([
            ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessageArgs::default()
                    .content("You are a helpful assistant that matches items in lists by semantic meaning, not just exact text.")
                    .build()?,
            ),
            ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessageArgs::default()
                    .content(ChatCompletionRequestUserMessageContent::Text(prompt))
                    .build()?,
            ),
        ])
        .build()?;

    let response = env
        .openai_client
        .chat()
        .create(request)
        .await
        .tap(|r| crate::metrics::update_service("openai", r.is_ok()))?;

    if let Some(usage) = response.usage {
        log::info!(
            "OpenAI matching usage: prompt_tokens={}, completion_tokens={}, total_tokens={}",
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens
        );

        metrics::counter!(
            METRIC_NAME,
            usage.prompt_tokens.into(),
            "type" => "prompt",
        );
        metrics::counter!(
            METRIC_NAME,
            usage.completion_tokens.into(),
            "type" => "completion",
        );
    }

    let response_text = response
        .choices
        .first()
        .context("Empty list of choices from OpenAI")?
        .message
        .content
        .as_ref()
        .context("No content in OpenAI response")?
        .as_str();

    log::debug!("Received OpenAI response for item matching: {response_text}");

    let json_text = extract_json_from_llm_response(response_text);
    log::debug!("Extracted JSON for matching: {json_text}");

    if let Ok(matches) = serde_json::from_str::<Vec<(usize, usize)>>(json_text)
    {
        log::info!(
            "LLM item matching found {} matches: {:?}",
            matches.len(),
            matches
        );
        return Ok(matches);
    }
    log::warn!("Failed to parse LLM matching response as JSON: {json_text}");

    log::info!("Falling back to fuzzy matching for items");
    let mut matches = Vec::new();
    for (returned_idx, returned_name) in returned_items.iter().enumerate() {
        for (unreturned_idx, unreturned_name) in
            unreturned_items.iter().enumerate()
        {
            if unreturned_name
                .to_lowercase()
                .contains(&returned_name.to_lowercase())
                || returned_name
                    .to_lowercase()
                    .contains(&unreturned_name.to_lowercase())
                || has_common_words(unreturned_name, returned_name)
            {
                matches.push((returned_idx, unreturned_idx));
                log::debug!(
                    "Fuzzy matched '{returned_name}' with '{unreturned_name}'"
                );
                break;
            }
        }
    }

    log::info!("Fuzzy matching found {} matches: {:?}", matches.len(), matches);
    Ok(matches)
}

// ============================================================================
// Butler Reminders - Functionality for reminding users about borrowed items
// ============================================================================

/// Background task to check for overdue borrowed items and send reminders
pub async fn reminder_task(
    env: Arc<BotEnv>,
    bot: Bot,
    cancel: CancellationToken,
) {
    let Some(reminder_config) = &env.config.borrowed_items.reminders else {
        log::info!(
            "Borrowed items reminder settings are not configured, skipping task"
        );
        return;
    };

    let check_interval =
        Duration::from_secs(reminder_config.check_interval_hours * 3600);

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                log::info!("Butler reminder task cancelled");
                break;
            }
            () = sleep(check_interval) => {
                if let Err(e) = check_and_send_reminders(&env, &bot, reminder_config).await {
                    log::error!("Error in butler reminder task: {e}");
                }
            }
        }
    }
}

/// Check for overdue items and send reminders
async fn check_and_send_reminders(
    env: &Arc<BotEnv>,
    bot: &Bot,
    reminder_config: &crate::config::BorrowedItemsReminders,
) -> Result<()> {
    let overdue_threshold = Utc::now().naive_utc()
        - chrono::Duration::hours(
            i64::try_from(reminder_config.overdue_after_hours).unwrap_or(24),
        );

    // Find all borrowed items that are overdue and not returned
    let overdue_items: Vec<models::BorrowedItems> =
        env.transaction(|conn| {
            schema::borrowed_items::table
                .filter(
                    schema::borrowed_items::created_at.lt(overdue_threshold),
                )
                .load(conn)
        })?;

    for borrowed_items in overdue_items {
        // Check each item in the borrowed_items record
        for (item_index, item) in borrowed_items.items.iter().enumerate() {
            // Skip if item is already returned
            if item.returned.is_some() {
                continue;
            }

            // Check if we should send a reminder for this item
            if should_send_reminder(
                env,
                &borrowed_items,
                &item.name,
                reminder_config,
            )? {
                send_reminder(
                    env,
                    bot,
                    &borrowed_items,
                    &item.name,
                    item_index,
                )
                .await?;
                record_reminder_sent(env, &borrowed_items, &item.name)?;
            }
        }
    }

    Ok(())
}

/// Check if we should send a reminder for a specific item
fn should_send_reminder(
    env: &Arc<BotEnv>,
    borrowed_items: &models::BorrowedItems,
    item_name: &str,
    reminder_config: &crate::config::BorrowedItemsReminders,
) -> Result<bool> {
    let reminder: Option<models::BorrowedItemsReminder> =
        env.transaction(|conn| {
            schema::borrowed_items_reminders::table
                .filter(
                    schema::borrowed_items_reminders::chat_id
                        .eq(borrowed_items.chat_id),
                )
                .filter(
                    schema::borrowed_items_reminders::user_message_id
                        .eq(borrowed_items.user_message_id),
                )
                .filter(
                    schema::borrowed_items_reminders::item_name.eq(item_name),
                )
                .first(conn)
                .optional()
        })?;

    let Some(reminder) = reminder else {
        // No reminder record exists yet, so we should send the first one
        return Ok(true);
    };

    // Check if we've exceeded the maximum number of reminders
    if reminder.reminders_sent
        >= i32::try_from(reminder_config.max_reminders).unwrap_or(3)
    {
        return Ok(false);
    }

    // Check if enough time has passed since the last reminder
    if let Some(last_sent) = reminder.last_reminder_sent {
        let time_since_last = Utc::now().naive_utc() - last_sent;
        let required_interval = chrono::Duration::hours(
            i64::try_from(reminder_config.reminder_interval_hours)
                .unwrap_or(12),
        );

        if time_since_last < required_interval {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Get the current reminder count for a borrowed item
fn get_reminder_count(
    env: &Arc<BotEnv>,
    borrowed_items: &models::BorrowedItems,
    item_name: &str,
) -> Result<i32> {
    let reminder_count = env.transaction(|conn| {
        schema::borrowed_items_reminders::table
            .filter(
                schema::borrowed_items_reminders::chat_id
                    .eq(borrowed_items.chat_id),
            )
            .filter(
                schema::borrowed_items_reminders::user_message_id
                    .eq(borrowed_items.user_message_id),
            )
            .filter(schema::borrowed_items_reminders::item_name.eq(item_name))
            .select(schema::borrowed_items_reminders::reminders_sent)
            .first::<i32>(conn)
            .optional()
    })?;

    Ok(reminder_count.unwrap_or(0) + 1)
}

/// Check if this should be the final reminder
fn is_final_reminder(env: &Arc<BotEnv>, current_count: i32) -> bool {
    let max_reminders = i32::try_from(
        env.config
            .borrowed_items
            .reminders
            .as_ref()
            .map_or(3, |r| r.max_reminders),
    )
    .unwrap_or(3);
    current_count >= max_reminders
}

/// Build the reminder message text
fn build_reminder_text(
    user: &models::TgUser,
    item_name: &str,
    days_borrowed: i64,
    is_final_reminder: bool,
) -> String {
    if is_final_reminder {
        format!(
            " <b>Borrowed Item Reminder</b>\n\n\
            {} has been using <b>{}</b> from the hackerspace for {} days now.\n\n\
            Hey! Just a gentle reminder that other residents might also want to use this item. \
            If you still need it, no worries - just let us know how much longer you'll need it!",
            html::user_mention(UserId::from(user.id), &user.first_name),
            item_name,
            days_borrowed
        )
    } else {
        format!(
            "👋 <b>Borrowed Item Reminder</b>\n\n\
            Hi {}! You've been using <b>{}</b> from the hackerspace for {} days.\n\n\
            Just wondering - are you still using it, or would it be ready to return? \
            Other residents might be interested in borrowing it too. Thanks! 😊",
            user.first_name,
            item_name,
            days_borrowed
        )
    }
}

/// Send a public reminder to the borrowed items topic
async fn send_public_reminder(
    bot: &Bot,
    env: &Arc<BotEnv>,
    borrowed_items: &models::BorrowedItems,
    text: &str,
    user: &models::TgUser,
    item_name: &str,
    days_borrowed: i64,
) -> Result<()> {
    if let Some(borrowed_items_chat) =
        env.config.telegram.chats.borrowed_items.first()
    {
        bot.send_message(borrowed_items_chat.chat, text)
            .parse_mode(ParseMode::Html)
            .message_thread_id(borrowed_items_chat.thread)
            .await?;

        log::info!(
            "Sent final public reminder for user {} and item '{}' (borrowed {} days ago) to borrowed items topic",
            user.first_name,
            item_name,
            days_borrowed
        );
    } else {
        log::warn!(
            "No borrowed_items chat configured for public final reminder"
        );
        // Fallback to private reminder
        send_private_reminder(bot, borrowed_items, text).await?;
    }
    Ok(())
}

/// Send a private reminder as reply to the original message
async fn send_private_reminder(
    bot: &Bot,
    borrowed_items: &models::BorrowedItems,
    text: &str,
) -> Result<()> {
    let mut send_msg = bot
        .send_message(ChatId::from(borrowed_items.chat_id), text)
        .parse_mode(ParseMode::Html)
        .message_thread_id(ThreadId::from(borrowed_items.thread_id));

    send_msg = send_msg.reply_to_message_id(MessageId::from(
        borrowed_items.user_message_id,
    ));

    send_msg.await?;
    Ok(())
}

/// Send a reminder message to the user
async fn send_reminder(
    env: &Arc<BotEnv>,
    bot: &Bot,
    borrowed_items: &models::BorrowedItems,
    item_name: &str,
    _item_index: usize,
) -> Result<()> {
    let user: models::TgUser = env.transaction(|conn| {
        schema::tg_users::table
            .filter(schema::tg_users::id.eq(borrowed_items.user_id))
            .first(conn)
    })?;

    let days_borrowed =
        (Utc::now().naive_utc() - borrowed_items.created_at).num_days();

    let current_reminder_count = get_reminder_count(env, borrowed_items, item_name)?;
    let is_final_reminder = is_final_reminder(env, current_reminder_count);

    let text = build_reminder_text(&user, item_name, days_borrowed, is_final_reminder);

    if is_final_reminder {
        send_public_reminder(bot, env, borrowed_items, &text, &user, item_name, days_borrowed).await?;
    } else {
        send_private_reminder(bot, borrowed_items, &text).await?;
        log::info!(
            "Sent private reminder to user {} for item '{}' (borrowed {} days ago)",
            user.first_name,
            item_name,
            days_borrowed
        );
    }

    Ok(())
}

/// Record that a reminder was sent
fn record_reminder_sent(
    env: &Arc<BotEnv>,
    borrowed_items: &models::BorrowedItems,
    item_name: &str,
) -> Result<()> {
    env.transaction(|conn| {
        diesel::insert_into(schema::borrowed_items_reminders::table)
            .values((
                schema::borrowed_items_reminders::chat_id
                    .eq(borrowed_items.chat_id),
                schema::borrowed_items_reminders::user_message_id
                    .eq(borrowed_items.user_message_id),
                schema::borrowed_items_reminders::user_id
                    .eq(borrowed_items.user_id),
                schema::borrowed_items_reminders::item_name.eq(item_name),
                schema::borrowed_items_reminders::reminders_sent.eq(1),
                schema::borrowed_items_reminders::last_reminder_sent
                    .eq(Some(Utc::now().naive_utc())),
                schema::borrowed_items_reminders::created_at
                    .eq(Utc::now().naive_utc()),
            ))
            .on_conflict((
                schema::borrowed_items_reminders::chat_id,
                schema::borrowed_items_reminders::user_message_id,
                schema::borrowed_items_reminders::item_name,
            ))
            .do_update()
            .set((
                schema::borrowed_items_reminders::reminders_sent
                    .eq(schema::borrowed_items_reminders::reminders_sent + 1),
                schema::borrowed_items_reminders::last_reminder_sent
                    .eq(Some(Utc::now().naive_utc())),
            ))
            .execute(conn)?;

        Ok(())
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::BorrowedItem;

    #[test]
    fn test_make_text() {
        let item = |name: &str, minutes: Option<i64>| BorrowedItem {
            name: name.to_string(),
            returned: minutes
                .map(|m| chrono::DateTime::from_timestamp(m * 60, 0).unwrap()),
        };
        let user = User {
            id: UserId(1),
            is_bot: false,
            first_name: "John".to_string(),
            last_name: None,
            username: None,
            language_code: None,
            is_premium: false,
            added_to_attachment_menu: false,
        };
        assert_eq!(
            make_text(
                &user,
                &[item("hammer", Some(0)), item("screwdriver", Some(1))]
            ),
            "1970-01-01 00:00: returned hammer, screwdriver"
        );
        assert_eq!(
            make_text(
                &user,
                &[item("hammer", Some(0)), item("screwdriver", Some(60))]
            ),
            "1970-01-01 00:00: returned hammer\n\
            1970-01-01 01:00: returned screwdriver"
        );
    }
}
