//! Pixi's `exclude-newer` release-age cutoff, as observed with Pixi 0.80.0:
//! durations count back from now (a year is 365.25 days, `m` is minutes), a
//! plain date means the end of that day in UTC, and RFC 3339 timestamps are
//! exact. Other forms are rejected so availability is never guessed.

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let length = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&month) || day < 1 || day > length[(month - 1) as usize] {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

fn digits(text: &str, len: usize) -> Option<i64> {
    (text.len() == len && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// Days since the epoch for `YYYY-MM-DD`.
fn date(text: &str) -> Option<i64> {
    let mut parts = text.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    days_from_civil(digits(year, 4)?, digits(month, 2)?, digits(day, 2)?)
}

/// Milliseconds since the Unix epoch for an RFC 3339 timestamp.
pub(crate) fn timestamp(text: &str) -> Option<i64> {
    let (day, time) = text.trim().split_once(['T', 't', ' '])?;
    let days = date(day)?;
    let (clock, offset) = if let Some(clock) = time.strip_suffix(['Z', 'z']) {
        (clock, 0)
    } else {
        let at = time.rfind(['+', '-'])?;
        let (clock, zone) = time.split_at(at);
        let (hours, minutes) = zone[1..].split_once(':')?;
        let minutes = digits(hours, 2)? * 60 + digits(minutes, 2)?;
        (
            clock,
            if zone.starts_with('-') {
                -minutes
            } else {
                minutes
            },
        )
    };
    let (whole, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut fields = whole.split(':');
    let (hour, minute, second) = (
        digits(fields.next()?, 2)?,
        digits(fields.next()?, 2)?,
        digits(fields.next()?, 2)?,
    );
    if fields.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let millis = format!("{fraction:0<3}")[..3].parse::<i64>().ok()?;
    Some(days * DAY + hour * HOUR + minute * MINUTE + second * 1000 + millis - offset * MINUTE)
}

/// A relative duration such as `14d`, `2w`, `1d2h` or `14 days`.
fn duration(text: &str) -> Option<i64> {
    let mut rest = text.trim();
    let mut total = 0i64;
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let split = rest.find(|c: char| !c.is_ascii_digit())?;
        let amount: i64 = rest[..split].parse().ok()?;
        let tail = rest[split..].trim_start();
        let unit_end = tail
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(tail.len());
        // Units are case-sensitive: `M` (months) must not be read as minutes.
        let unit = match &tail[..unit_end] {
            "s" | "sec" | "secs" | "second" | "seconds" => 1000,
            "m" | "min" | "mins" | "minute" | "minutes" => MINUTE,
            "h" | "hr" | "hrs" | "hour" | "hours" => HOUR,
            "d" | "day" | "days" => DAY,
            "w" | "week" | "weeks" => 7 * DAY,
            "y" | "yr" | "yrs" | "year" | "years" => 365 * DAY + DAY / 4,
            _ => return None,
        };
        total = total.checked_add(amount.checked_mul(unit)?)?;
        rest = tail[unit_end..].trim_start();
    }
    Some(total)
}

/// End of a `YYYY-MM-DD` day in UTC, in milliseconds since the Unix epoch.
pub(crate) fn end_of_date(text: &str) -> Option<i64> {
    date(text.trim()).map(|days| (days + 1) * DAY)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Cutoff in milliseconds since the Unix epoch, or `None` if not understood.
pub(crate) fn parse(text: &str, now_ms: i64) -> Option<i64> {
    let text = text.trim();
    if let Some(instant) = timestamp(text) {
        return Some(instant);
    }
    if let Some(days) = date(text) {
        return Some((days + 1) * DAY);
    }
    duration(text).map(|span| now_ms - span)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_607_130_000; // 2026-09-28T14:52:10Z
    const DAY: i64 = 86_400_000;

    #[test]
    fn matches_cutoffs_observed_from_pixi() {
        assert_eq!(timestamp("2026-09-28T14:52:10Z"), Some(NOW));
        let cases = [
            ("14d", Some(NOW - 14 * DAY)),
            ("14 days", Some(NOW - 14 * DAY)),
            ("2w", Some(NOW - 14 * DAY)),
            ("1d2h", Some(NOW - DAY - 2 * 3_600_000)),
            ("30m", Some(NOW - 30 * 60_000)),
            ("12h", Some(NOW - 12 * 3_600_000)),
            ("1y", Some(NOW - 365 * DAY - DAY / 4)),
            ("2026-09-01", timestamp("2026-09-02T00:00:00Z")),
            (
                "2026-09-01T12:00:00+02:00",
                timestamp("2026-09-01T10:00:00Z"),
            ),
            ("P14D", None),
            ("3mo", None),
            ("bogus", None),
            ("", None),
        ];
        for (text, expected) in cases {
            assert_eq!(parse(text, NOW), expected, "{text}");
        }
    }

    #[test]
    fn parses_index_upload_times() {
        assert_eq!(
            timestamp("2024-12-04T17:35:28.113385Z"),
            Some(1_733_333_728_113)
        );
        assert_eq!(timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(timestamp("2026-13-01T00:00:00Z"), None);
        assert_eq!(timestamp("2026-09-01"), None);
    }
}
