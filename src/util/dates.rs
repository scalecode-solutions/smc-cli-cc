//! Date-filter normalization for lexical ISO-8601 timestamp comparison.

/// Normalize a date-only upper bound ("YYYY-MM-DD") so lexical comparison
/// includes the entire named day. Full timestamps compare lexically fine, but
/// a bare date sorts *before* every timestamp on that date, silently excluding
/// the whole day from `--before`. '~' (0x7E) sorts after every character that
/// can appear in an ISO-8601 timestamp.
pub fn normalize_before(b: String) -> String {
    if b.len() == 10 && b.as_bytes()[4] == b'-' && b.as_bytes()[7] == b'-' {
        format!("{b}T~")
    } else {
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_only_covers_whole_day() {
        let before = normalize_before("2026-07-01".into());
        assert!(*"2026-07-01T23:59:59.999Z" < *before);
        assert!(*"2026-07-02T00:00:00Z" > *before);
    }

    #[test]
    fn full_timestamps_pass_through() {
        assert_eq!(
            normalize_before("2026-07-01T12:00:00Z".into()),
            "2026-07-01T12:00:00Z"
        );
    }

    #[test]
    fn adversarial_short_or_weird_input_untouched() {
        assert_eq!(normalize_before("yesterday".into()), "yesterday");
        assert_eq!(normalize_before("2026/07/01".into()), "2026/07/01");
        assert_eq!(normalize_before("".into()), "");
    }
}
