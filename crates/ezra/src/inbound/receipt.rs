use std::fmt::Write;

use sha2::{Digest, Sha256};

use super::EventKey;

impl EventKey {
    /// Stable correlation ID for native message submissions and receipt checks.
    pub fn delivery_id(&self) -> String {
        let mut digest = Sha256::new();
        for component in [
            self.conversation.source.as_str(),
            self.conversation.subject.as_str(),
            self.id.as_str(),
        ] {
            let byte_length = u64::try_from(component.len())
                .expect("event identifier lengths fit in a 64-bit length prefix");
            digest.update(byte_length.to_be_bytes());
            digest.update(component.as_bytes());
        }
        let mut delivery_id = String::from("ezra-");
        for byte in digest.finalize() {
            write!(delivery_id, "{byte:02x}").expect("writing to a String cannot fail");
        }
        delivery_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound::ConversationKey;

    impl EventKey {
        fn receipt_example(source: &str, subject: &str, id: &str) -> Self {
            Self {
                conversation: ConversationKey {
                    source: source.to_owned(),
                    subject: subject.to_owned(),
                },
                id: id.to_owned(),
            }
        }
    }

    #[test]
    fn delivery_id_has_a_stable_encoding_across_serialization() {
        let key = EventKey::receipt_example("github:github.com", "repository/123", "comment-456");
        let persisted = serde_json::to_string(&key).expect("key serializes");
        let restored: EventKey = serde_json::from_str(&persisted).expect("key deserializes");
        assert_eq!(key.delivery_id(), restored.delivery_id());
        assert_eq!(
            key.delivery_id(),
            "ezra-b19b5d2348b98191190d8e7903c33ba82e4cda289eb8accbf04d864f4ea0b48f"
        );
    }

    #[test]
    fn each_identity_component_changes_the_delivery_id() {
        let original =
            EventKey::receipt_example("github:github.com", "repository/123", "comment-456");
        for changed in [
            EventKey::receipt_example("github:enterprise.example", "repository/123", "comment-456"),
            EventKey::receipt_example("github:github.com", "repository/124", "comment-456"),
            EventKey::receipt_example("github:github.com", "repository/123", "comment-457"),
        ] {
            assert_ne!(original.delivery_id(), changed.delivery_id());
        }
    }

    #[test]
    fn component_boundaries_are_unambiguous_including_multibyte_identifiers() {
        for (first, second) in [
            (
                EventKey::receipt_example("a", "bc", "d"),
                EventKey::receipt_example("ab", "c", "d"),
            ),
            (
                EventKey::receipt_example("a", "b", "cd"),
                EventKey::receipt_example("a", "bc", "d"),
            ),
            (
                EventKey::receipt_example("a:b", "c", "d"),
                EventKey::receipt_example("a", "b:c", "d"),
            ),
            (
                EventKey::receipt_example("é", "a", "b"),
                EventKey::receipt_example("éa", "b", ""),
            ),
        ] {
            assert_ne!(first.delivery_id(), second.delivery_id());
        }
        let key = EventKey::receipt_example("源", "议题", "消息");
        assert_eq!(key.delivery_id().len(), 69);
        assert!(key.delivery_id().is_ascii());
    }
}
