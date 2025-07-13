//! Main processing functionality for NLP

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
};
use chrono::{Local, Utc};
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use log::debug;
use tap::Tap;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, Message};
use teloxide::utils::html::escape;
use tokio::sync::RwLock;

use crate::common::BotEnv;
use crate::db::DbUserId;
use crate::models::{ChatHistoryEntry, Memory};
use crate::modules::mac_monitoring;
use crate::modules::nlp::classification::{
    classify_request, ClassificationResult,
};
use crate::modules::nlp::commands::handle_execute_command;
use crate::modules::nlp::memory::{
    get_chat_history, get_relevant_memories, handle_remove_memory,
    handle_save_memory, store_bot_response,
};
use crate::modules::nlp::types::{
    ExecuteCommandArgs, NlpDebug, NlpResponse, RemoveMemoryArgs, SearchArgs,
    METRIC_NAME,
};
use crate::modules::nlp::utils::split_long_message;
use crate::utils::{MessageExt, ResultExt};

/// Prompt for the main chat interaction
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
  Do not overdo it, just use it in some words. Use it in a way that makes sense and is not too forced.
- For Russian messages, use diminutive forms of words (e.g., "котик" instead of "кот",
  "привет" -> "приветик", "собака" -> "собачка"). Use childish forms of words.
  Do not overdo it, just use it in some words. Use it in a way that makes sense and is not too forced.
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

/// Prompt for search functionality
const SEARCH_PROMPT: &str = r"You are a helpful assistant that can search for information.
You can use the search function to find relevant information based on the user's query.

ALWAYS USE THE SEARCH FUNCTION TO FIND INFORMATION.
DO NOT USE MARKDOWN OR HTML FORMATTING.
DO NOT USE YOUR OWN KNOWLEDGE, ONLY USE THE SEARCH FUNCTION.
";

/// Get the set of tools available for chat completion
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

/// Main handler for NLP messages
pub async fn handle_nlp_message(
    bot: Bot,
    env: Arc<BotEnv>,
    mac_state: Arc<RwLock<mac_monitoring::State>>,
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
                            .eq(crate::db::DbChatId::from(msg.chat.id)),
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

/// Process message with LLM using the function calling protocol
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
pub async fn process_with_function_calling(
    bot: &Bot,
    env: &Arc<BotEnv>,
    mac_state: &Arc<RwLock<mac_monitoring::State>>,
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

/// Handle search function call
pub async fn handle_search(env: &Arc<BotEnv>, query: &str) -> Result<String> {
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
