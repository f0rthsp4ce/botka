use std::time::Duration;

use nom::bytes::complete::{tag, take_while1};
use nom::character::complete::{digit1, one_of};
use nom::combinator::rest;
use nom::error::ParseError;
use nom::sequence::tuple;
use nom::IResult;
use serde::{Deserialize, Deserializer};
use teloxide::prelude::ChatId;
use teloxide::types::{MessageId, ThreadId};

use super::ThreadIdPair;

/// Deserialize a duration returned by mikrotik, e.g. `"2w3d4h56m23s"`.
pub fn deserealize_duration<'de, D>(
    deserializer: D,
) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    let (_, duration) = duration(&s).map_err(serde::de::Error::custom)?;
    Ok(duration)
}

fn duration(mut input: &str) -> IResult<&str, Duration> {
    if input == "never" {
        return Ok(("", Duration::new(u64::MAX, 0)));
    }
    let mut duration = Duration::new(0, 0);
    while !input.is_empty() {
        let (i, segment) = duration_segment(input)?;
        input = i;
        duration += segment;
    }
    Ok((input, duration))
}

fn duration_segment(input: &str) -> IResult<&str, Duration> {
    let (input, (value, unit)) = tuple((digit1, one_of("wdhms")))(input)?;
    let Ok(value) = value.parse::<u64>() else {
        // TODO: better error
        return Err(nom::Err::Error(ParseError::from_error_kind(
            input,
            nom::error::ErrorKind::Digit,
        )));
    };
    let duration = match unit {
        'w' => Duration::from_secs(value * 7 * 24 * 60 * 60),
        'd' => Duration::from_secs(value * 24 * 60 * 60),
        'h' => Duration::from_secs(value * 60 * 60),
        'm' => Duration::from_secs(value * 60),
        's' => Duration::from_secs(value),
        _ => unreachable!(),
    };
    Ok((input, duration))
}

/// Parse a telegram bot api path, dropping the credentials. E.g., parsing
/// `"/bot123:abc/GetUpdates"` gives `Some("GetUpdates")`.
pub fn parse_tgapi_method(input: &str) -> Option<&str> {
    tgapi_method(input).ok().map(|(_, method)| method)
}

fn tgapi_method(input: &str) -> IResult<&str, &str> {
    let (input, _) = tag("/bot")(input)?;
    let (input, _) = digit1(input)?;
    let (input, _) = tag(":")(input)?;
    let (input, _) = take_while1(|c| c != '/')(input)?;
    let (input, _) = tag("/")(input)?;
    let (input, method) = rest(input)?;
    Ok((input, method))
}

/// Parse a Telegram thread link, e.g. `"https://t.me/c/1234567890/321"`.
/// XXX: This doesn't support chats with @username.
pub fn parse_tg_thread_link(input: &str) -> Option<ThreadIdPair> {
    let input = input.strip_prefix("https://t.me/c/")?;
    let (chat, thread) = input.split_once('/')?;
    Some(ThreadIdPair {
        chat: ChatId(-1_000_000_000_000 - chat.parse::<i64>().ok()?),
        thread: ThreadId(MessageId(thread.parse().ok()?)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: Duration = Duration::from_secs(1);
    const MIN: Duration = SEC.saturating_mul(60);
    const HOUR: Duration = MIN.saturating_mul(60);
    const DAY: Duration = HOUR.saturating_mul(24);
    const WEEK: Duration = DAY.saturating_mul(7);

    const DURATION: Duration = WEEK
        .saturating_mul(2)
        .saturating_add(DAY.saturating_mul(3))
        .saturating_add(HOUR.saturating_mul(4))
        .saturating_add(MIN.saturating_mul(56))
        .saturating_add(SEC.saturating_mul(23));
    const DURATION_STR: &str = "2w3d4h56m23s";

    #[test]
    fn test_duration() {
        assert_eq!(duration(DURATION_STR), Ok(("", DURATION)));
    }

    #[test]
    fn test_deserialize_duration() {
        #[derive(Debug, Deserialize)]
        pub struct TestDuration(
            #[serde(deserialize_with = "deserealize_duration")] Duration,
        );

        let time_struct: TestDuration =
            serde_json::from_str(&format!("\"{DURATION_STR}\"")).unwrap();
        assert_eq!(time_struct.0, DURATION);
    }

    #[test]
    fn test_parse_tgapi_method() {
        assert_eq!(
            parse_tgapi_method("/bot123:abc/GetUpdates"),
            Some("GetUpdates"),
        );
    }

    #[test]
    fn test_parse_tg_thread_link() {
        assert_eq!(
            parse_tg_thread_link("https://t.me/c/1234567890/321"),
            Some(ThreadIdPair {
                #[allow(clippy::unreadable_literal)]
                chat: ChatId(-1001234567890),
                thread: ThreadId(MessageId(321)),
            }),
        );
        assert_eq!(
            parse_tg_thread_link("https://t.me/c/1234567890/321/4321"),
            None
        );
    }
}
