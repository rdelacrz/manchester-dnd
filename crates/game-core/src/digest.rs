use std::fmt::{self, Write as _};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::{GameCoreError, Result};

/// A canonical lowercase SHA-256 identifier suitable for durable provenance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(GameCoreError::InvalidSha256Digest);
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(GameCoreError::InvalidSha256Digest);
        }
        Ok(Self(value))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        let mut value = String::with_capacity(71);
        value.push_str("sha256:");
        for byte in bytes {
            write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_requires_canonical_prefixed_lowercase_hex() {
        let valid = format!("sha256:{}", "a".repeat(64));
        assert_eq!(Sha256Digest::new(&valid).unwrap().as_str(), valid);
        assert!(Sha256Digest::new("sha256:example").is_err());
        assert!(Sha256Digest::new(format!("sha256:{}", "A".repeat(64))).is_err());
    }

    #[test]
    fn digest_round_trips_as_a_json_string() {
        let digest = Sha256Digest::from_bytes([0xab; 32]);
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(serde_json::from_str::<Sha256Digest>(&json).unwrap(), digest);
    }
}
