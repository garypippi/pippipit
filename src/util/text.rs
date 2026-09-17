//! String handling that respects display width (East Asian Width).
//!
//! Wide-character SSIDs and track titles are common, so `chars().count()` does not give the column count.

use unicode_width::UnicodeWidthStr;

/// Display width, in columns.
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Fit into `max` columns, ending with `…` when it overflows.
///
/// Never cuts a wide character in half.
pub fn truncate(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    // The `…` itself needs one column.
    let budget = max.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0usize;
    for ch in text.chars() {
        let cw = UnicodeWidthStr::width(ch.to_string().as_str());
        if w + cw > budget {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Truncate to `max` columns, then pad on the right to exactly `max`.
pub fn fit(text: &str, max: usize) -> String {
    let cut = truncate(text, max);
    let pad = max.saturating_sub(width(&cut));
    format!("{cut}{}", " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_counts_wide_chars_as_two() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("あいう"), 6);
        assert_eq!(width("aあ"), 3);
    }

    #[test]
    fn short_text_is_untouched() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abc", 3), "abc");
    }

    #[test]
    fn long_ascii_is_cut_with_ellipsis() {
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        assert_eq!(width(&truncate("abcdefghij", 5)), 5);
    }

    /// A wide character is never cut in half; an odd width leaves one column spare.
    #[test]
    fn never_splits_a_wide_char() {
        let out = truncate("あいうえお", 5);
        assert!(out.ends_with('…'));
        assert!(width(&out) <= 5, "width {} of {out}", width(&out));
        assert!(!out.contains('\u{fffd}'));
    }

    #[test]
    fn fit_always_returns_exact_width() {
        for (text, w) in [
            ("abc", 10),
            ("あいうえお", 7),
            ("", 4),
            ("very long text here", 6),
        ] {
            assert_eq!(width(&fit(text, w)), w, "text={text} w={w}");
        }
    }

    #[test]
    fn zero_width_is_empty() {
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(fit("abc", 0), "");
    }

    /// An SSID too long for the column budget still ends up exactly that wide.
    #[test]
    fn long_ssid_fits() {
        assert_eq!(width(&fit("VeryLongNetworkName-5GHz", 12)), 12);
        assert_eq!(width(&fit("うちのネットワーク5G", 12)), 12);
    }
}
