//! Natural language processing module for understanding and responding to
//! non-command messages using OpenAI API.
//!
//! This module allows interaction with the bot using natural language instead
//! of formal commands, triggered by specific keywords.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs,
    ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart, ChatCompletionTool,
    ChatCompletionToolChoiceOption, ChatCompletionToolType,
    CreateChatCompletionRequestArgs, FunctionObject,
};
use chrono::{Duration, Utc};
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use serde::{Deserialize, Serialize};
use tap::Tap;
use teloxide::prelude::*;
use teloxide::types::{Message, MessageEntityKind, ThreadId};

use crate::common::{BotEnv, UpdateHandler};
use crate::db::{DbChatId, DbThreadId, DbUserId};
use crate::models::{ChatHistoryEntry, Memory, NewChatHistoryEntry, NewMemory};
use crate::utils::{BotExt, ResultExt, GENERAL_THREAD_ID};

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
struct ExecuteCommandArgs {
    command: String,
    #[serde(default)]
    arguments: Option<String>,
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

    // Skip messages without text
    let text = msg.text()?;

    // Skip bot commands (those starting with '/')
    if text.starts_with('/') {
        return None;
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
    let text_lower = text.to_lowercase();

    // If no trigger words defined, or message contains a trigger word
    if trigger_words.is_empty()
        || trigger_words
            .iter()
            .any(|word| text_lower.contains(&word.to_lowercase()))
    {
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
    let final_response =
        process_with_function_calling(&bot, &env, &msg, &history, &memories)
            .await?;

    // 4. Send the final response to the user
    // Only use thread_id if the chat supports threads
    let mut reply_builder = bot
        .reply_message(&msg, &final_response)
        .parse_mode(teloxide::types::ParseMode::Html)
        .disable_web_page_preview(true);

    // and the chat is a forum chat, so we don't need to manually set it

    let sent_msg = reply_builder.await?;

    // 5. Store bot's response in chat history
    store_bot_response(&env, &msg, &sent_msg, &final_response).await?;

    Ok(())
}

/// Store a new message in chat history
pub async fn store_message(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
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
    let now = Utc::now().naive_utc();
    let yesterday = (Utc::now() - Duration::days(1)).naive_utc();

    let memories = env.transaction(|conn| {
        // Get active global memories
        let global_active: Vec<Memory> = crate::schema::memories::table
            .filter(crate::schema::memories::chat_id.is_null())
            .filter(crate::schema::memories::expiration_date.gt(now))
            .load(conn)?;

        // Get active chat-specific memories (for this chat)
        let chat_active: Vec<Memory> = crate::schema::memories::table
            .filter(
                crate::schema::memories::chat_id.eq(DbChatId::from(chat_id)),
            )
            .filter(crate::schema::memories::thread_id.is_null())
            .filter(crate::schema::memories::expiration_date.gt(now))
            .load(conn)?;

        // Get active thread-specific memories (for this thread)
        let thread_active: Vec<Memory> = crate::schema::memories::table
            .filter(
                crate::schema::memories::chat_id.eq(DbChatId::from(chat_id)),
            )
            .filter(
                crate::schema::memories::thread_id
                    .eq(DbThreadId::from(thread_id)),
            )
            .filter(crate::schema::memories::expiration_date.gt(now))
            .load(conn)?;

        // Get recently expired global memories
        let global_expired: Vec<Memory> = crate::schema::memories::table
            .filter(crate::schema::memories::chat_id.is_null())
            .filter(crate::schema::memories::expiration_date.le(now))
            .filter(crate::schema::memories::expiration_date.gt(yesterday))
            .load(conn)?;

        // Get recently expired chat-specific memories
        let chat_expired: Vec<Memory> = crate::schema::memories::table
            .filter(
                crate::schema::memories::chat_id.eq(DbChatId::from(chat_id)),
            )
            .filter(crate::schema::memories::thread_id.is_null())
            .filter(crate::schema::memories::expiration_date.le(now))
            .filter(crate::schema::memories::expiration_date.gt(yesterday))
            .load(conn)?;

        // Get recently expired thread-specific memories
        let thread_expired: Vec<Memory> = crate::schema::memories::table
            .filter(
                crate::schema::memories::chat_id.eq(DbChatId::from(chat_id)),
            )
            .filter(
                crate::schema::memories::thread_id
                    .eq(DbThreadId::from(thread_id)),
            )
            .filter(crate::schema::memories::expiration_date.le(now))
            .filter(crate::schema::memories::expiration_date.gt(yesterday))
            .load(conn)?;

        let mut user_active = Vec::new();
        let mut user_expired = Vec::new();

        // Get active user-specific global memories (user_id set, chat_id IS NULL)
        let user_global_active: Vec<Memory> = crate::schema::memories::table
            .filter(
                crate::schema::memories::user_id.eq(DbUserId::from(user_id)),
            ) // Filter by user_id
            .filter(crate::schema::memories::chat_id.is_null()) // Global scope for this user
            .filter(crate::schema::memories::expiration_date.gt(now))
            .load(conn)?;
        user_active.extend(user_global_active);

        // Get active user-specific chat memories (user_id set, chat_id set, thread_id IS NULL)
        let user_chat_active: Vec<Memory> = crate::schema::memories::table
            .filter(
                crate::schema::memories::user_id.eq(DbUserId::from(user_id)),
            ) // Filter by user_id
            .filter(
                crate::schema::memories::chat_id.eq(DbChatId::from(chat_id)),
            ) // Filter by chat_id
            .filter(crate::schema::memories::thread_id.is_null()) // Chat scope for this user (no thread)
            .filter(crate::schema::memories::expiration_date.gt(now))
            .load(conn)?;
        user_active.extend(user_chat_active);

        // Get active user-specific thread memories (user_id set, chat_id set, thread_id set)
        let user_thread_active: Vec<Memory> = crate::schema::memories::table
            .filter(
                crate::schema::memories::user_id.eq(DbUserId::from(user_id)),
            ) // Filter by user_id
            .filter(
                crate::schema::memories::chat_id.eq(DbChatId::from(chat_id)),
            ) // Filter by chat_id
            .filter(
                crate::schema::memories::thread_id
                    .eq(DbThreadId::from(thread_id)),
            ) // Filter by thread_id
            .filter(crate::schema::memories::expiration_date.gt(now))
            .load(conn)?;
        user_active.extend(user_thread_active);

        // Get recently expired user-specific global memories
        let user_global_expired: Vec<Memory> = crate::schema::memories::table
            .filter(
                crate::schema::memories::user_id.eq(DbUserId::from(user_id)),
            )
            .filter(crate::schema::memories::chat_id.is_null())
            .filter(crate::schema::memories::expiration_date.le(now))
            .filter(crate::schema::memories::expiration_date.gt(yesterday))
            .load(conn)?;
        user_expired.extend(user_global_expired);

        // Get recently expired user-specific chat memories
        let user_chat_expired: Vec<Memory> = crate::schema::memories::table
            .filter(
                crate::schema::memories::user_id.eq(DbUserId::from(user_id)),
            )
            .filter(
                crate::schema::memories::chat_id.eq(DbChatId::from(chat_id)),
            )
            .filter(crate::schema::memories::thread_id.is_null())
            .filter(crate::schema::memories::expiration_date.le(now))
            .filter(crate::schema::memories::expiration_date.gt(yesterday))
            .load(conn)?;
        user_expired.extend(user_chat_expired);

        // Get recently expired user-specific thread memories
        let user_thread_expired: Vec<Memory> = crate::schema::memories::table
            .filter(
                crate::schema::memories::user_id.eq(DbUserId::from(user_id)),
            )
            .filter(
                crate::schema::memories::chat_id.eq(DbChatId::from(chat_id)),
            )
            .filter(
                crate::schema::memories::thread_id
                    .eq(DbThreadId::from(thread_id)),
            )
            .filter(crate::schema::memories::expiration_date.le(now))
            .filter(crate::schema::memories::expiration_date.gt(yesterday))
            .load(conn)?;
        user_expired.extend(user_thread_expired);

        // Combine all memories
        let mut all_memories = Vec::new();
        all_memories.extend(global_active);
        all_memories.extend(chat_active);
        all_memories.extend(thread_active);
        all_memories.extend(global_expired);
        all_memories.extend(chat_expired);
        all_memories.extend(thread_expired);

        Ok(all_memories)
    })?;

    Ok(memories)
}

/// Helper function to determine if a chat supports threads
fn chat_supports_threads(chat: &teloxide::types::Chat) -> bool {
    matches!(
        &chat.kind,
        teloxide::types::ChatKind::Public(teloxide::types::ChatPublic {
            kind: teloxide::types::PublicChatKind::Supergroup(
                teloxide::types::PublicChatSupergroup { is_forum: true, .. }
            ),
            ..
        })
    )
}

/// Process message with LLM using the function calling protocol
async fn process_with_function_calling(
    bot: &Bot,
    env: &Arc<BotEnv>,
    msg: &Message,
    history: &[ChatHistoryEntry],
    memories: &[Memory],
) -> Result<String> {
    if env.config.services.openai.disable {
        anyhow::bail!("OpenAI integration is disabled in config");
    }

    // Choose the model from config or default to a reasonable one
    let model = &env.config.nlp.model;

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
                        "type": "integer",
                        "description": "How long the memory should be kept active in hours"
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
    ];

    // Convert to tools
    let tools: Vec<ChatCompletionTool> = functions
        .iter()
        .map(|f| ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: f.clone(),
        })
        .collect();

    // Construct system prompt with memory information
    let mut system_prompt = String::new();
    system_prompt.push_str("You are a helpful assistant integrated with a Telegram bot called F0BOT (or 'botka'). ");
    system_prompt.push_str(
        "In your response you should use hacker slang and abbreviations, response should give cringe vibes.\n\
        Use html telegram formatting for your response.\n\n",
    );
    system_prompt.push_str("You can execute bot commands or save memories for future reference, or respond directly to users' questions.\n\n");

    // Add memories to the system prompt
    if !memories.is_empty() {
        system_prompt.push_str("## Active Memories\n");
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

            system_prompt.push_str(&format!(
                "[{status}][{scope}] {}\n",
                memory.memory_text
            ));
        }
        system_prompt.push_str("\n");
    }

    // Add available commands information (extracted from help command)
    system_prompt.push_str("## Available Commands\n");
    system_prompt.push_str("- help - display command list\n");
    system_prompt.push_str("- residents - list residents\n");
    system_prompt
        .push_str("- residents_admin_table - show residents admin table\n");
    system_prompt.push_str("- residents_timeline - show residents timeline\n");
    system_prompt.push_str("- status - show status\n");
    system_prompt.push_str("- topics - show topic list\n");
    system_prompt.push_str("- version - show bot version\n");
    system_prompt.push_str("- needs - show shopping list\n");
    system_prompt
        .push_str("- need <item> - add an item to the shopping list\n");
    system_prompt
        .push_str("- userctl <args> - control personal configuration\n");
    system_prompt
        .push_str("- add_ssh <key> - add an SSH public key for yourself\n");
    system_prompt.push_str(
        "- get_ssh <username> - get SSH public keys of a user by username\n",
    );
    system_prompt
        .push_str("- ldap_register <email> [username] - Register in LDAP\n");
    system_prompt.push_str("- ldap_reset_password - Reset LDAP password\n");
    system_prompt.push_str("- ldap_update с<args> - Update LDAP settings\n");
    system_prompt.push_str("- racovina - show racovina camera image\n");
    system_prompt.push_str("- hlam - show hlam camera image\n");

    // Add bot instructions
    system_prompt.push_str("\n## Guidelines\n");
    system_prompt.push_str("1. If a user asks to perform a task that corresponds to a known command, use the execute_command function.\n");
    system_prompt.push_str("2. If you need to remember information for future reference, use the save_memory function.\n");
    system_prompt.push_str("3. For general questions or inquiries that don't require commands, respond directly.\n");
    system_prompt.push_str("4. Be concise in your responses and focus on helping the user complete their task.\n");
    system_prompt.push_str("5. Some commands are only available to residents or admins, so your attempt to execute them might fail.\n");

    // Build chat history context
    let mut messages = Vec::new();

    // Add system message
    messages.push(ChatCompletionRequestMessage::System(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()?,
    ));

    // Add chat history as assistant/user messages
    // Reverse the history since we queried in desc order
    for entry in history.iter().rev() {
        if entry.from_user_id.is_none() {
            // Bot message
            messages.push(
                ChatCompletionRequestMessage::Assistant(
                    async_openai::types::ChatCompletionRequestAssistantMessageArgs::default()
                        .content(entry.message_text.clone())
                        .build()?
                )
            );
        } else {
            // User message
            messages.push(ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessageArgs::default()
                    .content(entry.message_text.clone())
                    .build()?,
            ));
        }
    }

    // Add current message from user
    let message_parts =
        vec![ChatCompletionRequestUserMessageContentPart::Text(
            ChatCompletionRequestMessageContentPartText {
                text: msg.text().unwrap_or("").to_string(),
            },
        )];

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
                        handle_save_memory(env, msg, &function.arguments)
                            .await?;
                        "Memory saved successfully.".to_string()
                    }
                    "execute_command" => {
                        let args: ExecuteCommandArgs =
                            serde_json::from_str(&function.arguments)?;
                        handle_execute_command(bot, env, msg, &args).await?;
                        format!("Command /{} executed.", args.command)
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
) -> Result<()> {
    let args: SaveMemoryArgs = serde_json::from_str(arguments)?;

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

    log::info!("Saved memory: {}", args.memory_text);

    Ok(())
}

/// Handle execute_command function call
async fn handle_execute_command(
    bot: &Bot,
    env: &Arc<BotEnv>,
    msg: &Message,
    args: &ExecuteCommandArgs,
) -> Result<()> {
    let command_text = format!(
        "/{} {}",
        args.command,
        args.arguments.clone().unwrap_or_default()
    );
    log::info!("Executing command: {}", command_text);

    // Create a reference to the original message to maintain context
    let mut message_builder =
        bot.send_message(msg.chat.id, command_text).disable_notification(true);

    // Only add thread_id if the chat supports threads
    if chat_supports_threads(&msg.chat) && msg.thread_id.is_some() {
        message_builder =
            message_builder.message_thread_id(msg.thread_id.unwrap());
    }

    let sent_msg = message_builder.await?;

    // The existing command handlers will process this message

    Ok(())
}
