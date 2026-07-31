pub mod serde;

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn compact_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// `String::truncate` panics when the byte offset splits a multi-byte character.
pub fn truncate_on_char_boundary(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
}

#[cfg(test)]
mod tests {
    use super::truncate_on_char_boundary;

    #[test]
    fn floors_a_mid_character_offset_to_the_previous_boundary() {
        // 'a' is byte 0; 'é' occupies bytes 1..3, so byte 2 splits it.
        let mut text = "aébc".to_string();
        truncate_on_char_boundary(&mut text, 2);
        assert_eq!(text, "a");
    }

    #[test]
    fn keeps_an_exact_boundary_offset() {
        let mut text = "aébc".to_string();
        truncate_on_char_boundary(&mut text, 3);
        assert_eq!(text, "aé");
    }

    #[test]
    fn leaves_a_string_shorter_than_the_limit_untouched() {
        let mut text = "hi 😀".to_string();
        let before = text.clone();
        truncate_on_char_boundary(&mut text, 100);
        assert_eq!(text, before);
    }
}
