//! Natural language processing module for understanding and responding to
//! non-command messages using OpenAI API.
//!
//! This module allows interaction with the bot using natural language instead
//! of formal commands, triggered by specific keywords.
//!
//! Known issues:
//! - The bot works weirdly in general topic threads of forum. Idk why.

use std::collections::HashMap;
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
};
use chrono::{Duration, Local, Utc};
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use log::debug;
use serde::{Deserialize, Serialize};
use tap::Tap;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, Message, MessageEntityKind, ThreadId};
use teloxide::utils::html::escape;
use tokio::sync::RwLock;

use crate::common::{is_resident, BotEnv, UpdateHandler};
use crate::db::{DbChatId, DbThreadId, DbUserId};
use crate::models::{ChatHistoryEntry, Memory, NewChatHistoryEntry, NewMemory};
use crate::modules::basic::cmd_status_text;
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

fn default_duration_hours() -> Option<u32> {
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

/// Register metrics for OpenAI API usage
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

/// Filter function to identify messages that should be processed with NLP
fn filter_nlp_messages(env: Arc<BotEnv>, msg: Message) -> Option<Message> {
    // Skip if NLP is disabled
    if !env.config.nlp.enabled {
        return None;
    }

    // Skip messages without text or without caption
    let text = match msg.text().or(msg.caption()) {
        Some(text) => text,
        None => return None,
    };

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
        if replied_msg.from.as_ref().map_or(false, |user| user.is_bot) {
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

/// Checks if the message mentions other users but not the bot
fn has_mentions_but_not_bot(msg: &Message, env: &Arc<BotEnv>) -> bool {
    let msg_entities = msg.entities();
    let entities = match &msg_entities {
        Some(entities) => entities,
        None => return false,
    };

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

/// Main handler for NLP messages
async fn handle_nlp_message(
    bot: Bot,
    env: Arc<BotEnv>,
    mac_state: Arc<RwLock<super::mac_monitoring::State>>,
    msg: Message,
) -> Result<()> {
    // 1. Get chat history
    let history = get_chat_history(&env, msg.chat.id, msg.thread_id).await?;

    // 2. Get relevant memories
    let memories = get_relevant_memories(
        &env,
        msg.chat.id,
        msg.thread_id,
        msg.from.clone().expect("unknown from").id,
    )
    .await?;

    // 3. Process with OpenAI using the proper function calling protocol
    let final_response = process_with_function_calling(
        &bot, &env, &mac_state, &msg, &history, &memories,
    )
    .await?;

    // 4. Send the final response to the user
    let reply_builder = bot
        .send_message(msg.chat.id, &final_response)
        .reply_to_message_id(msg.id)
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true);

    let sent_msg = reply_builder.await?;

    // 5. Store bot's response in chat history
    store_bot_response(&env, &msg, &sent_msg, &final_response).await?;

    Ok(())
}

/// Store a new message in chat history
pub async fn store_message(env: Arc<BotEnv>, msg: Message) -> Result<()> {
    let text = match msg.text().or(msg.caption()) {
        Some(text) => text,
        None => return Ok(()),
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
    let max_history = env.config.nlp.max_history;

    let new_entry = NewChatHistoryEntry {
        chat_id: msg.chat.id.into(),
        thread_id: thread_id.into(),
        message_id: msg.id.into(),
        from_user_id: msg.from.as_ref().map(|u| u.id.into()),
        timestamp: Utc::now().naive_utc(),
        message_text: msg.text().unwrap_or(""),
    };

    env.transaction(|conn| {
        // Insert new message
        diesel::insert_into(crate::schema::chat_history::table)
            .values(&new_entry)
            .execute(conn)?;

        // Prune old messages to maintain limit
        let count: i64 = crate::schema::chat_history::table
            .filter(crate::schema::chat_history::chat_id.eq(new_entry.chat_id))
            .filter(
                crate::schema::chat_history::thread_id.eq(new_entry.thread_id),
            )
            .count()
            .get_result(conn)?;

        if count > max_history as i64 {
            let excess = count - max_history as i64;

            // Get IDs of oldest messages to delete
            let to_delete: Vec<i32> = crate::schema::chat_history::table
                .filter(
                    crate::schema::chat_history::chat_id.eq(new_entry.chat_id),
                )
                .filter(
                    crate::schema::chat_history::thread_id
                        .eq(new_entry.thread_id),
                )
                .order(crate::schema::chat_history::timestamp.asc())
                .limit(excess)
                .select(crate::schema::chat_history::rowid)
                .load(conn)?;

            // Delete oldest messages
            diesel::delete(crate::schema::chat_history::table)
                .filter(crate::schema::chat_history::rowid.eq_any(to_delete))
                .execute(conn)?;
        }

        Ok(())
    })?;

    Ok(())
}

/// Store bot's response in chat history
async fn store_bot_response(
    env: &Arc<BotEnv>,
    original_msg: &Message,
    sent_msg: &Message,
    content: &str,
) -> Result<()> {
    let thread_id = original_msg.thread_id.unwrap_or(GENERAL_THREAD_ID);

    let new_entry = NewChatHistoryEntry {
        chat_id: original_msg.chat.id.into(),
        thread_id: thread_id.into(),
        message_id: sent_msg.id.into(),
        from_user_id: None, // From bot
        timestamp: Utc::now().naive_utc(),
        message_text: content,
    };

    env.transaction(|conn| {
        diesel::insert_into(crate::schema::chat_history::table)
            .values(&new_entry)
            .execute(conn)
    })?;

    Ok(())
}

/// Retrieve chat history
async fn get_chat_history(
    env: &Arc<BotEnv>,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
) -> Result<Vec<ChatHistoryEntry>> {
    let thread_id = thread_id.unwrap_or(GENERAL_THREAD_ID);
    let max_history = env.config.nlp.max_history;

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
            .order(crate::schema::chat_history::timestamp.desc())
            .limit(max_history as i64)
            .load::<ChatHistoryEntry>(conn)
    })?;

    Ok(history)
}

/// Get relevant memories (active and recently expired)
async fn get_relevant_memories(
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

In your response you should use hacker slang and abbreviations.
Response should be as concise and short as possible, but still informative.
Answer in user language.

You can use ONLY this formatting for your responses:
- <b> for bold text
- <i> for italic text
- <code> for code blocks
- <pre language="c++"> for code blocks with syntax highlighting
- <s> for strikethrough text
- <u> for underline text

Do not use any other formatting. Always close tags. Do not nest tags.
Do not add extra attributes to tags.

Use formatting only when it is necessary, do not overuse it.

You can execute bot commands or save memories for future reference, or respond directly to users' questions.

Messages are provided in format "<username>: <message text>".

## Available Commands
- status - show space status. Includes information about all residents that are currently in hackerspace.
- needs - show shopping list.
- need <item> - add an item to the shopping list. Only one item at function call. If user wants to add multiple items, you should call this function multiple times.

## Operational Guidelines
1. If a user asks to perform a task that corresponds to a known command, use the execute_command function with the command name and arguments.
   - For example, if the user says "I need to buy a new printer", you should call the need command with the item "printer".
   - If the user asks for space status, use the status command.
2. If you need to remember information for future reference, use the save_memory function.
    - Set the memory_text to the information you want to remember.
    - Set duration_hours to the number of hours the memory should be kept active, or null for persistent memory. Use information about current date and time to determine the duration.
    - Set chat_specific, thread_specific, and user_specific to true if the memory is specific to the current chat, thread, or user respectively.
      If user requests for example how do you call him, use user_specific false and duration_hours to null.
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
   "Added 'printer' to the shopping list. 🛒"

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

/// Process message with LLM using the function calling protocol
async fn process_with_function_calling(
    bot: &Bot,
    env: &Arc<BotEnv>,
    mac_state: &Arc<RwLock<super::mac_monitoring::State>>,
    msg: &Message,
    history: &[ChatHistoryEntry],
    memories: &[Memory],
) -> Result<String> {
    if env.config.services.openai.disable {
        anyhow::bail!("OpenAI integration is disabled in config");
    }

    // Send typing action
    let mut typing_builder =
        bot.send_chat_action(msg.chat.id, ChatAction::Typing);
    if let Some(thread_id) = msg.thread_id_ext() {
        typing_builder = typing_builder.message_thread_id(thread_id);
    }
    typing_builder.await.log_error(module_path!(), "send_chat_action failed");

    // Choose the model from config or default to a reasonable one
    let model = &env.config.nlp.model;

    // Define available tools (functions)
    let tools = get_chat_completion_tools();

    // Construct basic system prompt without memories
    let mut system_prompt = PROMPT.to_string();

    // Add current date and time
    let now = Local::now();
    let now_formatted = now.format("%Y-%m-%d %H:%M").to_string();
    system_prompt
        .push_str(&format!("Current Date and Time: {}\n\n", now_formatted));

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

            let expires = match memory.expiration_date {
                Some(expiration_date) => {
                    let expires = expiration_date
                        .and_local_timezone(Local)
                        .unwrap()
                        .format("%Y-%m-%d %H:%M")
                        .to_string();
                    format!("Expires: {expires}")
                }
                None => "No expiration".to_string(),
            };

            memory_content.push_str(&format!(
                "[{status} Expires:{expires}][{scope}][ID:{rowid}] {}\n",
                memory.memory_text,
                rowid = memory.rowid
            ));
        }
        memory_content.push_str("\n");
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
    let usernames: HashMap<DbUserId, String> = if !user_ids.is_empty() {
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
                        .map(|u| format!("@{}", u))
                        .unwrap_or_else(|| first_name);
                    (id, display_name)
                })
                .collect())
        })?
    } else {
        HashMap::new()
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

                user_message_combined
                    .push_str(&format!("{}\n\n", user_message));
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
    let user_name = match msg.from.as_ref() {
        Some(user) => {
            if let Some(username) = &user.username {
                format!("@{}", username)
            } else {
                user.first_name.clone()
            }
        }
        None => "Unknown User".to_string(),
    };

    let mut message_text = String::new();

    // Check if the message is a reply to another message
    if let Some(replied_to) = msg.reply_to_message() {
        // Get the username of the user being replied to
        let replied_user_name = match replied_to.from.as_ref() {
            Some(user) => {
                if let Some(username) = &user.username {
                    format!("@{}", username)
                } else {
                    user.first_name.clone()
                }
            }
            None => "Unknown User".to_string(),
        };

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
        message_text.push_str(&format!(
            "{} replied to ({}):\n{}\n\n",
            user_name, replied_user_name, replied_text
        ));

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
        ChatCompletionRequestMessageContentPartText { text: message_text },
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
            .max_tokens(500 as u32)
            .temperature(0.6)
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
                            .await
                        {
                            Ok(r) => r,
                            Err(e) => {
                                log::error!("Error saving memory: {}", e);
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
                                        "Error parsing remove_memory args: {}",
                                        e
                                    );
                                    return Err(anyhow::anyhow!(
                                        "Error parsing remove_memory args: {}",
                                        e
                                    ));
                                }
                            };
                        match handle_remove_memory(env, msg, args.memory_id)
                            .await
                        {
                            Ok(r) => r,
                            Err(e) => {
                                log::error!("Error removing memory: {}", e);
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
                            bot, &env, &mac_state, msg, &args,
                        )
                        .await
                        {
                            Ok(r) => r,
                            Err(e) => {
                                log::error!("Error executing command: {}", e);
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
                                log::error!("Error searching: {}", e);
                                format!(
                                    "Error searching for '{}': {}",
                                    args.query, e
                                )
                            }
                        }
                    }
                    unknown => {
                        log::warn!("Unknown function call: {}", unknown);
                        format!("Error: unknown function '{}'", unknown)
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
                    final_response = content.clone();
                }
                break;
            }

            // Continue the loop to get the model's next response
        } else {
            // No function calls, we're done
            if let Some(content) = &choice.message.content {
                final_response = content.clone();
            }
            break;
        }
    }

    // Return the final response from the LLM
    Ok(final_response)
}

/// Handle save_memory function call
async fn handle_save_memory(
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
    let expiration = if let Some(hours) = args.duration_hours {
        let memory_limit = env.config.nlp.memory_limit;
        Some(now + Duration::hours((hours as i64).min(memory_limit)))
    } else {
        None
    };

    let chat_id = if args.chat_specific {
        Some(DbChatId::from(msg.chat.id))
    } else {
        None
    };

    let thread_id = if args.thread_specific && args.chat_specific {
        Some(DbThreadId::from(msg.thread_id.unwrap_or(GENERAL_THREAD_ID)))
    } else {
        None
    };

    let user_id = if args.user_specific {
        Some(DbUserId::from(msg.from.clone().expect("empty from user").id))
    } else {
        None
    };

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

/// Handle remove_memory function call
async fn handle_remove_memory(
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

    log::info!("Removed memory with ID: {}", memory_id);

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
                    log::error!("Error executing status command: {}", e);
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
            match command_needs_text(&env) {
                Ok(text) => text,
                Err(e) => {
                    log::error!("Error executing needs command: {}", e);
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
                &bot,
                &env,
                &[&item],
                &msg.from.clone().expect("empty from user"),
            )
            .await
            {
                Ok(text) => text,
                Err(e) => {
                    log::error!("Error executing need command: {}", e);
                    return Err(anyhow::anyhow!(
                        "Error executing need command: {}",
                        e
                    ));
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

const SEARCH_PROMPT: &str = r#"You are a helpful assistant that can search for information.
You can use the search function to find relevant information based on the user's query.

ALWAYS USE THE SEARCH FUNCTION TO FIND INFORMATION.
DO NOT USE MARKDOWN OR HTML FORMATTING.
DO NOT USE YOUR OWN KNOWLEDGE, ONLY USE THE SEARCH FUNCTION.
"#;

async fn handle_search(
    env: &Arc<BotEnv>,
    query: &str,
) -> Result<String> {
    log::debug!("Searching for: {}", query);

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
        .max_tokens(1500 as u32)
        .temperature(0.6)
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
    let choice = response
        .choices
        .first()
        .context("No choices in LLM response")?;
    let content = choice
        .message
        .content
        .clone()
        .unwrap_or_default();

    // Check if the response is empty
    if content.is_empty() {
        return Err(anyhow::anyhow!("Empty response from search"));
    }

    log::debug!("Search result: {}", content);

    // Return the search result
    Ok(content)
}
