//! Classification of messages for appropriate handling

use std::sync::Arc;

use anyhow::{Context as _, Result};
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs, CreateChatCompletionRequestArgs,
    ResponseFormat, ResponseFormatJsonSchema,
};
use tap::Tap;

use crate::common::BotEnv;
use crate::models::ChatHistoryEntry;
use crate::modules::nlp::types::{ClassificationResponse, RandomClassificationResult, METRIC_NAME};

/// Prompt for message classification
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

/// Prompt for random classification
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

/// Classification result
#[allow(dead_code)]
pub enum ClassificationResult {
    /// Request should be handled by the bot with specified complexity
    Handle(u8),
    /// Request should be ignored
    Ignore,
}

impl ClassificationResult {
    pub fn as_str(&self) -> String {
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

/// Classify the request type based on the message content.
///
/// This function uses cheap model to classify the request type,
/// this info is then used to determine if the request should be handled
/// and which model to use.
pub async fn classify_request(
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

/// Classify whether to intervene in random message sampling
pub async fn classify_random_request(
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