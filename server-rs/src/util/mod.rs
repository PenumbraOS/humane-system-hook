pub mod markdown;
pub mod serde;

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

pub fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn compact_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}


/// Removes URLs from `text`. Useful for preventing LLM responses being read aloud
/// as an annoyingly long and useless reading of a URL like
/// "H T T P slash slash example com slash path slash page H T M L."
pub fn remove_urls(text: &str) -> String {
    // Matches RFC 3986 scheme followed by `://` and a run of non-whitespace,
    // non-quote/angle-bracket characters.
    static URL_PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"\b[a-zA-Z][a-zA-Z0-9+.-]*://[^\s<>"]+"#).expect("valid regex"));

    // Characters that are almost always sentence punctuation that may come
    // after a URL (e.g. "(see https://example.com).").
    const TRAILING_PUNCTUATION: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '\'', '"'];

    URL_PATTERN
        .replace_all(text, |caps: &regex::Captures| {
            let matched = &caps[0];
            let trimmed = matched.trim_end_matches(TRAILING_PUNCTUATION);
            let trailing = &matched[trimmed.len()..];

            match reqwest::Url::parse(trimmed).ok() {
                Some(url) if url.host_str().is_some() => trailing.to_string(),
                _ => matched.to_string(),
            }
        })
        .into_owned()
}

/// Removes markdown characters and URLs so `text` can be read cleanly
pub fn clean_for_speech(text: &str) -> String {
    let text = markdown::strip_markdown_for_speech(text);
    let text = remove_urls(&text);
    compact_whitespace(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_url_entirely() {
        assert_eq!(
            remove_urls("foo://a.b.c.d/a/b/c.d/e.f#foo?g=h%20ijk&lmno=pqr"),
            ""
        );
    }

    #[test]
    fn preserves_surrounding_sentence_and_trailing_punctuation() {
        assert_eq!(
            remove_urls(
                "Check https://www.nytimes.com/2026/07/18/sports/world-cup.html for details."
            ),
            "Check  for details."
        );
    }

    #[test]
    fn preserves_wrapping_parentheses() {
        assert_eq!(remove_urls("(see https://example.com/page)"), "(see )");
    }

    #[test]
    fn removes_multiple_urls_independently() {
        assert_eq!(
            remove_urls("From http://a.com/x and https://b.org/y, both agree."),
            "From  and , both agree."
        );
    }

    #[test]
    fn leaves_text_without_urls_completely_unchanged() {
        let text = "e.g. version 3.4.5 was released on 7/18, per the docs.";
        assert_eq!(remove_urls(text), text);
    }

    #[test]
    fn leaves_urls_without_a_host_unchanged() {
        let text = "The config lives at file:///etc/passwd on that box.";
        assert_eq!(remove_urls(text), text);
    }

    #[test]
    fn leaves_empty_string_unchanged() {
        assert_eq!(remove_urls(""), "");
    }

    #[test]
    fn clean_for_speech_strips_markdown_and_urls_and_collapses_whitespace() {
        assert_eq!(
            clean_for_speech(
                "**Check** this out: https://example.com/page\n\n- one\n- two"
            ),
            "Check this out: one; two"
        );
    }
}
