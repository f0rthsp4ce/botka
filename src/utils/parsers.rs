use std::time::Duration;

use nom::bytes::complete::{take_while1, take_while_m_n};
use nom::character::complete::{char, digit1, one_of};
use nom::combinator::{eof, opt};
use nom::error::ParseError;
use nom::sequence::{preceded, terminated, tuple};
use nom::IResult;
use serde::{Deserialize, Deserializer};

/// Deserialize a duration returned by mikrotik, e.g. "2w3d4h56m23s".
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
    let value = match value.parse::<u64>() {
        Ok(value) => value,
        Err(_) => {
            // TODO: better error
            return Err(nom::Err::Error(ParseError::from_error_kind(
                input,
                nom::error::ErrorKind::Digit,
            )));
        }
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

/// Parse a telegram bot command, e.g. "/command@my_bot". A bot name is
/// optional.
pub fn parse_tg_bot_command(input: &str) -> Option<(&str, Option<&str>)> {
    terminated(tg_bot_command, eof)(input).ok().map(|x| x.1)
}

fn tg_bot_command(input: &str) -> IResult<&str, (&str, Option<&str>)> {
    let (input, _) = char('/')(input)?;
    let (input, command) = take_while_m_n(
        1,
        64,
        |c| matches!(c, '0'..='9' | 'A'..='Z' | 'a'..='z' | '_'),
    )(input)?;
    let (input, bot_name) = opt(preceded(
        char('@'),
        take_while1(|c| matches!(c, '0'..='9' | 'A'..='Z' | 'a'..='z' | '_')),
    ))(input)?;
    Ok((input, (command, bot_name)))
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
    fn test_parse_tg_bot_command() {
        assert_eq!(parse_tg_bot_command("/command"), Some(("command", None)));
        assert_eq!(
            parse_tg_bot_command("/command@my_bot"),
            Some(("command", Some("my_bot")))
        );
        assert_eq!(parse_tg_bot_command("/command "), None);
    }
}
