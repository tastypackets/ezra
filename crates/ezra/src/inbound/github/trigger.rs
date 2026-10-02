use crate::inbound::Shortcut;
use crate::inbound::shortcuts::TriggerTextExt;

pub(super) struct TriggerRequest {
    pub message: String,
    pub new_chat: bool,
}

impl TriggerRequest {
    pub fn parse(message: &str, trigger: &str) -> Self {
        let mut options = Vec::new();
        for offset in message.trigger_offsets(trigger) {
            let following = &message[offset.saturating_add(trigger.len())..];
            let option = following.trim_start_matches([' ', '\t']);
            if option.len() == following.len() {
                continue;
            }
            if let Some(remainder) = option.strip_prefix("--new")
                && remainder.chars().next().is_none_or(char::is_whitespace)
            {
                let start = message.len().saturating_sub(option.len());
                options.push(start..start.saturating_add("--new".len()));
            }
        }
        let new_chat = !options.is_empty();
        let mut prompt = message.to_owned();
        for option in options.into_iter().rev() {
            prompt.replace_range(option, "");
        }
        Self {
            message: Shortcut::remove_trigger(&prompt, trigger),
            new_chat,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_chat_is_a_leading_option_for_any_configured_trigger() {
        for trigger in ["/ezra", "/my-agent", "@custom"] {
            for message in [
                format!("{trigger} --new"),
                format!("Please {trigger}\t--new fix this"),
            ] {
                let request = TriggerRequest::parse(&message, trigger);
                assert!(request.new_chat);
                assert!(!request.message.contains("--new"));
                assert!(!request.message.contains(trigger));
            }
        }
        let request = TriggerRequest::parse("/ezra --new fix this then /ezra --new", "/ezra");
        assert_eq!(request.message, "fix this then");
        assert!(request.new_chat);
    }

    #[test]
    fn prose_and_similar_literals_do_not_reset_the_chat() {
        for message in [
            "/ezra explain --new",
            "/ezra \"--new\"",
            "/ezra --newer",
            "/ezra --new-file",
            "/ezra\n--new",
            "project/ezra --new /ezra explain",
            "/ezra-other --new /ezra explain",
        ] {
            let request = TriggerRequest::parse(message, "/ezra");
            assert!(!request.new_chat, "{message}");
            assert!(request.message.contains("--new"), "{message}");
        }
        let request = TriggerRequest::parse("/ezra --new explain the --new option", "/ezra");
        assert!(request.new_chat);
        assert_eq!(request.message, "explain the --new option");
    }
}
