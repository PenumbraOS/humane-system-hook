use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

enum Skip {
    None,
    CodeBlock(String),
    Table,
}

/// Renders markdown as plain prose suitable for text-to-speech.
/// - Bold, italic, strikethrough, headings, superscript, subscript,
///   blockquote, and horizontal rule markdown syntax characters are
///   dropped while their inner text is retained.
/// - Raw inline HTML is removed entirely; text within it is retained.
/// - URLs in links are removed entirely; text linking to a URL is retained.
/// - Bullet lists are flattened into a single semicolon-delimited list.
/// - Multi-line blocks of code are replaced with an explanation that a code
///   block was removed for brevity.
/// - Single lines of code are retained.
/// - Tabular data is replaced with a similar explanation as multi-line code.
pub fn strip_markdown_for_speech(text: &str) -> String {
    const CODE_BLOCK_PLACEHOLDER: &str =
        "Code snippet was provided here, but removed for brevity.";
    const TABLE_PLACEHOLDER: &str = "Table data was provided here, but removed for brevity.";

    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT;
    let parser = Parser::new_ext(text, options);

    let mut output = String::new();
    let mut list_item_seen: Vec<bool> = Vec::new();
    let mut skip = Skip::None;

    let push_block_separator = |output: &mut String| {
        if !output.is_empty() && !output.ends_with(char::is_whitespace) {
            output.push(' ');
        }
    };

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::CodeBlock(_) => {
                    skip = Skip::CodeBlock(String::new());
                }
                Tag::Table(_) => {
                    push_block_separator(&mut output);
                    output.push_str(TABLE_PLACEHOLDER);
                    skip = Skip::Table;
                }
                Tag::List(_) => {
                    list_item_seen.push(false);
                }
                Tag::Item if matches!(skip, Skip::None) => {
                    let is_first = !list_item_seen.last().copied().unwrap_or(false);
                    if is_first {
                        push_block_separator(&mut output);
                    } else {
                        output.push_str("; ");
                    }
                    if let Some(seen) = list_item_seen.last_mut() {
                        *seen = true;
                    }
                }
                Tag::Paragraph | Tag::Heading { .. } | Tag::BlockQuote(_)
                    if matches!(skip, Skip::None) =>
                {
                    push_block_separator(&mut output);
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::CodeBlock => {
                    if let Skip::CodeBlock(code) = std::mem::replace(&mut skip, Skip::None) {
                        let trimmed = code.strip_suffix('\n').unwrap_or(&code);
                        push_block_separator(&mut output);
                        if !trimmed.is_empty() && !trimmed.contains('\n') {
                            output.push_str(trimmed);
                        } else {
                            output.push_str(CODE_BLOCK_PLACEHOLDER);
                        }
                    }
                }
                TagEnd::Table => {
                    skip = Skip::None;
                }
                TagEnd::List(_) => {
                    list_item_seen.pop();
                }
                _ => {}
            },
            Event::Text(t) | Event::Code(t) => match &mut skip {
                Skip::CodeBlock(code) => code.push_str(&t),
                Skip::Table => {}
                Skip::None => output.push_str(&t),
            },
            Event::SoftBreak | Event::HardBreak if matches!(skip, Skip::None) => {
                push_block_separator(&mut output);
            }
            _ => {}
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_bold() {
        assert_eq!(strip_markdown_for_speech("This is **bold** text."), "This is bold text.");
    }

    #[test]
    fn strips_italic_asterisk_and_underscore() {
        assert_eq!(strip_markdown_for_speech("This is *italic*."), "This is italic.");
        assert_eq!(strip_markdown_for_speech("This is _italic_."), "This is italic.");
    }

    #[test]
    fn strips_bold_underscore() {
        assert_eq!(strip_markdown_for_speech("This is __bold__."), "This is bold.");
    }

    #[test]
    fn strips_combined_bold_italic() {
        assert_eq!(strip_markdown_for_speech("This is ***both***."), "This is both.");
    }

    #[test]
    fn strips_nested_emphasis() {
        assert_eq!(
            strip_markdown_for_speech("**bold with *nested* stuff**"),
            "bold with nested stuff"
        );
    }

    #[test]
    fn strips_strikethrough() {
        assert_eq!(strip_markdown_for_speech("That is ~~wrong~~ correct."), "That is wrong correct.");
    }

    #[test]
    fn strips_superscript_caret_with_word_boundary() {
        assert_eq!(
            strip_markdown_for_speech("The result is ^super^ neat."),
            "The result is super neat."
        );
    }

    #[test]
    fn strips_subscript_tilde_with_word_boundary() {
        assert_eq!(
            strip_markdown_for_speech("Say ~sub~ out loud."),
            "Say sub out loud."
        );
    }

    #[test]
    fn intraword_caret_without_boundary_is_left_as_is() {
        // ^ (like _) can't open directly against a preceding word character,
        // so this deliberately does NOT get treated as superscript.
        assert_eq!(strip_markdown_for_speech("x^2^ is x squared."), "x^2^ is x squared.");
    }

    #[test]
    fn strips_heading_hashes() {
        assert_eq!(strip_markdown_for_speech("# Tomorrow's forecast"), "Tomorrow's forecast");
        assert_eq!(strip_markdown_for_speech("### Sub sub heading"), "Sub sub heading");
    }

    #[test]
    fn flattens_bullet_list_to_semicolons() {
        assert_eq!(
            strip_markdown_for_speech("- one\n- two\n- three"),
            "one; two; three"
        );
    }

    #[test]
    fn flattens_asterisk_bullet_list_to_semicolons() {
        assert_eq!(strip_markdown_for_speech("* one\n* two"), "one; two");
    }

    #[test]
    fn flattens_numbered_list_to_semicolons() {
        assert_eq!(
            strip_markdown_for_speech("1. first\n2. second\n3. third"),
            "first; second; third"
        );
    }

    #[test]
    fn keeps_link_text_drops_destination() {
        assert_eq!(
            strip_markdown_for_speech("According to [the New York Times](https://nytimes.com/x), it rained."),
            "According to the New York Times, it rained."
        );
    }

    #[test]
    fn strips_inline_code_backticks() {
        assert_eq!(strip_markdown_for_speech("Run `cargo build` now."), "Run cargo build now.");
    }

    #[test]
    fn reads_single_line_fenced_code_block_like_inline_code() {
        assert_eq!(
            strip_markdown_for_speech("Run this:\n\n```bash\ncargo build --release\n```\n\nThat's it."),
            "Run this: cargo build --release That's it."
        );
    }

    #[test]
    fn replaces_multiline_fenced_code_block_with_placeholder() {
        assert_eq!(
            strip_markdown_for_speech("Here:\n\n```rust\nfn main() {\n    todo!();\n}\n```\n\nThat's it."),
            "Here: Code snippet was provided here, but removed for brevity. That's it."
        );
    }

    #[test]
    fn replaces_empty_fenced_code_block_with_placeholder() {
        assert_eq!(
            strip_markdown_for_speech("```\n```"),
            "Code snippet was provided here, but removed for brevity."
        );
    }

    #[test]
    fn replaces_table_with_placeholder() {
        let table = "| A | B |\n|---|---|\n| 1 | 2 |";
        assert_eq!(strip_markdown_for_speech(table), "Table data was provided here, but removed for brevity.");
    }

    #[test]
    fn strips_blockquote_marker() {
        assert_eq!(strip_markdown_for_speech("> quoted text"), "quoted text");
    }

    #[test]
    fn drops_horizontal_rule() {
        assert_eq!(
            strip_markdown_for_speech("before\n\n---\n\nafter"),
            "before after"
        );
    }

    #[test]
    fn resolves_escaped_asterisks_as_literal_not_emphasis() {
        assert_eq!(strip_markdown_for_speech(r"\*not italic\*"), "*not italic*");
    }

    #[test]
    fn does_not_misinterpret_math_or_snake_case_as_emphasis() {
        assert_eq!(
            strip_markdown_for_speech("2 * 3 = 6, see file_name.txt"),
            "2 * 3 = 6, see file_name.txt"
        );
    }

    #[test]
    fn leaves_plain_text_completely_unchanged() {
        let text = "Just a normal sentence with no markdown at all.";
        assert_eq!(strip_markdown_for_speech(text), text);
    }

    #[test]
    fn leaves_empty_string_unchanged() {
        assert_eq!(strip_markdown_for_speech(""), "");
    }

    #[test]
    fn joins_multiple_paragraphs_with_separation() {
        assert_eq!(
            strip_markdown_for_speech("First paragraph.\n\nSecond paragraph."),
            "First paragraph. Second paragraph."
        );
    }

    #[test]
    fn handles_realistic_mixed_response() {
        let input = "## Weekend Forecast\n\nHere's what to expect:\n\n- **Saturday**: sunny, high of 75\n- *Sunday*: rain, check [the weather site](https://weather.example.com) for updates\n\nStay dry!";
        assert_eq!(
            strip_markdown_for_speech(input),
            "Weekend Forecast Here's what to expect: Saturday: sunny, high of 75; Sunday: rain, check the weather site for updates Stay dry!"
        );
    }
}
