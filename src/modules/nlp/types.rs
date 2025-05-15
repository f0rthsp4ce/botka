//! Common types and constants for the NLP module

use serde::{Deserialize, Serialize};

// Metric name for monitoring token usage
pub const METRIC_NAME: &str = "botka_openai_nlp_used_tokens_total";

// Function call argument definitions
#[derive(Serialize, Deserialize, Debug)]
pub struct SaveMemoryArgs {
    pub memory_text: String,
    #[serde(default = "crate::modules::nlp::utils::default_duration_hours")]
    pub duration_hours: Option<u32>,
    #[serde(default)]
    pub chat_specific: bool,
    #[serde(default)]
    pub thread_specific: bool,
    #[serde(default)]
    pub user_specific: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RemoveMemoryArgs {
    pub memory_id: i32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ExecuteCommandArgs {
    pub command: String,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SearchArgs {
    pub query: String,
}

/// Response from NLP processing
pub enum NlpResponse {
    Text(String),
    Ignore,
}

/// Debug information about NLP processing
pub struct NlpDebug {
    pub classification_result: crate::modules::nlp::classification::ClassificationResult,
    pub used_model: Option<String>,
}

// Classification related types
#[derive(Deserialize)]
pub struct ClassificationResponse {
    pub classification: Option<u8>,
}

#[derive(Deserialize)]
pub struct RandomClassificationResult {
    pub intervene: bool,
}