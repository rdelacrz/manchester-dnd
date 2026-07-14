/// Maximum length shared by durable and cross-boundary opaque identifiers.
pub const MAX_OPAQUE_ID_LEN: usize = 128;

/// IDs are deliberately ASCII and URL/log friendly. Display names and prose
/// have separate fields and must never be smuggled into identifiers.
pub fn is_valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OPAQUE_ID_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_ids_have_one_shared_boundary() {
        assert!(is_valid_opaque_id("campaign:rain-1"));
        assert!(is_valid_opaque_id(&"a".repeat(MAX_OPAQUE_ID_LEN)));
        assert!(!is_valid_opaque_id(&"a".repeat(MAX_OPAQUE_ID_LEN + 1)));
        assert!(!is_valid_opaque_id("contains spaces"));
        assert!(!is_valid_opaque_id("../escape"));
    }
}
