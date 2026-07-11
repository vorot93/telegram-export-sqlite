use crate::error::{Result, TelegramExportError};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use regex::Regex;

pub fn parse_telegram_timestamp(input: &str) -> Result<String> {
    let re = Regex::new(
        r"^(?P<day>[0-9]{2})\.(?P<month>[0-9]{2})\.(?P<year>[0-9]{4}) (?P<hour>[0-9]{2}):(?P<minute>[0-9]{2}):(?P<second>[0-9]{2}) (?P<tz>.+)$",
    )
    .expect("timestamp regex compiles");
    let captures = re
        .captures(input)
        .ok_or_else(|| TelegramExportError::Parse(format!("invalid timestamp: {input}")))?;

    let date = NaiveDate::from_ymd_opt(
        parse_number(&captures["year"], "invalid date", input)?,
        parse_number(&captures["month"], "invalid date", input)?,
        parse_number(&captures["day"], "invalid date", input)?,
    )
    .ok_or_else(|| TelegramExportError::Parse(format!("invalid date: {input}")))?;
    let time = NaiveTime::from_hms_opt(
        parse_number(&captures["hour"], "invalid time", input)?,
        parse_number(&captures["minute"], "invalid time", input)?,
        parse_number(&captures["second"], "invalid time", input)?,
    )
    .ok_or_else(|| TelegramExportError::Parse(format!("invalid time: {input}")))?;
    let local = NaiveDateTime::new(date, time);
    let offset = parse_offset(&captures["tz"])?;
    let fixed: DateTime<FixedOffset> = offset
        .from_local_datetime(&local)
        .single()
        .ok_or_else(|| TelegramExportError::Parse(format!("ambiguous timestamp: {input}")))?;

    Ok(fixed
        .with_timezone(&Utc)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string())
}

pub fn parse_duration_seconds(input: &str) -> Result<i64> {
    let parts: Vec<i64> = input
        .split(':')
        .map(|part| part.parse::<i64>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| TelegramExportError::Parse(format!("invalid duration: {input}")))?;

    match parts.as_slice() {
        [minutes, seconds] if (0..60).contains(minutes) && (0..60).contains(seconds) => {
            checked_duration_seconds(input, 0, *minutes, *seconds)
        }
        [hours, minutes, seconds]
            if *hours >= 0 && (0..60).contains(minutes) && (0..60).contains(seconds) =>
        {
            checked_duration_seconds(input, *hours, *minutes, *seconds)
        }
        _ => Err(TelegramExportError::Parse(format!(
            "invalid duration: {input}"
        ))),
    }
}

fn parse_offset(input: &str) -> Result<FixedOffset> {
    if input == "UTC" || input == "GMT" {
        return Ok(FixedOffset::east_opt(0).unwrap());
    }

    let re = Regex::new(r"^UTC(?P<sign>[+-])(?P<hours>[0-9]{2}):(?P<minutes>[0-9]{2})$")
        .expect("offset regex compiles");
    let captures = re
        .captures(input)
        .ok_or_else(|| TelegramExportError::Parse(format!("unsupported timezone: {input}")))?;
    let hours: i32 = parse_number(&captures["hours"], "invalid timezone offset", input)?;
    let minutes: i32 = parse_number(&captures["minutes"], "invalid timezone offset", input)?;
    if minutes >= 60 {
        return Err(TelegramExportError::Parse(format!(
            "invalid timezone offset: {input}"
        )));
    }
    let seconds = hours * 3600 + minutes * 60;

    if &captures["sign"] == "-" {
        FixedOffset::west_opt(seconds)
    } else {
        FixedOffset::east_opt(seconds)
    }
    .ok_or_else(|| TelegramExportError::Parse(format!("invalid timezone offset: {input}")))
}

fn parse_number<T>(value: &str, message: &str, input: &str) -> Result<T>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| TelegramExportError::Parse(format!("{message}: {input}")))
}

fn checked_duration_seconds(input: &str, hours: i64, minutes: i64, seconds: i64) -> Result<i64> {
    hours
        .checked_mul(3600)
        .and_then(|hours| {
            minutes
                .checked_mul(60)
                .and_then(|minutes| hours.checked_add(minutes))
        })
        .and_then(|total| total.checked_add(seconds))
        .ok_or_else(|| TelegramExportError::Parse(format!("invalid duration: {input}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_telegram_timestamp_with_utc_offset() {
        assert_eq!(
            parse_telegram_timestamp("12.02.2025 08:37:48 UTC-08:00").unwrap(),
            "2025-02-12T16:37:48Z"
        );
    }

    #[test]
    fn parses_telegram_timestamp_with_known_abbreviation() {
        assert_eq!(
            parse_telegram_timestamp("12.02.2025 08:37:48 UTC").unwrap(),
            "2025-02-12T08:37:48Z"
        );
    }

    #[test]
    fn parses_duration() {
        assert_eq!(parse_duration_seconds("01:02:03").unwrap(), 3723);
        assert_eq!(parse_duration_seconds("02:03").unwrap(), 123);
    }

    #[test]
    fn rejects_telegram_timestamp_with_invalid_offset_minutes() {
        for tz in ["UTC+00:60", "UTC+00:99", "UTC-00:60"] {
            let input = format!("12.02.2025 08:37:48 {tz}");

            assert!(matches!(
                parse_telegram_timestamp(&input),
                Err(TelegramExportError::Parse(_))
            ));
        }
    }

    #[test]
    fn rejects_invalid_duration_fields() {
        for duration in ["02:60", "01:99:99", "-01:02"] {
            assert!(matches!(
                parse_duration_seconds(duration),
                Err(TelegramExportError::Parse(_))
            ));
        }
    }

    #[test]
    fn rejects_non_ascii_numeric_timestamp_fields_without_panicking() {
        assert_parse_error_without_panic(|| {
            parse_telegram_timestamp("\u{0661}\u{0662}.02.2025 08:37:48 UTC")
        });
    }

    #[test]
    fn rejects_non_ascii_numeric_offset_fields_without_panicking() {
        assert_parse_error_without_panic(|| {
            parse_telegram_timestamp("12.02.2025 08:37:48 UTC+\u{0660}\u{0661}:00")
        });
    }

    #[test]
    fn rejects_overflowing_duration_without_panicking() {
        let duration = format!("{}:00:00", i64::MAX);

        assert_parse_error_without_panic(|| parse_duration_seconds(&duration));
    }

    fn assert_parse_error_without_panic<T, F>(parse: F)
    where
        F: FnOnce() -> Result<T> + std::panic::UnwindSafe,
    {
        let result = match std::panic::catch_unwind(parse) {
            Ok(result) => result,
            Err(_) => panic!("parser panicked instead of returning a parse error"),
        };

        assert!(matches!(result, Err(TelegramExportError::Parse(_))));
    }
}
