use crate::discovery::Product;

/// Recognizes product UI chrome, not agent intent. Call with the current screen only.
/// English/default key hints are deliberately conservative; prose and idle composers
/// are not evidence of an outstanding decision.
pub(crate) fn detect(product: Product, screen: &str) -> Option<&'static str> {
    let screen = screen.trim_end();
    let footer = screen.lines().rev().take(12).find(|line| match product {
        Product::Omp => {
            line.contains(" · ") && (line.contains(" cancel") || line.contains(" deny"))
        }
        Product::Codex => {
            line.contains("to confirm") && line.contains("to cancel")
                || line.contains("to submit answer")
                || line.contains("to submit all")
        }
        Product::OpenCode => {
            line.contains("enter confirm")
                || line.contains("esc dismiss")
                    && (line.contains("enter submit") || line.contains("enter confirm"))
        }
    })?;
    match product {
        Product::Omp => {
            if screen.contains("Allow tool:")
                && footer.contains(" approve · ")
                && footer.contains(" deny")
            {
                return Some("approval prompt detected");
            }
            if screen.contains("Ask")
                && footer.contains(" cancel")
                && (screen.contains("Other (type your own)") || screen.contains("Review answers"))
                && (footer.contains(" select · ")
                    || footer.contains(" toggle · ")
                    || footer.contains(" submit · "))
            {
                return Some("question prompt detected");
            }
        }
        Product::Codex => {
            let approval = [
                "Would you like to run the following command?",
                "Would you like to make the following edits?",
                "Would you like to grant these permissions?",
                "Would you like to send input to ",
                "Do you want to approve network access to ",
            ]
            .iter()
            .any(|title| screen.contains(title));
            if approval
                && footer.contains("to confirm")
                && footer.contains("to cancel")
                && (screen.contains("Yes,") || screen.contains("Yes, proceed"))
            {
                return Some("approval prompt detected");
            }
            if screen.lines().any(|line| {
                let line = line.trim_start_matches(|c: char| c.is_whitespace() || "│┃".contains(c));
                line.starts_with("Question ") && line.contains('/')
            }) && (footer.contains("to submit answer") || footer.contains("to submit all"))
            {
                return Some("question prompt detected");
            }
        }
        Product::OpenCode => {
            if screen.contains("△ Permission required")
                && screen.contains("Allow once")
                && screen.contains("Reject")
                && footer.contains("enter confirm")
            {
                return Some("approval prompt detected");
            }
            // Current OpenCode forms, including questions and MCP elicitation.
            if footer.contains("esc dismiss")
                && (footer.contains("enter submit") || footer.contains("enter confirm"))
                && (footer.contains("↑↓ select") || screen.contains("Submit"))
            {
                return Some("question prompt detected");
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const OMP_APPROVAL: &str = "╭─ Allow tool: bash ─╮\n│ rm -rf build      │\n│ ctrl+enter approve · esc deny · type to steer instead │\n╰───────────────────╯";
    const OMP_ASK: &str = "╭─ Ask ─╮\nWhich database?\n› SQLite\n  Postgres\n  Other (type your own)\nenter select · n note · ↑/↓ move · esc cancel · ctrl+g hide\n╰───────╯";
    const CODEX_APPROVAL: &str = "Would you like to run the following command?\n$ rm -rf build\n› 1. Yes, proceed (y)\n  2. No, and tell Codex what to do differently (esc)\nPress enter to confirm or esc to cancel";
    const CODEX_ASK: &str = "Question 1/2 (2 unanswered)\nWhich database?\n› 1. SQLite\n  2. Postgres\nenter to submit answer | ←/→ to navigate questions | esc to interrupt";
    const OPENCODE_APPROVAL: &str = "△ Permission required\n  # Shell command\n  $ rm -rf build\n Allow once  Always allow  Reject\n⇆ select  enter confirm";
    const OPENCODE_ASK: &str =
        "Choose a database\n› SQLite\n  Postgres\n↑↓ select  enter submit  esc dismiss";

    #[test]
    fn recognizes_approval_and_question_dialogs_for_each_product() {
        for (product, approval, question) in [
            (Product::Omp, OMP_APPROVAL, OMP_ASK),
            (Product::Codex, CODEX_APPROVAL, CODEX_ASK),
            (Product::OpenCode, OPENCODE_APPROVAL, OPENCODE_ASK),
        ] {
            assert_eq!(detect(product, approval), Some("approval prompt detected"));
            assert_eq!(detect(product, question), Some("question prompt detected"));
        }
    }

    #[test]
    fn quiet_prose_questions_and_idle_composers_are_not_input_dialogs() {
        for product in [Product::Omp, Product::Codex, Product::OpenCode] {
            for screen in [
                "",
                "Working… esc to interrupt",
                "Which database would you like?",
                "› Type a message\n? for shortcuts",
                "The log says: Permission required; Allow once or Reject.",
            ] {
                assert_eq!(detect(product, screen), None);
            }
        }
    }

    #[test]
    fn requires_product_specific_chrome_and_recent_footer() {
        assert_eq!(detect(Product::Codex, OMP_APPROVAL), None);
        assert_eq!(detect(Product::Omp, "Allow tool: bash\nWorking…"), None);
        assert_eq!(
            detect(
                Product::OpenCode,
                "△ Permission required\nAllow once Reject"
            ),
            None
        );
        for (product, dialog) in [
            (Product::Omp, OMP_APPROVAL),
            (Product::Codex, CODEX_ASK),
            (Product::OpenCode, OPENCODE_APPROVAL),
        ] {
            let screen = format!("{dialog}\n{}Done.\n› ", "New output\n".repeat(16));
            assert_eq!(detect(product, &screen), None);
        }
    }
}
