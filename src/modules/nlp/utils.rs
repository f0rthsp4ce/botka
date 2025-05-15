//! Utility functions for the NLP module

/// Default duration hours for memory (24 hours)
#[allow(clippy::unnecessary_wraps)]
pub const fn default_duration_hours() -> Option<u32> {
    Some(24)
}

/// Intelligently splits a message into chunks smaller than the maximum allowed size
pub fn split_long_message(text: &str, max_size: usize) -> Vec<String> {
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