pub trait BytesExt {
    /// These bytes as lowercase hexadecimal.
    fn to_hex(&self) -> String;
}

impl BytesExt for [u8] {
    fn to_hex(&self) -> String {
        self.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_written_as_lowercase_hex() {
        assert_eq!([0x00, 0x0f, 0xa0, 0xff].to_hex(), "000fa0ff");
        assert_eq!([].to_hex(), "");
    }
}
