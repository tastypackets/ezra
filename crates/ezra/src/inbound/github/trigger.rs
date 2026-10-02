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
