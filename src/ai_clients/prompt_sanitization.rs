const MAX_PROMPT_INPUT_CHARS: usize = 500;

/// Sanitize untrusted text before embedding it in LLM prompts.
/// Strips control characters (except newline) and truncates the payload
/// so attacker-controlled strings cannot dominate the prompt body.
pub(crate) fn sanitize_for_llm_prompt(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(MAX_PROMPT_INPUT_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sanitize_for_llm_prompt;

    #[test]
    fn strips_control_chars_and_truncates_untrusted_prompt_text() {
        let input = format!("boom\u{0}{}", "x".repeat(600));
        let sanitized = sanitize_for_llm_prompt(&input);

        assert!(!sanitized.contains('\u{0}'));
        assert_eq!(sanitized.len(), 500);
    }

    #[test]
    fn preserves_newlines_for_prompt_readability() {
        let sanitized = sanitize_for_llm_prompt("line one\nline two");
        assert_eq!(sanitized, "line one\nline two");
    }
}
