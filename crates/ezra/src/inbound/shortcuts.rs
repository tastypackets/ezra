use serde::{Deserialize, Serialize};

use super::{InvalidEvent, MAX_IDENTIFIER_BYTES};

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
