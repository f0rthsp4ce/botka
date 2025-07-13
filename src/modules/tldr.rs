use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs,
    ResponseFormat, ResponseFormatJsonSchema,
};
use chrono::{Duration, Utc};
use diesel::{ExpressionMethods, QueryDsl, RunQueryDsl};
use macro_rules_attribute::derive;
use serde::Deserialize;
use teloxide::macros::BotCommands;
use teloxide::prelude::*;

use crate::common::{filter_command, BotCommandsExt, BotEnv, UpdateHandler};
use crate::db::DbUserId;
use crate::models::{ChatHistoryEntry, TgUser};
use crate::modules::nlp::memory::get_chat_history;
use crate::utils::BotExt;

/// JSON filter returned by LLM for TLDR requests.
#[derive(Debug, Deserialize)]
struct TldrFilter {
    /// How many hours back to take messages. `None` means ignore.
    #[serde(default)]
    time: Option<u32>,
    /// How many last messages to take. `None` means ignore.
    #[serde(default)]
    messages: Option<u32>,
}

/// Bot commands handled by this module.
#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(
        description = "summarize long discussion (TL;DR). Rest of the text is query."
    )]
    Tldr,
}

pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(handle_command)
}

async fn handle_command(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    _cmd: Commands,
) -> Result<()> {
    // Extract the rest of the message after the command itself.
    let user_query = msg
        .text()
        .unwrap_or("")
        .split_once(' ')
        .map_or("", |(_, rest)| rest.trim());

    // First step: convert natural language query to machine-readable filter using LLM.
    let filter = if user_query.is_empty() {
        TldrFilter { time: None, messages: Some(100) }
    } else {
        convert_query_to_filter(&env, user_query).await?
    };

    // Fetch chat history via NLP helper
    let mut history = get_chat_history(&env, msg.chat.id, msg.thread_id)?;

    // Apply time filter
    if let Some(hours) = filter.time {
        let since =
            (Utc::now() - Duration::hours(i64::from(hours))).naive_utc();
        history.retain(|e| e.timestamp >= since);
    }

    // The helper returns newest first; we want chronological order for summarizer
    history.reverse();

    // Apply message count filter (take last N) WITH HARD CAP OF 500
    // Determine the effective limit: requested value or 500 (whichever is smaller).
    // If the user did not specify any limit, default to the hard cap of 500.
    let effective_limit: u32 = filter.messages.unwrap_or(500).min(500);

    if history.len() > effective_limit as usize {
        history = history[history.len() - effective_limit as usize..].to_vec();
    }

    if history.is_empty() {
        bot.reply_message(&msg, "No messages found for summarization.").await?;
        return Ok(());
    }

    // Second step: summarize collected messages using LLM.
    let summary = summarize_messages(&env, &history).await?;

    let summary_trimmed = summary.trim();
    if summary_trimmed.is_empty() {
        bot.reply_message(&msg, "Failed to build summary.").await?;
    } else {
        bot.reply_message(&msg, summary_trimmed).await?;
    }

    Ok(())
}

/// Call LLM to convert user query into a `TldrFilter`.
async fn convert_query_to_filter(
    env: &Arc<BotEnv>,
    query: &str,
) -> Result<TldrFilter> {
    // Prepare prompt
    const SYSTEM_PROMPT: &str = "You are a converter that transforms Russian or English natural language TLDR requests into a strict JSON filter with the following schema: {\n  \"time\": <integer hours or null>,\n  \"messages\": <integer or null>\n}.\nThe field \"time\" represents how many hours back from now to include messages. \nThe field \"messages\" represents how many last messages to include.\nReturn ONLY the JSON object with no leading or trailing explanation.";

    let request = CreateChatCompletionRequestArgs::default()
        .model(env.config.services.openai.model.clone())
        .messages(vec![
            ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessageArgs::default()
                    .content(SYSTEM_PROMPT.to_string())
                    .build()?,
            ),
            ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessageArgs::default()
                    .content(query.to_string())
                    .build()?,
            ),
        ])
        .response_format(ResponseFormat::JsonSchema {
            json_schema: ResponseFormatJsonSchema {
                name: "TldrFilter".to_string(),
                description: Some(
                    "Filter specification for TLDR operation".to_string(),
                ),
                schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "time": {"type": ["integer", "null"]},
                        "messages": {"type": ["integer", "null"]}
                    },
                    "required": ["time", "messages"],
                    "additionalProperties": false
                })),
                strict: Some(true),
            },
        })
        .max_tokens(20u32)
        .temperature(0.0)
        .build()?;

    let response = env.openai_client.chat().create(request).await?;

    let choice =
        response.choices.first().context("No choices in LLM response")?;
    let content = choice.message.content.clone().unwrap_or_default();

    if content.is_empty() {
        anyhow::bail!("Empty content from LLM when converting query");
    }

    let filter: TldrFilter = serde_json::from_str(&content)
        .context("Failed to parse filter JSON")?;

    Ok(filter)
}

// Removed custom DB query. History retrieval handled via NLP module.

/// Summarize collected messages using LLM.
async fn summarize_messages(
    env: &Arc<BotEnv>,
    history: &[ChatHistoryEntry],
) -> Result<String> {
    if history.is_empty() {
        anyhow::bail!("No messages to summarize");
    }

    // Map user IDs to display names once to avoid multiple DB queries
    let user_map: HashMap<DbUserId, String> = {
        let user_ids: Vec<DbUserId> =
            history.iter().filter_map(|e| e.from_user_id).collect();

        if user_ids.is_empty() {
            HashMap::new()
        } else {
            env.transaction(|conn| {
                use crate::schema::tg_users::dsl as tu;
                tu::tg_users
                    .filter(tu::id.eq_any(&user_ids))
                    .load::<TgUser>(conn)
            })?
            .into_iter()
            .map(|u| {
                let name = u.username.map_or_else(
                    || {
                        let mut n = u.first_name;
                        if let Some(last) = u.last_name {
                            n.push(' ');
                            n.push_str(&last);
                        }
                        n
                    },
                    |un| format!("@{un}"),
                );
                (u.id, name)
            })
            .collect()
        }
    };

    let transcript = history
        .iter()
        .filter(|e| !e.message_text.is_empty())
        .map(|e| {
            let prefix = e
                .from_user_id
                .and_then(|id| user_map.get(&id).cloned())
                .unwrap_or_else(|| "Unknown".to_string());
            format!("{prefix}: {}", e.message_text)
        })
        .collect::<Vec<_>>()
        .join("\n");

    if transcript.trim().is_empty() {
        anyhow::bail!("No messages to summarize");
    }

    const SYSTEM_PROMPT: &str = "You are an assistant that produces a concise TL;DR summary (in the same language as the messages) of the following Telegram thread messages. Focus on the key discussion points and decisions. Return no more than 10 sentences. Mention the most important points and decisions. Do not mention the user names with @.";

    // Choose summarization model: use the last (most capable) model from config.nlp.models if present, else default openai model.
    let model = env
        .config
        .nlp
        .models
        .last()
        .cloned()
        .unwrap_or_else(|| env.config.services.openai.model.clone());

    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages(vec![
            ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessageArgs::default()
                    .content(SYSTEM_PROMPT.to_string())
                    .build()?,
            ),
            ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessageArgs::default()
                    .content(transcript)
                    .build()?,
            ),
        ])
        .max_tokens(300u32)
        .temperature(0.3)
        .build()?;

    let response = env.openai_client.chat().create(request).await?;
    let choice = response
        .choices
        .first()
        .context("No choices in summarization response")?;
    let summary = choice.message.content.clone().unwrap_or_default();

    if summary.is_empty() {
        anyhow::bail!("LLM returned empty summary");
    }

    Ok(summary)
}
