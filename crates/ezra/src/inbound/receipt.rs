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
