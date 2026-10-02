use serde::{Deserialize, Serialize};

use super::{InboundSettings, InvalidEvent, MAX_IDENTIFIER_BYTES};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct Shortcut {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ShortcutError {
    #[error("shortcut must contain 1 to 128 bytes without whitespace or control characters: {0}")]
    InvalidTrigger(String),
    #[error("comment contains more than one configured shortcut")]
    Ambiguous,
}

pub(super) trait TriggerTextExt {
    fn trigger_offsets<'text>(
        &'text self,
        trigger: &'text str,
    ) -> impl Iterator<Item = usize> + 'text;
}

impl TriggerTextExt for str {
    fn trigger_offsets<'text>(
        &'text self,
        trigger: &'text str,
    ) -> impl Iterator<Item = usize> + 'text {
        self.match_indices(trigger).filter_map(move |(offset, _)| {
            let preceding = self[..offset].chars().next_back();
            let following = self[offset.saturating_add(trigger.len())..].chars().next();
            [preceding, following]
                .into_iter()
                .all(|neighbor| {
                    neighbor.is_none_or(|character| {
                        !character.is_alphanumeric() && !matches!(character, '_' | '-' | '/' | '@')
                    })
                })
                .then_some(offset)
        })
    }
}

impl Shortcut {
    pub fn validate(&self) -> Result<(), InvalidEvent> {
        for (field, value) in [
            ("agent", &self.agent),
            ("model", &self.model),
            ("effort", &self.effort),
        ] {
            if let Some(value) = value {
                if value.trim().is_empty() {
                    return Err(InvalidEvent::Empty { field });
                }
                if value.len() > MAX_IDENTIFIER_BYTES {
                    return Err(InvalidEvent::TooLong {
                        field,
                        limit: MAX_IDENTIFIER_BYTES,
                    });
                }
                if value
                    .chars()
                    .any(|character| character.is_control() || character.is_whitespace())
                {
                    return Err(InvalidEvent::InvalidIdentifier { field });
                }
            }
        }
        Ok(())
    }

    pub fn remove_trigger(message: &str, trigger: &str) -> String {
        let offsets: Vec<_> = message.trigger_offsets(trigger).collect();
        let mut prompt = message.to_owned();
        for offset in offsets.into_iter().rev() {
            prompt.replace_range(offset..offset.saturating_add(trigger.len()), "");
        }
        prompt.trim().to_owned()
    }
}

impl InboundSettings {
    pub fn match_shortcut(
        &self,
        message: &str,
    ) -> Result<Option<(&str, &Shortcut)>, ShortcutError> {
        let mut matched = None;
        for (trigger, shortcut) in &self.shortcuts {
            if trigger.is_empty()
                || trigger.len() > 128
                || trigger
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control())
            {
                return Err(ShortcutError::InvalidTrigger(trigger.clone()));
            }
            let found = message.trigger_offsets(trigger).next().is_some();
            if found {
                if matched.is_some() {
                    return Err(ShortcutError::Ambiguous);
                }
                matched = Some((trigger.as_str(), shortcut));
            }
        }
        Ok(matched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_matching_requires_token_boundaries_and_allows_repeated_shortcuts() {
        let settings = InboundSettings::default();
        for message in [
            "/ezra",
            "Please /ezra fix this",
            "(/ezra): help",
            "`/ezra`",
            "> /ezra",
            "/ezra then /ezra",
            "🦦 /ezra",
        ] {
            assert_eq!(
                settings
                    .match_shortcut(message)
                    .expect("match")
                    .expect("found")
                    .0,
                "/ezra"
            );
        }
        for message in [
            "/ezra-other",
            "/ezra_other",
            "/ezra2",
            "project/ezra",
            "https://example.com/ezra",
            "/ezra/path",
            "/EZRA",
            "/ezraé",
        ] {
            assert!(
                settings.match_shortcut(message).expect("match").is_none(),
                "{message}"
            );
        }
    }

    #[test]
    fn removing_triggers_preserves_substrings_and_accepts_empty_requests() {
        assert_eq!(Shortcut::remove_trigger("/ezra", "/ezra"), "");
        assert_eq!(
            Shortcut::remove_trigger("🦦 /ezra fix /ezra-more then /ezra", "/ezra"),
            "🦦  fix /ezra-more then"
        );
        assert_eq!(
            Shortcut::remove_trigger("project/ezra /ezra/path", "/ezra"),
            "project/ezra /ezra/path"
        );
    }

    #[test]
    fn custom_shortcuts_return_their_options_and_reject_ambiguity() {
        let mut settings = InboundSettings::default();
        let options = Shortcut {
            agent: Some("codex".to_owned()),
            model: Some("chosen-model".to_owned()),
            effort: Some("high".to_owned()),
        };
        settings
            .shortcuts
            .insert("/ezra-codex".to_owned(), options.clone());
        assert_eq!(
            settings
                .match_shortcut("Please /ezra-codex investigate")
                .expect("match"),
            Some(("/ezra-codex", &options))
        );
        assert_eq!(
            settings.match_shortcut("/ezra and /ezra-codex"),
            Err(ShortcutError::Ambiguous)
        );
        settings.shortcuts.clear();
        assert!(
            settings
                .match_shortcut("/ezra")
                .expect("disabled shortcuts")
                .is_none()
        );
        settings
            .shortcuts
            .insert("@custom".to_owned(), Shortcut::default());
        assert!(
            settings
                .match_shortcut("@custom help")
                .expect("custom text")
                .is_some()
        );
    }

    #[test]
    fn invalid_configuration_is_reported() {
        for trigger in ["", "two words", "bad\ntrigger", &"x".repeat(129)] {
            let mut settings = InboundSettings::default();
            settings
                .shortcuts
                .insert(trigger.to_owned(), Shortcut::default());
            assert!(matches!(
                settings.match_shortcut("comment"),
                Err(ShortcutError::InvalidTrigger(_))
            ));
        }
    }
}
