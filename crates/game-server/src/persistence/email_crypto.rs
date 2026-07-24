use std::{collections::HashMap, fmt, sync::Arc};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use hmac::{Hmac, Mac};
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use zeroize::Zeroize;

use crate::config::AuthenticationConfig;

const ALGORITHM: &str = "xchacha20poly1305";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailCiphertext {
    pub algorithm: String,
    pub key_id: String,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
}

impl fmt::Debug for EmailCiphertext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmailCiphertext")
            .field("algorithm", &self.algorithm)
            .field("key_id", &self.key_id)
            .field("nonce_b64", &"[REDACTED]")
            .field("ciphertext_b64", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum EmailCryptoError {
    #[error("email failed canonical validation")]
    InvalidEmail,
    #[error("email encryption key configuration is invalid")]
    InvalidEncryptionKey,
    #[error("email lookup key configuration is invalid")]
    InvalidLookupKey,
    #[error("email encryption key identifier is invalid")]
    InvalidKeyId,
    #[error("email encryption failed")]
    Encryption,
    #[error("email decryption failed")]
    Decryption,
    #[error("email ciphertext is invalid")]
    InvalidCiphertext,
    #[error("cryptographic randomness source failed")]
    Randomness,
}

struct EmailDataKey([u8; 32]);

impl Drop for EmailDataKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

struct EmailLookupKey(Vec<u8>);

impl Drop for EmailLookupKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone)]
pub struct EmailCrypto {
    current_key_id: Arc<str>,
    encryption_keys: Arc<HashMap<String, EmailDataKey>>,
    lookup_key: Arc<EmailLookupKey>,
}

impl fmt::Debug for EmailCrypto {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmailCrypto")
            .field("current_key_id", &self.current_key_id)
            .field("encryption_keys", &"[REDACTED]")
            .field("lookup_key", &"[REDACTED]")
            .finish()
    }
}

impl EmailCrypto {
    pub fn from_config(config: &AuthenticationConfig) -> Result<Self, EmailCryptoError> {
        Self::new(
            config.email_encryption_key_id.clone(),
            config.email_encryption_key.expose_secret(),
            config.email_lookup_hmac_key.expose_secret().as_bytes(),
        )
    }

    pub fn new(
        current_key_id: String,
        current_key_b64: &str,
        lookup_key: &[u8],
    ) -> Result<Self, EmailCryptoError> {
        validate_key_id(&current_key_id)?;
        let current_key = decode_data_key(current_key_b64)?;
        if lookup_key.len() < 32 {
            return Err(EmailCryptoError::InvalidLookupKey);
        }
        let mut encryption_keys = HashMap::new();
        encryption_keys.insert(current_key_id.clone(), EmailDataKey(current_key));
        Ok(Self {
            current_key_id: Arc::from(current_key_id),
            encryption_keys: Arc::new(encryption_keys),
            lookup_key: Arc::new(EmailLookupKey(lookup_key.to_vec())),
        })
    }

    /// Returns a new key ring retaining the current write key and adding one
    /// bounded decryption-only key for rotation.
    pub fn with_decryption_key(
        &self,
        key_id: String,
        key_b64: &str,
    ) -> Result<Self, EmailCryptoError> {
        validate_key_id(&key_id)?;
        let key = decode_data_key(key_b64)?;
        let mut encryption_keys = HashMap::new();
        for (existing_id, existing_key) in self.encryption_keys.iter() {
            encryption_keys.insert(existing_id.clone(), EmailDataKey(existing_key.0));
        }
        encryption_keys.insert(key_id, EmailDataKey(key));
        Ok(Self {
            current_key_id: self.current_key_id.clone(),
            encryption_keys: Arc::new(encryption_keys),
            lookup_key: self.lookup_key.clone(),
        })
    }

    pub fn current_key_id(&self) -> &str {
        &self.current_key_id
    }

    pub fn normalize(raw: &str) -> Result<String, EmailCryptoError> {
        let normalized = raw.trim().to_lowercase();
        let valid = (3..=320).contains(&normalized.len())
            && !normalized.chars().any(char::is_whitespace)
            && normalized.matches('@').count() == 1
            && normalized.split('@').all(|part| !part.is_empty());
        valid
            .then_some(normalized)
            .ok_or(EmailCryptoError::InvalidEmail)
    }

    pub fn lookup_hmac(&self, normalized_email: &str) -> Result<String, EmailCryptoError> {
        if Self::normalize(normalized_email)? != normalized_email {
            return Err(EmailCryptoError::InvalidEmail);
        }
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.lookup_key.0)
            .map_err(|_| EmailCryptoError::InvalidLookupKey)?;
        mac.update(normalized_email.as_bytes());
        Ok(format!(
            "hmac-sha256:{}",
            encode_hex(&mac.finalize().into_bytes())
        ))
    }

    pub fn verify_lookup_hmac(
        &self,
        normalized_email: &str,
        expected: &str,
    ) -> Result<bool, EmailCryptoError> {
        if Self::normalize(normalized_email)? != normalized_email {
            return Err(EmailCryptoError::InvalidEmail);
        }
        let Some(expected) = expected.strip_prefix("hmac-sha256:") else {
            return Ok(false);
        };
        let Some(expected) = decode_hex(expected) else {
            return Ok(false);
        };
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.lookup_key.0)
            .map_err(|_| EmailCryptoError::InvalidLookupKey)?;
        mac.update(normalized_email.as_bytes());
        Ok(mac.verify_slice(&expected).is_ok())
    }

    pub fn encrypt(
        &self,
        account_id: &str,
        schema_version: u32,
        normalized_email: &str,
    ) -> Result<EmailCiphertext, EmailCryptoError> {
        if Self::normalize(normalized_email)? != normalized_email {
            return Err(EmailCryptoError::InvalidEmail);
        }
        let key = self
            .encryption_keys
            .get(self.current_key_id.as_ref())
            .ok_or(EmailCryptoError::InvalidEncryptionKey)?;
        let mut nonce = [0_u8; 24];
        rand::rngs::OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| EmailCryptoError::Randomness)?;
        let cipher = XChaCha20Poly1305::new((&key.0).into());
        let aad = aad(account_id, schema_version, &self.current_key_id);
        let encrypted = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: normalized_email.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| EmailCryptoError::Encryption)?;
        let output = EmailCiphertext {
            algorithm: ALGORITHM.to_owned(),
            key_id: self.current_key_id.to_string(),
            nonce_b64: STANDARD_NO_PAD.encode(nonce),
            ciphertext_b64: STANDARD_NO_PAD.encode(encrypted),
        };
        nonce.zeroize();
        Ok(output)
    }

    pub fn decrypt(
        &self,
        account_id: &str,
        schema_version: u32,
        encrypted: &EmailCiphertext,
    ) -> Result<String, EmailCryptoError> {
        if encrypted.algorithm != ALGORITHM {
            return Err(EmailCryptoError::InvalidCiphertext);
        }
        let key = self
            .encryption_keys
            .get(&encrypted.key_id)
            .ok_or(EmailCryptoError::Decryption)?;
        let nonce = STANDARD_NO_PAD
            .decode(&encrypted.nonce_b64)
            .map_err(|_| EmailCryptoError::InvalidCiphertext)?;
        let nonce: [u8; 24] = nonce
            .try_into()
            .map_err(|_| EmailCryptoError::InvalidCiphertext)?;
        let ciphertext = STANDARD_NO_PAD
            .decode(&encrypted.ciphertext_b64)
            .map_err(|_| EmailCryptoError::InvalidCiphertext)?;
        let cipher = XChaCha20Poly1305::new((&key.0).into());
        let aad = aad(account_id, schema_version, &encrypted.key_id);
        let plaintext = cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| EmailCryptoError::Decryption)?;
        let result = String::from_utf8(plaintext).map_err(|_| EmailCryptoError::Decryption)?;
        if Self::normalize(&result)? != result {
            return Err(EmailCryptoError::Decryption);
        }
        Ok(result)
    }
}

pub fn validate_encryption_key_b64(value: &str) -> Result<(), EmailCryptoError> {
    decode_data_key(value).map(|mut key| key.zeroize())
}

pub fn validate_lookup_key(value: &[u8]) -> Result<(), EmailCryptoError> {
    if value.len() >= 32 {
        Ok(())
    } else {
        Err(EmailCryptoError::InvalidLookupKey)
    }
}

fn decode_data_key(value: &str) -> Result<[u8; 32], EmailCryptoError> {
    let decoded = STANDARD
        .decode(value)
        .map_err(|_| EmailCryptoError::InvalidEncryptionKey)?;
    decoded
        .try_into()
        .map_err(|_| EmailCryptoError::InvalidEncryptionKey)
}

fn validate_key_id(value: &str) -> Result<(), EmailCryptoError> {
    let valid = (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(EmailCryptoError::InvalidKeyId)
    }
}

fn aad(account_id: &str, schema_version: u32, key_id: &str) -> Vec<u8> {
    format!(
        "{}:{account_id}{}:{schema_version}{}:{key_id}",
        account_id.len(),
        schema_version.to_string().len(),
        key_id.len()
    )
    .into_bytes()
}

fn encode_hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|chunk| {
            let text = std::str::from_utf8(chunk).ok()?;
            u8::from_str_radix(text, 16).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const KEY_B: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
    const LOOKUP: &[u8] = b"unit-test-email-lookup-key-32-bytes-minimum";

    #[test]
    fn randomized_ciphertext_has_stable_lookup_and_round_trips() {
        let crypto = EmailCrypto::new("email-key:test".to_owned(), KEY_A, LOOKUP).unwrap();
        let normalized = EmailCrypto::normalize(" Player@Example.Test ").unwrap();
        let first = crypto.encrypt("account:test", 1, &normalized).unwrap();
        let second = crypto.encrypt("account:test", 1, &normalized).unwrap();

        assert_ne!(first, second);
        assert_eq!(
            crypto.lookup_hmac(&normalized).unwrap(),
            crypto.lookup_hmac(&normalized).unwrap()
        );
        assert!(
            crypto
                .verify_lookup_hmac(&normalized, &crypto.lookup_hmac(&normalized).unwrap())
                .unwrap()
        );
        assert_eq!(
            crypto.decrypt("account:test", 1, &first).unwrap(),
            normalized
        );
    }

    #[test]
    fn wrong_key_and_aad_are_rejected_without_secret_debug_output() {
        let crypto = EmailCrypto::new("email-key:test".to_owned(), KEY_A, LOOKUP).unwrap();
        let wrong = EmailCrypto::new("email-key:test".to_owned(), KEY_B, LOOKUP).unwrap();
        let encrypted = crypto
            .encrypt("account:test", 1, "player@example.test")
            .unwrap();

        assert!(wrong.decrypt("account:test", 1, &encrypted).is_err());
        assert!(crypto.decrypt("account:other", 1, &encrypted).is_err());
        assert!(crypto.decrypt("account:test", 2, &encrypted).is_err());
        assert!(!format!("{crypto:?}").contains(KEY_A));
        assert!(!format!("{encrypted:?}").contains(&encrypted.ciphertext_b64));
        assert!(
            !EmailCryptoError::Decryption
                .to_string()
                .contains("player@example.test")
        );
    }

    #[test]
    fn rotation_key_ring_can_read_old_ciphertext() {
        let old = EmailCrypto::new("email-key:old".to_owned(), KEY_A, LOOKUP).unwrap();
        let encrypted = old
            .encrypt("account:test", 1, "player@example.test")
            .unwrap();
        let current = EmailCrypto::new("email-key:new".to_owned(), KEY_B, LOOKUP)
            .unwrap()
            .with_decryption_key("email-key:old".to_owned(), KEY_A)
            .unwrap();
        assert_eq!(
            current.decrypt("account:test", 1, &encrypted).unwrap(),
            "player@example.test"
        );
        assert_eq!(current.current_key_id(), "email-key:new");
    }
}
