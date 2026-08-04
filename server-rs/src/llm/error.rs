use std::fmt::Display;

/// Strip query strings from any URLs in a string
/// Intended to prevent leaking API keys in error messages
pub fn strip_query_strings(string: &str) -> String {
    let mut out = String::with_capacity(string.len());
    let mut in_query_params = false;

    for c in string.chars() {
        if in_query_params {
            if c == ')' || c.is_whitespace() {
                in_query_params = false;
            } else {
                continue;
            }
        } else if c == '?' {
            in_query_params = true;
            continue;
        }

        out.push(c);
    }

    out
}

/// Convert a raw LLM provider error into a friendly, speakable sentence.
pub fn friendly_error_message(e: &impl Display) -> String {
    let raw = e.to_string().to_lowercase();

    if raw.contains("429")
        || raw.contains("rate limit")
        || raw.contains("resource_exhausted")
        || raw.contains("too many requests")
    {
        "I'm getting too many requests right now. Please try again in a moment.".into()
    } else if raw.contains("401")
        || raw.contains("403")
        || raw.contains("unauthorized")
        || raw.contains("forbidden")
        || raw.contains("invalid api key")
        || raw.contains("permission denied")
    {
        "There's a problem with the API key configuration. Please check the server settings.".into()
    } else if raw.contains("404")
        || raw.contains("model not found")
        || raw.contains("not_found")
        || raw.contains("does not exist")
    {
        "The configured AI model wasn't found. Please check the server settings.".into()
    } else if raw.contains("500")
        || raw.contains("502")
        || raw.contains("503")
        || raw.contains("internal server error")
        || raw.contains("service unavailable")
        || raw.contains("bad gateway")
    {
        "The AI service is temporarily unavailable. Please try again shortly.".into()
    } else if raw.contains("timeout")
        || raw.contains("timed out")
        || raw.contains("deadline exceeded")
    {
        "The request to the AI service timed out. Please try again.".into()
    } else if raw.contains("connection")
        || raw.contains("dns")
        || raw.contains("resolve")
        || raw.contains("unreachable")
    {
        "I couldn't reach the AI service. Please check the server's internet connection.".into()
    } else if raw.contains("content filter")
        || raw.contains("safety")
        || raw.contains("blocked")
        || raw.contains("harm_category")
    {
        "The AI service declined to answer that. Try rephrasing your question.".into()
    } else if raw.contains("context length")
        || raw.contains("too long")
        || raw.contains("max tokens")
        || raw.contains("token limit")
    {
        "That conversation got too long for the AI service to handle. Try starting a new one."
            .into()
    } else {
        "Something went wrong while contacting the AI service. Please try again.".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_query_from_reqwest_style_url() {
        let msg = "CompletionError: HttpError: Http client error: error sending request for url (https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?alt=sse&key=AIzaSECRET)";
        let stripped = strip_query_strings(msg);
        assert!(!stripped.contains("AIzaSECRET"));
        assert!(!stripped.contains("alt=sse"));
        assert_eq!(
            stripped,
            "CompletionError: HttpError: Http client error: error sending request for url (https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent)"
        );
    }

    #[test]
    fn passes_through_without_query() {
        let msg = "error sending request for url (https://example.com/path)";
        assert_eq!(strip_query_strings(msg), msg);
    }

    #[test]
    fn resumes_after_whitespace() {
        let msg = "failed https://a.example/x?key=SECRET then retried";
        assert_eq!(
            strip_query_strings(msg),
            "failed https://a.example/x then retried"
        );
    }

    #[test]
    fn question_mark_at_end() {
        assert_eq!(strip_query_strings("did it fail?"), "did it fail");
        assert_eq!(strip_query_strings(""), "");
    }
}
