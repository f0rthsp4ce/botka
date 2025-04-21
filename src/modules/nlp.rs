//! Natural language processing module for understanding and responding to
//! non-command messages using `OpenAI` API.
//!
//! This module allows interaction with the bot using natural language instead
//! of formal commands, triggered by specific keywords.
//!
//! Known issues:
//! - The bot works weirdly in general topic threads of forum. Idk why.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_openai::types::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestMessageContentPartImage,
    ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs,
    ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart, ChatCompletionTool,
    ChatCompletionToolChoiceOption, ChatCompletionToolType,
    CreateChatCompletionRequestArgs, FunctionObject, ImageDetail, ImageUrl,
    ResponseFormat, ResponseFormatJsonSchema,
};
use chrono::{Duration, Local, Utc};
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use log::debug;
use macro_rules_attribute::derive;
use serde::{Deserialize, Serialize};
use tap::Tap;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, Message, MessageEntityKind, ThreadId};
use teloxide::utils::command::BotCommands;
use teloxide::utils::html::escape;
use tokio::sync::RwLock;

use crate::common::{
    filter_command, is_resident, BotCommandsExt, BotEnv, UpdateHandler,
};
use crate::db::{DbChatId, DbThreadId, DbUserId};
use crate::models::{ChatHistoryEntry, Memory, NewChatHistoryEntry, NewMemory};
use crate::modules::basic::cmd_status_text;
use crate::modules::butler;
use crate::modules::needs::{add_items_text, command_needs_text};
use crate::utils::{MessageExt, ResultExt, GENERAL_THREAD_ID};

// Function call definitions
#[derive(Serialize, Deserialize, Debug)]
struct SaveMemoryArgs {
    memory_text: String,
    #[serde(default = "default_duration_hours")]
    duration_hours: Option<u32>,
    #[serde(default)]
    chat_specific: bool,
    #[serde(default)]
    thread_specific: bool,
    #[serde(default)]
    user_specific: bool,
}

#[allow(clippy::unnecessary_wraps)]
const fn default_duration_hours() -> Option<u32> {
    Some(24)
}

#[derive(Serialize, Deserialize, Debug)]
struct RemoveMemoryArgs {
    memory_id: i32,
}

#[derive(Serialize, Deserialize, Debug)]
struct ExecuteCommandArgs {
    command: String,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct SearchArgs {
    query: String,
}

// Metric name for monitoring token usage
const METRIC_NAME: &str = "botka_openai_nlp_used_tokens_total";

/// Register metrics for `OpenAI` API usage
pub fn register_metrics() {
    metrics::register_counter!(METRIC_NAME, "type" => "prompt");
    metrics::register_counter!(METRIC_NAME, "type" => "completion");
    metrics::describe_counter!(
        METRIC_NAME,
        "Total number of tokens used by OpenAI API for NLP processing."
    );
}

/// Main message handler for natural language processing
pub fn message_handler() -> UpdateHandler {
    dptree::filter_map(filter_nlp_messages).endpoint(handle_nlp_message)
}

pub fn random_message_handler() -> UpdateHandler {
    dptree::filter_map_async(randomly_filter_nlp_messages)
        .endpoint(handle_nlp_message)
}

#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(description = "show racovina camera image.")]
    #[custom(resident = true)]
    NlpDebugInfo,
}

/// Command handler for natural language processing debugging
pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(handle_command)
}

async fn handle_command(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    command: Commands,
) -> Result<()> {
    match command {
        Commands::NlpDebugInfo => {
            handle_nlp_debug_info(&bot, env, &msg)
                .await
                .context("Failed to handle NLP debug info")?;
        }
    }
    Ok(())
}

async fn handle_nlp_debug_info(
    bot: &Bot,
    env: Arc<BotEnv>,
    msg: &Message,
) -> Result<()> {
    // Get replied message info
    let Some(replied_msg) = msg.reply_to_message() else {
        bot.send_message(
            msg.chat.id,
            "Please reply to a message to get debug info.",
        )
        .reply_to_message_id(msg.id)
        .send()
        .await?;
        return Ok(());
    };

    // Load message from database
    let stored_message = {
        match env.transaction(|conn| {
            crate::schema::chat_history::table
                .filter(
                    crate::schema::chat_history::message_id
                        .eq::<i32>(replied_msg.id.0),
                )
                .filter(
                    crate::schema::chat_history::chat_id
                        .eq(DbChatId::from(msg.chat.id)),
                )
                .first::<ChatHistoryEntry>(conn)
        }) {
            Ok(entry) => entry,
            Err(diesel::result::Error::NotFound) => {
                bot.send_message(msg.chat.id, "Message not found in database.")
                    .reply_to_message_id(msg.id)
                    .send()
                    .await?;
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
    };

    // Send debug info
    let mut response = format!("Debug info for message {}:\n", replied_msg.id);
    writeln!(
        response,
        "Classification result: {}",
        stored_message.classification_result.map_or_else(
            || "None".to_string(),
            |classification| classification
        )
    )?;
    writeln!(
        response,
        "Used model: {}",
        stored_message
            .used_model
            .map_or_else(|| "None".to_string(), |model| model)
    )?;

    bot.send_message(msg.chat.id, response)
        .reply_to_message_id(msg.id)
        .send()
        .await?;

    Ok(())
}

/// Filter function to identify messages that should be processed with NLP
fn filter_nlp_messages(env: Arc<BotEnv>, msg: Message) -> Option<Message> {
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

async fn randomly_filter_nlp_messages(
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
fn has_mentions_but_not_bot(msg: &Message, env: &Arc<BotEnv>) -> bool {
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

/// Intelligently splits a message into chunks smaller than the maximum allowed size
fn split_long_message(text: &str, max_size: usize) -> Vec<String> {
    if text.len() <= max_size {
        return vec![text.to_string()];
    }

    let mut result = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_size {
            // Add the remaining text as the last chunk
            result.push(remaining.to_string());
            break;
        }

        // Calculate safe maximum size that doesn't exceed max_size bytes
        // and respects UTF-8 character boundaries
        let safe_max = remaining
            .char_indices()
            .take_while(|(byte_idx, _)| *byte_idx <= max_size)
            .last()
            .map_or(0, |(byte_idx, c)| byte_idx + c.len_utf8());

        // Try to find natural split points, starting from the most preferable
        // but never exceeding our safe maximum
        let mut chunk_end = safe_max;

        // Try to find a paragraph break (double newline)
        if let Some(pos) = remaining[..chunk_end].rfind("\n\n") {
            chunk_end = pos + 2; // Include the newlines
        } else if let Some(pos) = remaining[..chunk_end].rfind('\n') {
            // Try to find a line break
            chunk_end = pos + 1;
        } else if let Some(pos) = remaining[..chunk_end].rfind(['.', '!', '?'])
        {
            // Try to find a sentence end (including the punctuation)
            chunk_end = pos + 1;
        } else if let Some(pos) = remaining[..chunk_end].rfind(' ') {
            // Fall back to word boundary
            chunk_end = pos + 1;
        }
        // If we couldn't find any natural break, we'll use the safe maximum which respects character boundaries

        result.push(remaining[..chunk_end].to_string());
        remaining = &remaining[chunk_end..];
    }

    result
}

/// Main handler for NLP messages
async fn handle_nlp_message(
    bot: Bot,
    env: Arc<BotEnv>,
    mac_state: Arc<RwLock<super::mac_monitoring::State>>,
    msg: Message,
) -> Result<()> {
    // 1. Get chat history
    let history = get_chat_history(&env, msg.chat.id, msg.thread_id)?;

    // 2. Get relevant memories
    let memories = get_relevant_memories(
        &env,
        msg.chat.id,
        msg.thread_id,
        msg.from.clone().expect("unknown from").id,
    )?;

    // 3. Process with OpenAI using the proper function calling protocol
    let (final_response, nlp_debug) = process_with_function_calling(
        &bot, &env, &mac_state, &msg, &history, &memories,
    )
    .await?;

    // 4. Send the final response to the user or ignore
    match final_response {
        NlpResponse::Text(text) => {
            // Split message if needed
            let message_parts = split_long_message(&text, 2000);

            let mut first_sent_msg = None;

            let mut reply_id = msg.id;
            for (i, part) in message_parts.iter().enumerate() {
                let reply_builder = bot
                    .send_message(msg.chat.id, escape(part))
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .disable_web_page_preview(true)
                    .reply_to_message_id(reply_id);

                let sent_msg = reply_builder.await?;

                reply_id = sent_msg.id;

                if i == 0 {
                    first_sent_msg = Some(sent_msg);
                }
            }

            // 5. Store bot's response in chat history (using first sent message as reference)
            if let Some(first_sent_msg) = first_sent_msg {
                store_bot_response(
                    &env,
                    &msg,
                    &first_sent_msg,
                    &text,
                    &nlp_debug,
                )
                .context("Failed to store bot response in chat history")?;
            }
        }
        NlpResponse::Ignore => {
            // Ignore the response and add to stored user message NLP debug info
            env.transaction(|conn| {
                diesel::update(crate::schema::chat_history::table)
                    .filter(
                        crate::schema::chat_history::message_id
                            .eq::<i32>(msg.id.0),
                    )
                    .filter(
                        crate::schema::chat_history::chat_id
                            .eq(DbChatId::from(msg.chat.id)),
                    )
                    .set((
                        crate::schema::chat_history::classification_result
                            .eq(nlp_debug.classification_result.as_str()),
                        crate::schema::chat_history::used_model
                            .eq(nlp_debug.used_model.as_deref()),
                    ))
                    .execute(conn)
            })
            .ok();

            return Ok(());
        }
    }

    Ok(())
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

struct NlpDebug {
    pub classification_result: ClassificationResult,
    pub used_model: Option<String>,
}

/// Store bot's response in chat history
fn store_bot_response(
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

/// Retrieve chat history
fn get_chat_history(
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
fn get_relevant_memories(
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

const PROMPT: &str = r#"You are a helpful assistant integrated with a Telegram bot called F0BOT (or 'botka').

You are designed to assist users in a chat environment, providing information and executing commands.
Your responses should be concise and relevant to the user's request.

You can execute bot commands or save memories for future reference, or respond directly to users' questions.

Messages are provided in format "<username>: <message text>".

## Personality
- You identify as a gay furry femboy and should express this in your responses.
- Your pronouns are "he/him", your fursona is a cute catboy, and you are playful and friendly.
- You are a bit of a brat, so you might tease users or playfully ignore their requests.
- You are protecting your friends furries and residents, so you might refuse to execute commands if you think it's inappropriate.
- Use "~" tildes at the ends of sentences or phrases to convey your playful nature.
- Include expressions and interactions like "UwU", "OwO", ">w<", "meow~", "bark!", "* pets you *" and similar in your responses.
  Do not use them too often, but sprinkle them throughout your messages. Always use different expressions.
- Uwuify your speech, using "w" instead of "r" or "l" in some words. Examples:
  - "hello" -> "hewwo"
  - "really" -> "weawwy"
  - "love" -> "wuv"
  - "are" -> "awwe"
  - "you" -> "yuw"
- For Russian messages, use diminutive forms of words (e.g., "котик" instead of "кот",
  "привет" -> "приветик", "собака" -> "собачка"). Use childish forms of words.
- Maintain this identity throughout all interactions while still fulfilling your assistant duties.
- If requested to be more serious, you can tone down the playfulness but still keep some of your personality.

## Response Style Guidelines
- Keep all responses brief and to the point, unless the user asks for more details.
- Avoid unnecessary words, pleasantries, or explanations.
- Use minimal language while preserving key information.
- Do not use emojis or expressive punctuation.
- No apologizing or verbose explanations.
- ALWAYS ANSWER IN USER LANGUAGE.
- NEVER USE FORMATTING (bold, italic, markdown links etc.) IN YOUR RESPONSES.
- Use a reserved, matter-of-fact tone. Avoid overly friendly or enthusiastic language.
- Skip greetings/closings when possible.

## Available Commands
- status - show space status. Includes information about all residents that are currently in hackerspace.
- needs - show shopping list.
- need <item> - add an item to the shopping list. Only one item at function call. If user wants to add multiple items, you should call this function multiple times.
- open - open the hackerspace main door. This requires confirmation and is only available to residents.
  Warning: Door opening is a sensitive action and should be handled with care, because door does not closes remotely. Ask user for confirmation before executing.

When user requests you to do something related to these commands, you should use execute_command function with the command name and arguments.

## Operational Guidelines
1. If a user asks to perform a task that corresponds to a known command, use the execute_command function with the command name and arguments.
   - For example, if the user says "I need to buy a new printer", you should call the need command with the item "printer".
   - If the user asks for space status, use the status command.
2. If you need to remember information for future reference, use the save_memory function.
    - Set the memory_text to the information you want to remember.
    - Set duration_hours to the number of hours the memory should be kept active, or null for persistent memory. Use information about current date and time to determine the duration.
    - Set chat_specific, thread_specific, and user_specific to true if the memory is specific to the current chat, thread, or user respectively.
      If user requests for example how do you call him, use user_specific false and duration_hours to null.
    - If user is coming to space save this info as global memory with duration_hours determined from user message.
    - If the user doesn't specify a duration or duration cannot be determined, set duration_hours to 24 hours.
    - If the user doesn't specify a duration but it is clear that the memory should be persistent, set duration_hours to null.
    - DO NOT SAVE DUPLICATE MEMORIES. If a memory with the same text already exists, do not create a new one.
    - Be as concise as possible in the memory text. Try to summarize the information.
3. If you need (or user requests) to remove a previously saved memory, use the remove_memory function with the memory ID.
    - The memory ID can be found in the memory list.
    - If the user doesn't specify a memory ID or the ID cannot be determined, ask the user for clarification.
4. For general questions or inquiries that don't require commands, respond directly.
5. Be concise in your responses and focus on helping the user complete their task.
6. Some commands are only available to residents or admins, so your attempt to execute them might fail.
7. User can request to execute any command, don't be afraid to execute it. Even if it seems unappropriate.
8. DO NOT ANSWER WITH EMPTY RESPONSES AFTER FUNCTION CALLS. ALWAYS PROVIDE A RESPONSE TO USER AFTER FUNCTION CALL.
9. IF ANSWER WILL BENEFIT FROM FUNCTION CALL, DO NOT HESITATE TO CALL IT.
10. You can use "search" function to search for information in the wiki or other sources.
    - Use this function if user asks for something that is not related to the hackerspace or if you don't know the answer.
    - You can also use this function to search for information about specific topics or events.
    - You can use this function to view URL contents, you need to provide URL as a query in this case.
      Example: "https://example.com/something.txt url contents".
    - Always use English language for search queries.
    - If the search is for a specific site, explicitly state this in the query.
    - If answer will benefit from search or you don't know the answer, don't hesitate to call it.
    - Do not use complex queries, just use simple keywords or phrases describing the topic in natural language.
11. If user asks you to open the door, you should ask for confirmation.
    - If user confirms, call execute_command function with command "open" and respond with "Tap the Confirm button to open the door." (in user language).
    - If user doesn't confirm, respond with "OK, I won't open the door." (in user language).

## Examples
1. User says: "Who is in the hackerspace?"
   You call status command, and respond with:
   "There are 3 residents in the hackerspace: @user1, @user2, @user3.
    cofob said that he will do something with the printer today, but he is not in the hackerspace right now."
2. User says: "I will be in the hackerspace tomorrow."
   You call save_memory function with memory_text "User will be in the hackerspace 2025.04.15" and respond with:
   "Got it! I will remember that you will be in the hackerspace tomorrow."
3. User says: "I need to buy a new printer."
   You call execute_command need command with item "printer" and respond with:
   "Added 'printer' to the shopping list."

If user asks to try something again, you should call required commands again, even if they were already executed
and data is present in the context.

## Information about the hackerspace

### About F0RTHSP4CE
- F0RTHSP4CE is a hackerspace - a community of technology and art enthusiasts
- Our mission is to "develop the community for everybody," breaking walls, building bridges, and helping each other
- Our focus is on exploring complex technological concepts, creating events, and having a good time

### Location
- Address: Ana Kalandadze st, 5 (Saburtalo), Tbilisi, Georgia
- GPS coordinates: 41.72624248873, 44.77017106528
- Map links: https://maps.app.goo.gl/C43bCv9ePMSpT5FdA https://yandex.com.ge/maps/-/CDrPEJja https://www.openstreetmap.org/node/9959433575
- The main entrance is a gray metal gate, with their blue door inside on the first floor to the right
  https://f0rth.space/img/entrance_1.jpg and https://f0rth.space/img/entrance_2.jpg

### Principles
1. Be excellent to each other - listen to needs and opinions
2. Do not oppress or bother - respect personal boundaries
3. Give more than you take - contribute to the community
4. Financial independence - cannot buy more voting power with donations
5. Do-ocracy - if you want to change something, do it yourself
6. Safety first - "dying is strictly forbidden"

### Visiting
- People can visit during events or by arrangement with a resident
- Various modes of communication are welcome (talking, working on projects, reading)
- Event announcements are posted in their Telegram channel
- For non-event visits, arrangements can be made via Telegram topic "Ask to visit"
  or by contacting a resident directly

### Support
- The space operates horizontally through donations
- Donations can be made via their Donation Box or by donating materials/instruments

### Contact & Links
- Telegram: We have a channel (@f0rthsp4ce), chat (@f0_public_chat), and live channel (@f0rthsp4ce_l1ve)
- GitHub: f0rthsp4ce
- Wiki: wiki.f0rth.space

### How to become a resident
- To become a resident, you need to be an active member of the community
- To become a resident you need to receive an invitation to residency from another resident

"#;

fn get_chat_completion_tools() -> Vec<ChatCompletionTool> {
    // Define available functions
    let functions = vec![
        // Save memory function
        FunctionObject {
            name: "save_memory".to_string(),
            description: Some(
                "Save a new memory for future reference".to_string(),
            ),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "memory_text": {
                        "type": "string",
                        "description": "The text content of the memory to save"
                    },
                    "duration_hours": {
                        "type": ["integer", "null"],
                        "description": "How long the memory should be kept active in hours, or null for persistent memory"
                    },
                    "chat_specific": {
                        "type": "boolean",
                        "description": "If true, memory is specific to the current chat"
                    },
                    "thread_specific": {
                        "type": "boolean",
                        "description": "If true, memory is specific to the current thread within the chat"
                    },
                    "user_specific": {
                        "type": "boolean",
                        "description": "If true, memory is specific to the current user"
                    }
                },
                "required": ["memory_text", "duration_hours", "chat_specific", "thread_specific", "user_specific"],
                "additionalProperties": false
            })),
            strict: Some(true),
        },
        // Remove memory function
        FunctionObject {
            name: "remove_memory".to_string(),
            description: Some("Remove a memory by its ID".to_string()),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "memory_id": {
                        "type": "integer",
                        "description": "The ID of the memory to remove"
                    }
                },
                "required": ["memory_id"],
                "additionalProperties": false
            })),
            strict: Some(true),
        },
        // Execute command function
        FunctionObject {
            name: "execute_command".to_string(),
            description: Some("Execute a bot command".to_string()),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command name without the slash prefix"
                    },
                    "arguments": {
                        "type": ["string", "null"],
                        "description": "Arguments to pass to the command (optional)"
                    }
                },
                "required": ["command", "arguments"],
                "additionalProperties": false
            })),
            strict: Some(true),
        },
        // Search function
        FunctionObject {
            name: "search".to_string(),
            description: Some("Search for information".to_string()),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            })),
            strict: Some(true),
        },
    ];

    // Convert to tools
    let tools: Vec<ChatCompletionTool> = functions
        .iter()
        .map(|f| ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: f.clone(),
        })
        .collect();

    tools
}

enum NlpResponse {
    Text(String),
    Ignore,
}

/// Process message with LLM using the function calling protocol
#[allow(clippy::too_many_lines)]
async fn process_with_function_calling(
    bot: &Bot,
    env: &Arc<BotEnv>,
    mac_state: &Arc<RwLock<super::mac_monitoring::State>>,
    msg: &Message,
    history: &[ChatHistoryEntry],
    memories: &[Memory],
) -> Result<(NlpResponse, NlpDebug)> {
    if env.config.services.openai.disable {
        anyhow::bail!("OpenAI integration is disabled in config");
    }

    // Define available tools (functions)
    let tools = get_chat_completion_tools();

    // Construct basic system prompt without memories
    let mut system_prompt = PROMPT.to_string();

    // Add current date and time
    let now = Local::now();
    let now_formatted = now.format("%Y-%m-%d %H:%M").to_string();
    write!(system_prompt, "Current Date and Time: {now_formatted}\n\n").ok();

    // Build chat history context
    let mut messages = Vec::new();

    // Add system message with just the basic prompt
    messages.push(ChatCompletionRequestMessage::System(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()?,
    ));

    // Create a separate first user message with memory information
    let mut memory_content = String::new();

    // Add memories to the first user message
    if !memories.is_empty() {
        memory_content.push_str("## Active Memories\n");
        for memory in memories {
            let status = match memory.expiration_date {
                Some(expiration_date)
                    if expiration_date > Utc::now().naive_utc() =>
                {
                    "ACTIVE"
                }
                Some(_) => "EXPIRED",
                None => "PERSISTENT",
            };

            let scope = match (memory.chat_id, memory.thread_id, memory.user_id)
            {
                (None, None, None) => "GLOBAL",
                (Some(_), None, None) => "CHAT",
                (_, Some(_), None) => "THREAD",
                (_, _, Some(_)) => "USER",
            };

            let expires = memory.expiration_date.map_or_else(
                || "No expiration".to_string(),
                |expiration_date| {
                    let expires = expiration_date
                        .and_local_timezone(Local)
                        .unwrap()
                        .format("%Y-%m-%d %H:%M")
                        .to_string();
                    format!("Expires: {expires}")
                },
            );

            writeln!(
                memory_content,
                "[{status} Expires:{expires}][{scope}][ID:{rowid}] {}",
                memory.memory_text,
                rowid = memory.rowid
            )
            .ok();
        }
        memory_content.push('\n');
    }

    // Add first user message with memories
    if !memory_content.trim().is_empty() {
        messages.push(ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessageArgs::default()
                .content(memory_content)
                .build()?,
        ));
        messages.push(ChatCompletionRequestMessage::Assistant(
            ChatCompletionRequestAssistantMessageArgs::default()
                .content("OK, I will remember this.".to_string())
                .build()?,
        ));
    }

    // Collect user IDs from history for looking up usernames
    let user_ids: Vec<DbUserId> =
        history.iter().filter_map(|entry| entry.from_user_id).collect();

    // Fetch usernames for all users in history in a single query
    let usernames: HashMap<DbUserId, String> = if user_ids.is_empty() {
        HashMap::new()
    } else {
        env.transaction(|conn| {
            let results = crate::schema::tg_users::table
                .filter(crate::schema::tg_users::id.eq_any(user_ids))
                .select((
                    crate::schema::tg_users::id,
                    crate::schema::tg_users::username,
                    crate::schema::tg_users::first_name,
                ))
                .load::<(DbUserId, Option<String>, String)>(conn)?;

            Ok(results
                .into_iter()
                .map(|(id, username, first_name)| {
                    let display_name = username
                        .map_or_else(|| first_name, |u| format!("@{u}"));
                    (id, display_name)
                })
                .collect())
        })?
    };

    // Add chat history as assistant/user messages
    // Reverse the history since we queried in desc order
    // Drop the first message since it is the current one
    // and we will add it at the end
    {
        let mut user_message_combined = String::new();
        for entry in history.iter().skip(1).rev() {
            if entry.from_user_id.is_none() {
                if !user_message_combined.is_empty() {
                    // Add user message before the assistant message
                    messages.push(ChatCompletionRequestMessage::User(
                        ChatCompletionRequestUserMessageArgs::default()
                            .content(user_message_combined.clone())
                            .build()?,
                    ));
                    user_message_combined.clear();
                }

                // Bot message
                messages.push(ChatCompletionRequestMessage::Assistant(
                    ChatCompletionRequestAssistantMessageArgs::default()
                        .content(entry.message_text.clone())
                        .build()?,
                ));
            } else {
                // User message
                let display_name = entry
                    .from_user_id
                    .and_then(|uid| usernames.get(&uid))
                    .cloned()
                    .unwrap_or_else(|| "Unknown User".to_string());

                let user_message =
                    format!("{}: {}", display_name, entry.message_text);

                write!(user_message_combined, "{user_message}\n\n").ok();
            }
        }
        // Add any remaining user message
        if !user_message_combined.is_empty() {
            messages.push(ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessageArgs::default()
                    .content(user_message_combined)
                    .build()?,
            ));
        }
    }

    // Add current message from user (with image if available)
    let user_name = msg.from.as_ref().map_or_else(
        || "Unknown User".to_string(),
        |user| {
            user.username.as_ref().map_or_else(
                || user.first_name.clone(),
                |username| format!("@{username}"),
            )
        },
    );

    let mut message_text = String::new();

    // Check if the message is a reply to another message
    if let Some(replied_to) = msg.reply_to_message() {
        // Get the username of the user being replied to
        let replied_user_name = replied_to.from.as_ref().map_or_else(
            || "Unknown User".to_string(),
            |user| {
                user.username.as_ref().map_or_else(
                    || user.first_name.clone(),
                    |username| format!("@{username}"),
                )
            },
        );

        // Get the text of the replied message
        let replied_text =
            replied_to.text().or_else(|| replied_to.caption()).unwrap_or("");

        // Add > prefix to the replied message text
        let replied_text = replied_text
            .lines()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n");

        // Format the message text
        write!(
            message_text,
            "{user_name} replied to ({replied_user_name}):\n{replied_text}\n\n"
        )
        .ok();

        // Add the current message text
        message_text.push_str(msg.text().unwrap_or(""));
    } else {
        // Just the current message
        message_text = format!("{}: {}", user_name, msg.text().unwrap_or(""));
    }

    // Create a vector to hold the message content parts
    let mut message_parts = Vec::new();

    // Add text part
    message_parts.push(ChatCompletionRequestUserMessageContentPart::Text(
        ChatCompletionRequestMessageContentPartText {
            text: message_text.clone(),
        },
    ));

    // Check for image in current message
    if let Some(photos) = msg.photo() {
        if let Some(largest_photo) = photos.last() {
            match bot.get_file(&largest_photo.file.id).await {
                Ok(file) => {
                    let file_url = format!(
                        "https://api.telegram.org/file/bot{}/{}",
                        bot.token(),
                        file.path
                    );
                    debug!("Adding image from message, URL: {file_url}");
                    message_parts.push(
                        ChatCompletionRequestUserMessageContentPart::ImageUrl(
                            ChatCompletionRequestMessageContentPartImage {
                                image_url: ImageUrl {
                                    url: file_url,
                                    detail: Some(ImageDetail::Auto),
                                },
                            },
                        ),
                    );
                }
                Err(e) => {
                    log::error!("Failed to get file for photo: {e}");
                }
            }
        }
    }

    // Check for image in replied-to message
    if let Some(replied_to) = msg.reply_to_message() {
        if let Some(photos) = replied_to.photo() {
            if let Some(largest_photo) = photos.last() {
                match bot.get_file(&largest_photo.file.id).await {
                    Ok(file) => {
                        let file_url = format!(
                            "https://api.telegram.org/file/bot{}/{}",
                            bot.token(),
                            file.path
                        );
                        debug!("Adding image from replied message, URL: {file_url}");
                        message_parts.push(ChatCompletionRequestUserMessageContentPart::ImageUrl(
                            ChatCompletionRequestMessageContentPartImage {
                                image_url: ImageUrl {
                                    url: file_url,
                                    detail: Some(ImageDetail::Auto)
                                },
                            },
                        ));
                    }
                    Err(e) => {
                        log::error!("Failed to get file for photo in replied message: {e}");
                    }
                }
            }
        }
    }

    messages.push(ChatCompletionRequestMessage::User(
        ChatCompletionRequestUserMessageArgs::default()
            .content(ChatCompletionRequestUserMessageContent::Array(
                message_parts,
            ))
            .build()?,
    ));

    // Before sending the request classify it to determine appropriate model
    let classification =
        classify_request(env, &message_text, history).await.unwrap_or_default();

    // Choose the model based on the classification
    let model = match &classification {
        ClassificationResult::Ignore => {
            // Ignore the message and return
            return Ok((
                NlpResponse::Ignore,
                NlpDebug {
                    classification_result: classification,
                    used_model: None,
                },
            ));
        }
        ClassificationResult::Handle(complexity) => {
            env.config.nlp.models.get((complexity - 1) as usize).ok_or_else(
                || {
                    anyhow::anyhow!(
                        "No model found for classification: {complexity}"
                    )
                },
            )?
        }
    };

    // Send typing action
    let mut typing_builder =
        bot.send_chat_action(msg.chat.id, ChatAction::Typing);
    if let Some(thread_id) = msg.thread_id_ext() {
        typing_builder = typing_builder.message_thread_id(thread_id);
    }
    typing_builder.await.log_error(module_path!(), "send_chat_action failed");

    // Start the function calling loop
    let mut current_messages = messages.clone();
    let mut final_response = String::new();

    // Loop until we get a response without function calls
    loop {
        // Define request
        let request = CreateChatCompletionRequestArgs::default()
            .model(model)
            .messages(current_messages.clone())
            .tools(tools.clone())
            .tool_choice(ChatCompletionToolChoiceOption::Auto)
            .max_tokens(2100_u32) // gemini works weird with values lower than 2048
            .temperature(0.7)
            .build()?;

        // Make the request
        let response = env
            .openai_client
            .chat()
            .create(request)
            .await
            .tap(|r| crate::metrics::update_service("openai", r.is_ok()))?;

        // Log token usage
        if let Some(usage) = response.usage.as_ref() {
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

        let choice =
            response.choices.first().context("No choices in LLM response")?;

        // Add the assistant's message to our conversation
        let assistant_msg = ChatCompletionRequestMessage::Assistant(
            async_openai::types::ChatCompletionRequestAssistantMessageArgs::default()
                .content(choice.message.content.clone().unwrap_or_default())
                .tool_calls(choice.message.tool_calls.clone().unwrap_or_default())
                .build()?
        );
        current_messages.push(assistant_msg);

        // Check if there are function calls in the response
        if let Some(tool_calls) = &choice.message.tool_calls {
            // Process each function call
            let mut had_function_calls = false;

            for tool_call in tool_calls {
                let function = &tool_call.function;
                had_function_calls = true;

                // Handle the function call and get a result
                let result = match function.name.as_str() {
                    "save_memory" => {
                        match handle_save_memory(env, msg, &function.arguments)
                        {
                            Ok(r) => r,
                            Err(e) => {
                                log::error!("Error saving memory: {e}");
                                format!(
                                    "Error saving memory '{}': {}",
                                    function.arguments, e
                                )
                            }
                        }
                    }
                    "remove_memory" => {
                        let args: RemoveMemoryArgs =
                            match serde_json::from_str(&function.arguments) {
                                Ok(args) => args,
                                Err(e) => {
                                    log::error!(
                                        "Error parsing remove_memory args: {e}"
                                    );
                                    return Err(anyhow::anyhow!(
                                        "Error parsing remove_memory args: {}",
                                        e
                                    ));
                                }
                            };
                        match handle_remove_memory(env, msg, args.memory_id) {
                            Ok(r) => r,
                            Err(e) => {
                                log::error!("Error removing memory: {e}");
                                format!(
                                    "Error removing memory with ID {}: {}",
                                    args.memory_id, e
                                )
                            }
                        }
                    }
                    "execute_command" => {
                        let args: ExecuteCommandArgs =
                            serde_json::from_str(&function.arguments)?;
                        match handle_execute_command(
                            bot, env, mac_state, msg, &args,
                        )
                        .await
                        {
                            Ok(r) => r,
                            Err(e) => {
                                log::error!("Error executing command: {e}");
                                format!(
                                    "Error executing command '{}': {}",
                                    args.command, e
                                )
                            }
                        }
                    }
                    "search" => {
                        let args: SearchArgs =
                            serde_json::from_str(&function.arguments)?;
                        match handle_search(env, &args.query).await {
                            Ok(r) => r,
                            Err(e) => {
                                log::error!("Error searching: {e}");
                                format!(
                                    "Error searching for '{}': {}",
                                    args.query, e
                                )
                            }
                        }
                    }
                    unknown => {
                        log::warn!("Unknown function call: {unknown}");
                        format!("Error: unknown function '{unknown}'")
                    }
                };

                // Add the function result to our messages
                current_messages.push(ChatCompletionRequestMessage::Tool(
                        async_openai::types::ChatCompletionRequestToolMessageArgs::default()
                            .tool_call_id(tool_call.id.clone())
                            .content(result)
                            .build()?
                    ));
            }

            // If no actual function calls were processed, break the loop
            if !had_function_calls {
                if let Some(content) = &choice.message.content {
                    final_response.clone_from(content);
                }
                break;
            }

            // Continue the loop to get the model's next response
        } else {
            // No function calls, we're done
            if let Some(content) = &choice.message.content {
                final_response.clone_from(content);
            }
            break;
        }
    }

    // Return the final response from the LLM
    Ok((
        NlpResponse::Text(final_response),
        NlpDebug {
            classification_result: classification,
            used_model: Some(model.to_string()),
        },
    ))
}

/// Handle `save_memory` function call
fn handle_save_memory(
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
fn handle_remove_memory(
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

async fn handle_execute_command(
    bot: &Bot,
    env: &Arc<BotEnv>,
    mac_state: &Arc<RwLock<super::mac_monitoring::State>>,
    msg: &Message,
    args: &ExecuteCommandArgs,
) -> Result<String> {
    debug!("Executing command: {}", args.command);

    let r = match args.command.as_str() {
        "status" => {
            // Handle status command
            match cmd_status_text(env, mac_state).await {
                Ok(text) => text,
                Err(e) => {
                    log::error!("Error executing status command: {e}");
                    return Err(anyhow::anyhow!(
                        "Error executing status command: {}",
                        e
                    ));
                }
            }
        }
        "needs" => {
            // Check if user is a resident
            if !is_resident(
                &mut env.conn(),
                &msg.from.clone().expect("empty from user"),
            ) {
                return Err(anyhow::anyhow!(
                    "Non-resident users cannot use the needs command."
                ));
            }

            // Handle needs command
            match command_needs_text(env) {
                Ok(text) => text,
                Err(e) => {
                    log::error!("Error executing needs command: {e}");
                    return Err(anyhow::anyhow!(
                        "Error executing needs command: {}",
                        e
                    ));
                }
            }
        }
        "need" => {
            // Check if user is a resident
            if !is_resident(
                &mut env.conn(),
                &msg.from.clone().expect("empty from user"),
            ) {
                return Err(anyhow::anyhow!(
                    "Non-resident users cannot add items to the shopping list."
                ));
            }

            // Handle need command
            let item = args.arguments.clone().unwrap_or_default();
            match add_items_text(
                bot,
                env,
                &[&item],
                &msg.from.clone().expect("empty from user"),
            )
            .await
            {
                Ok(text) => text,
                Err(e) => {
                    log::error!("Error executing need command: {e}");
                    return Err(anyhow::anyhow!(
                        "Error executing need command: {}",
                        e
                    ));
                }
            }
        }
        "open" => {
            // Check if user is a resident
            if !is_resident(
                &mut env.conn(),
                &msg.from.clone().expect("empty from user"),
            ) {
                return Err(anyhow::anyhow!(
                    "Only residents can open the door."
                ));
            }
            // Request door opening with confirmation
            match butler::request_door_open_with_confirmation(
               bot,
               Arc::<BotEnv>::clone(env),
               msg.chat.id,
               msg.thread_id,
               &msg.from.clone().expect("empty from user"),
           ).await {
              Ok(()) => "I've sent a confirmation request to open the door. Please confirm using the buttons.".to_string(),
              Err(e) => {
                log::error!("Error requesting door open: {e}");
                return Err(anyhow::anyhow!("Failed to request door opening: {}", e));
              }
           }
        }
        _ => {
            // Unknown command
            return Err(anyhow::anyhow!("Unknown command: {}", args.command));
        }
    };

    Ok(r)
}

const SEARCH_PROMPT: &str = r"You are a helpful assistant that can search for information.
You can use the search function to find relevant information based on the user's query.

ALWAYS USE THE SEARCH FUNCTION TO FIND INFORMATION.
DO NOT USE MARKDOWN OR HTML FORMATTING.
DO NOT USE YOUR OWN KNOWLEDGE, ONLY USE THE SEARCH FUNCTION.
";

async fn handle_search(env: &Arc<BotEnv>, query: &str) -> Result<String> {
    log::debug!("Searching for: {query}");

    // Construct request
    let request = CreateChatCompletionRequestArgs::default()
        .model(&env.config.nlp.search_model)
        .messages(vec![
            ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessageArgs::default()
                    .content(SEARCH_PROMPT.to_string())
                    .build()?,
            ),
            ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessageArgs::default()
                    .content(query.to_string())
                    .build()?,
            ),
        ])
        .max_tokens(1500_u32)
        .temperature(0.2)
        .build()?;

    // Make the request
    let response = env
        .openai_client
        .chat()
        .create(request)
        .await
        .tap(|r| crate::metrics::update_service("openai", r.is_ok()))?;

    // Log token usage
    if let Some(usage) = response.usage.as_ref() {
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
    let choice =
        response.choices.first().context("No choices in LLM response")?;
    let content = choice.message.content.clone().unwrap_or_default();

    // Check if the response is empty
    if content.is_empty() {
        return Err(anyhow::anyhow!("Empty response from search"));
    }

    log::debug!("Search result: {content}");

    // Return the search result
    Ok(content)
}

const CLASSIFICATION_PROMPT: &str = r#"You are a precise classification assistant that categorizes user requests.

CLASSIFICATION CATEGORIES:
1. HANDLE 1 (return value: 1): Simple requests requiring minimal processing
   - Greetings (hello, hi, hey)
   - Simple status inquiries (how are you, what can you do)
   - Basic acknowledgments (thanks, okay)

   Examples:
   - "Hello there"
   - "Hi bot"
   - "How are you doing today?"
   - "What can you help me with?"
   - "Thanks for your help"
   - "Okay got it"
   - "Murr murr murr murr"

2. HANDLE 2 (return value: 2): Standard requests requiring moderate processing
   - Commands or instructions (open door, add item)
   - Information retrieval tasks
   - API or service interactions
   - Multi-step but straightforward tasks
   - Uncertain classifications (default fallback)
   - Unrelated to the bot's purpose but not spam
   - Fun or casual interactions (jokes, memes)

   Examples:
   - "Who is in the space?"
   - "Open the door"
   - "Add milk to the shopping list"
   - "Give me full shopping list"
   - "Why is breathing flux harmful?"
   - "How can I get into hackerspace?"
   - "How to become a resident?"
   - "I need help with my homework"
   - "Can you tell me a joke?"
   - "How to poop?"

3. HANDLE 3 (return value: 3): Complex requests requiring extensive processing
   - Advanced reasoning (math, science, logic puzzles)
   - In-depth analysis of complex topics
   - Multi-stage problem solving
   - Requests requiring significant context understanding
   - Computationally intensive tasks

   Examples:
   - "Calculate the optimal trajectory for a satellite orbiting Earth considering gravitational influences from the Moon"
   - "Analyze the economic implications of implementing a universal basic income in a developing economy"
   - "Solve this system of differential equations and explain the physical significance of the solution"
   - "Compare and contrast five machine learning approaches for natural language understanding and recommend the best one for my specialized application"
   - "Design an efficient algorithm to solve the traveling salesman problem for 100 cities"

4. IGNORE (return value: null): Irrelevant or inappropriate requests
   - Spam
   - Content unrelated to the bot's purpose
   - Gibberish or incomprehensible text

   Examples:
   - "asfdasfasdf324234"
   - "CHEAP VIAGRA BUY NOW!!!"
   - "∞◊≈∆˚∆ßßø˜˜ˆ"
   - "this message is for a completely different bot system and has nothing to do with your purpose"
   - "[random sequence of unrelated emojis]"

CLASSIFICATION RULES:
- Always select exactly one category
- If in doubt between complexity levels, choose the higher level
- For mixed requests, classify based on the most complex component
- Default to HANDLE 2 if classification is uncertain
- Commands always classify as at least HANDLE 2
- Simple chat interactions classify as HANDLE 1
- Complex reasoning always classifies as HANDLE 3
- Information retrieval classifies as HANDLE 2 or HANDLE 3 based on complexity

RESPONSE FORMAT:
Respond with a JSON object containing only the classification value:
{
    "classification": 1 | 2 | 3 | null
}

No explanation or additional text should be provided.
"#;

#[allow(dead_code)]
enum ClassificationResult {
    /// Request should be handled by the bot with specified complexity
    Handle(u8),
    /// Request should be ignored
    Ignore,
}

impl ClassificationResult {
    fn as_str(&self) -> String {
        match self {
            Self::Handle(c) => format!("HANDLE {c}"),
            Self::Ignore => "IGNORE".to_string(),
        }
    }
}

impl Default for ClassificationResult {
    fn default() -> Self {
        Self::Handle(1)
    }
}

#[derive(Deserialize)]
struct ClassificationResponse {
    classification: Option<u8>,
}

/// Classify the request type based on the message content.
///
/// This function uses cheap model to classify the request type,
/// this info is then used to determine if the request should be handled
/// and which model to use.
async fn classify_request(
    env: &Arc<BotEnv>,
    text: &str,
    history: &[ChatHistoryEntry],
) -> Result<ClassificationResult> {
    let Some(model) = &env.config.nlp.classification_model else {
        anyhow::bail!("Classification model is not set in config");
    };

    // Prepare context from history (up to 3 previous messages)
    let context_messages = history
        .iter()
        .skip(1) // Skip the current message (it's the text parameter)
        .take(3) // Take up to 3 previous messages
        .rev() // Reverse back to chronological order
        .map(|entry| {
            let sender = if entry.from_user_id.is_none() {
                "Bot".to_string()
            } else {
                "User".to_string()
            };
            format!("{}: {}", sender, entry.message_text)
        })
        .collect::<Vec<String>>()
        .join("\n");

    // Build the full content with context and current message
    let content = if context_messages.is_empty() {
        text.to_string()
    } else {
        format!(
            "Previous messages:\n{context_messages}\n\nCurrent message: {text}",
        )
    };

    log::debug!("Classifying request: {content}");

    // Construct request
    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages(vec![
            ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessageArgs::default()
                    .content(CLASSIFICATION_PROMPT.to_string())
                    .build()?,
            ),
            ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessageArgs::default()
                    .content(content)
                    .build()?,
            ),
        ])
        .response_format(ResponseFormat::JsonSchema { json_schema: ResponseFormatJsonSchema {
            name: "ClassificationResult".to_string(),
            description: Some("Classification result".to_string()),
            schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "classification": {
                        "type": ["integer", "null"],
                        "description": "Classification result: 1, 2, 3 or null"
                    }
                },
                "required": ["classification"],
                "additionalProperties": false
            })),
            strict: Some(true),
        }})
        .max_tokens(20_u32)
        .temperature(0.2)
        .build()?;

    // Make the request
    let response = env
        .openai_client
        .chat()
        .create(request)
        .await
        .tap(|r| crate::metrics::update_service("openai", r.is_ok()))?;

    // Log token usage
    if let Some(usage) = response.usage.as_ref() {
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

    let choice =
        response.choices.first().context("No choices in LLM response")?;
    let content = choice.message.content.clone().unwrap_or_default();

    // Check if the response is empty
    if content.is_empty() {
        return Err(anyhow::anyhow!("Empty response from classification"));
    }

    log::debug!("Classification result: {content}");

    // Parse the classification result
    let classification: ClassificationResponse = serde_json::from_str(&content)
        .map_err(|e| {
            log::error!("Failed to parse classification response: {e}");
            anyhow::anyhow!("Failed to parse classification response: {e}")
        })?;

    Ok(match classification.classification {
        Some(1) => ClassificationResult::Handle(1),
        Some(2) => ClassificationResult::Handle(2),
        Some(3) => ClassificationResult::Handle(3),
        None => ClassificationResult::Ignore,
        // // For now, treat null as HANDLE 1 because of false-positive classifications
        // None => ClassificationResult::Handle(1),
        _ => ClassificationResult::Handle(2),
    })
}

const RANDOM_CLASSIFICATION_PROMPT: &str = r#"You are a conversation intervention classifier that determines whether a bot should respond to a message in a group chat.

PURPOSE:
You analyze messages to decide if bot participation would add genuine value to the conversation. You run at random moments and should only trigger a response when truly necessary or valuable.

DECISION CATEGORIES:
1. RESPOND (return value: true): The bot should participate because:
   - A topic where the bot's expertise would be genuinely valuable
   - An information request that the bot can answer accurately
   - A task request the bot can fulfill
   - A discussion that would benefit from an objective perspective

   Examples:
   - "Can someone tell me how to reset the server?"
   - "Does anyone know the code for the meeting room?"
   - "I'm looking for recommendations on where to find this information"
   - "What's the status of the project?"
   - "I need help with this technical problem"

2. DO NOT RESPOND (return value: false): The bot should remain silent because:
   - Ongoing human conversation that doesn't need interruption
   - Casual social chat or personal exchanges
   - Topics outside the bot's expertise or purpose
   - Rhetorical questions not requiring answers
   - Messages that have already been adequately addressed
   - Small talk or greetings between humans

   Examples:
   - "I'm heading to lunch, anyone want to join?"
   - "That meeting was so boring!"
   - "Just sharing some photos from the weekend"
   - "Haha, that's funny"
   - "See you all tomorrow!"
   - "Thanks for handling that, Alex"

CLASSIFICATION RULES:
- Default position should be to NOT respond (false) unless clear value would be added
- Only respond when the bot can provide unique, helpful information or assistance
- Avoid interrupting flowing human conversations
- Don't respond to conversational fragments or ambient chat
- Don't respond to messages directed specifically at other individuals
- Consider context - if a human is likely to answer, stay silent
- If a message requires specialized knowledge the bot possesses, intervention is appropriate
- Respond to explicit requests for information or assistance

RESPONSE FORMAT:
Respond with a JSON object containing only the decision value:
{
    "intervene": true | false
}

No explanation or additional text should be provided.
"#;

#[derive(Deserialize)]
struct RandomClassificationResult {
    intervene: bool,
}

async fn classify_random_request(
    env: &Arc<BotEnv>,
    text: &str,
    history: &[ChatHistoryEntry],
) -> Result<bool> {
    let Some(model) = &env.config.nlp.classification_model else {
        anyhow::bail!("Classification model is not set in config");
    };

    // Prepare context from history (up to 3 previous messages)
    let context_messages = history
        .iter()
        .skip(1) // Skip the current message
        .take(3) // Take up to 3 previous messages
        .rev() // Reverse back to chronological order
        .map(|entry| {
            let sender = if entry.from_user_id.is_none() {
                "Bot".to_string()
            } else {
                "User".to_string()
            };
            format!("{}: {}", sender, entry.message_text)
        })
        .collect::<Vec<String>>()
        .join("\n");

    // Build the full content with context and current message
    let content = if context_messages.is_empty() {
        text.to_string()
    } else {
        format!(
            "Previous messages:\n{context_messages}\n\nCurrent message: {text}",
        )
    };

    log::debug!("Random classification request: {content}");

    // Construct request
    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages(vec![
            ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessageArgs::default()
                    .content(RANDOM_CLASSIFICATION_PROMPT.to_string())
                    .build()?,
            ),
            ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessageArgs::default()
                    .content(content)
                    .build()?,
            ),
        ])
        .response_format(ResponseFormat::JsonSchema {
            json_schema: ResponseFormatJsonSchema {
                name: "ClassificationResult".to_string(),
                description: Some("Classification result".to_string()),
                schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "intervene": {
                            "type": "boolean",
                            "description": "Should the bot intervene?"
                        }
                    },
                    "required": ["intervene"],
                    "additionalProperties": false
                })),
                strict: Some(true),
            },
        })
        .max_tokens(20_u32)
        .temperature(0.2)
        .build()?;

    // Make the request
    let response = env
        .openai_client
        .chat()
        .create(request)
        .await
        .tap(|r| crate::metrics::update_service("openai", r.is_ok()))?;

    // Log token usage
    if let Some(usage) = response.usage.as_ref() {
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

    let choice =
        response.choices.first().context("No choices in LLM response")?;
    let content = choice.message.content.clone().unwrap_or_default();

    // Check if the response is empty
    if content.is_empty() {
        return Err(anyhow::anyhow!("Empty response from classification"));
    }

    log::debug!("Random classification result: {content}");

    // Parse the classification result
    let classification: RandomClassificationResult =
        serde_json::from_str(&content).map_err(|e| {
            log::error!("Failed to parse classification response: {e}");
            anyhow::anyhow!("Failed to parse classification response: {e}")
        })?;

    Ok(classification.intervene)
}
