//! Natural language processing module for understanding and responding to
//! non-command messages using `OpenAI` API.
//!
//! This module allows interaction with the bot using natural language instead
//! of formal commands, triggered by specific keywords.
//!
//! Known issues:
//! - The bot works weirdly in general topic threads of forum. Idk why.

pub mod classification;
pub mod commands;
pub mod filtering;
pub mod memory;
pub mod processing;
pub mod types;
pub mod utils;

// Re-export public API
pub use self::commands::command_handler;
pub use self::filtering::{message_handler, random_message_handler};
pub use self::memory::store_message;

/// Register metrics for `OpenAI` API usage
pub fn register_metrics() {
    let metric_name = types::METRIC_NAME;
    metrics::register_counter!(metric_name, "type" => "prompt");
    metrics::register_counter!(metric_name, "type" => "completion");
    metrics::describe_counter!(
        metric_name,
        "Total number of tokens used by OpenAI API for NLP processing."
    );
}