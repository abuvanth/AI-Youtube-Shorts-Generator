//! Trim bounds for re-exporting one short without a new model request.

use anyhow::{Result, bail};

/// Parses a start and end, in seconds, against the known media duration.
///
/// A non-positive duration skips the media-end check. Callers that do not know
/// the duration yet still reject empty, inverted, and extreme ranges.
pub fn parse_trim(start: &str, end: &str, media_duration: f64) -> Result<(f64, f64)> {
    let start = parse_seconds(start, "Start")?;
    let end = parse_seconds(end, "End")?;
    if start < 0.0 {
        bail!("Start time cannot be negative.");
    }
    let length = end - start;
    if length < 1.0 {
        bail!("The clip must be at least 1 second.");
    }
    if length > 600.0 {
        bail!("The clip cannot be longer than 600 seconds.");
    }
    if media_duration.is_finite() && media_duration > 0.0 && end > media_duration + 0.05 {
        bail!("End time is past the source duration ({media_duration:.2}s).");
    }
    Ok((round_hundredths(start), round_hundredths(end)))
}

pub fn format_seconds(value: f64) -> String {
    if !value.is_finite() {
        return "0.00".into();
    }
    format!("{:.2}", value.max(0.0))
}

fn parse_seconds(value: &str, label: &str) -> Result<f64> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{label} time is required.");
    }
    if value.contains(':') {
        let parts: Vec<_> = value.split(':').collect();
        let invalid = || anyhow::anyhow!("{label} time must use seconds, M:SS, or H:MM:SS.");
        if !(2..=3).contains(&parts.len()) {
            return Err(invalid());
        }
        let mut total = 0.0;
        for (index, part) in parts.iter().enumerate() {
            let last = index + 1 == parts.len();
            let (whole, fraction) = part.split_once('.').unwrap_or((part, "0"));
            if whole.is_empty()
                || !whole.bytes().all(|b| b.is_ascii_digit())
                || fraction.is_empty()
                || !fraction.bytes().all(|b| b.is_ascii_digit())
                || (!last && part.contains('.'))
            {
                return Err(invalid());
            }
            let number = part.parse::<f64>().map_err(|_| invalid())?;
            if index > 0 && number >= 60.0 {
                return Err(invalid());
            }
            total = total * 60.0 + number;
        }
        if !total.is_finite() {
            return Err(invalid());
        }
        return Ok(total);
    }
    let parsed = value
        .parse::<f64>()
        .map_err(|_| anyhow::anyhow!("{label} time must be a number of seconds."))?;
    if !parsed.is_finite() {
        bail!("{label} time must be a finite number of seconds.");
    }
    Ok(parsed)
}

fn round_hundredths(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_an_in_range_trim_and_rounds_hundredths() {
        let (start, end) = parse_trim(" 1.226 ", "8.2", 30.0).unwrap();
        assert_eq!((start, end), (1.23, 8.2));
        assert_eq!(format_seconds(1.2), "1.20");
    }

    #[test]
    fn accepts_clock_times_and_rejects_invalid_components() {
        assert_eq!(parse_trim("59", "1:02.25", 100.).unwrap(), (59., 62.25));
        assert_eq!(
            parse_trim("1:02:03", "1:02:08", 0.).unwrap(),
            (3723., 3728.)
        );
        for invalid in ["1:60", "1::02", "1.5:02", "1:-2", "1:2:3:4"] {
            assert!(parse_trim("0", invalid, 0.).is_err());
        }
    }

    #[test]
    fn rejects_inverted_short_and_overlong_trims() {
        assert!(
            parse_trim("8", "5", 30.0)
                .unwrap_err()
                .to_string()
                .contains("at least 1")
        );
        assert!(
            parse_trim("-1", "5", 30.0)
                .unwrap_err()
                .to_string()
                .contains("negative")
        );
        assert!(
            parse_trim("0", "700", 0.0)
                .unwrap_err()
                .to_string()
                .contains("600")
        );
        assert!(
            parse_trim("0", "12", 10.0)
                .unwrap_err()
                .to_string()
                .contains("past the source")
        );
        assert!(parse_trim("nope", "4", 10.0).is_err());
        assert!(parse_trim("0", "4", 0.0).is_ok());
    }
}
