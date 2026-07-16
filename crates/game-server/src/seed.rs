use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use manchester_dnd_core::{RollSeed, Sha256Digest, is_valid_opaque_id};
use rand::RngCore as _;
use sha2::{Digest, Sha256};
use thiserror::Error;

const KEY_BYTES: usize = 32;

#[derive(Debug, Error)]
pub enum SeedVaultError {
    #[error("could not {operation} the RNG master key at {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("RNG master key file {path} is unsafe: {reason}")]
    UnsafeFile { path: PathBuf, reason: &'static str },
    #[error("campaign id is invalid for RNG seed derivation")]
    InvalidCampaignId,
}

#[derive(Clone)]
pub struct SeedVault {
    key: [u8; KEY_BYTES],
    key_id: Sha256Digest,
}

impl fmt::Debug for SeedVault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SeedVault")
            .field("key", &"[REDACTED]")
            .field("key_id", &self.key_id)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CampaignSeed {
    seed: RollSeed,
    reference: String,
}

impl fmt::Debug for CampaignSeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CampaignSeed")
            .field("seed", &"[REDACTED]")
            .field("reference", &self.reference)
            .finish()
    }
}

impl CampaignSeed {
    pub const fn expose_to_engine(&self) -> RollSeed {
        self.seed
    }

    pub fn reference(&self) -> &str {
        &self.reference
    }
}

impl SeedVault {
    /// Loads a protected key or atomically creates one for this deployment.
    /// The key is intentionally outside public/static roots and must be backed
    /// up with the database for deterministic campaign replay.
    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self, SeedVaultError> {
        let path = path.as_ref();
        match load_key(path) {
            Ok(key) => Ok(Self::from_key(key)),
            Err(SeedVaultError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                create_key(path).map(Self::from_key)
            }
            Err(error) => Err(error),
        }
    }

    pub fn from_key(key: [u8; KEY_BYTES]) -> Self {
        let key_id = Sha256Digest::from_bytes(Sha256::digest(key).into());
        Self { key, key_id }
    }

    pub fn derive_campaign_seed(&self, campaign_id: &str) -> Result<CampaignSeed, SeedVaultError> {
        if !is_valid_opaque_id(campaign_id) {
            return Err(SeedVaultError::InvalidCampaignId);
        }
        let seed = hmac_sha256(&self.key, b"manchester-arcana/campaign-rng/v1", campaign_id);
        let campaign_hash: [u8; 32] = Sha256::digest(campaign_id.as_bytes()).into();
        let key_hash = self.key_id.as_str().trim_start_matches("sha256:");
        let reference = format!("seed:{}:{}", hex_prefix(&campaign_hash, 8), &key_hash[..16]);
        Ok(CampaignSeed { seed, reference })
    }
}

fn load_key(path: &Path) -> Result<[u8; KEY_BYTES], SeedVaultError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| SeedVaultError::Io {
        operation: "inspect",
        path: path.to_owned(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SeedVaultError::UnsafeFile {
            path: path.to_owned(),
            reason: "it must be a regular file and not a symlink",
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(SeedVaultError::UnsafeFile {
                path: path.to_owned(),
                reason: "group and other permissions must be disabled",
            });
        }
    }

    let mut file = fs::File::open(path).map_err(|source| SeedVaultError::Io {
        operation: "open",
        path: path.to_owned(),
        source,
    })?;
    let mut bytes = Vec::with_capacity(KEY_BYTES + 1);
    file.read_to_end(&mut bytes)
        .map_err(|source| SeedVaultError::Io {
            operation: "read",
            path: path.to_owned(),
            source,
        })?;
    bytes.try_into().map_err(|_| SeedVaultError::UnsafeFile {
        path: path.to_owned(),
        reason: "it must contain exactly 32 random bytes",
    })
}

fn create_key(path: &Path) -> Result<[u8; KEY_BYTES], SeedVaultError> {
    let parent = path.parent().filter(|path| !path.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent).map_err(|source| SeedVaultError::Io {
            operation: "create the parent directory for",
            path: path.to_owned(),
            source,
        })?;
    }

    let mut key = [0_u8; KEY_BYTES];
    rand::rng().fill_bytes(&mut key);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            return load_key(path);
        }
        Err(source) => {
            return Err(SeedVaultError::Io {
                operation: "create",
                path: path.to_owned(),
                source,
            });
        }
    };
    file.write_all(&key)
        .and_then(|_| file.sync_all())
        .map_err(|source| SeedVaultError::Io {
            operation: "write",
            path: path.to_owned(),
            source,
        })?;
    Ok(key)
}

fn hmac_sha256(key: &[u8; KEY_BYTES], domain: &[u8], campaign_id: &str) -> [u8; 32] {
    let mut inner_pad = [0x36_u8; 64];
    let mut outer_pad = [0x5c_u8; 64];
    for (index, byte) in key.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update((domain.len() as u64).to_le_bytes());
    inner.update(domain);
    inner.update((campaign_id.len() as u64).to_le_bytes());
    inner.update(campaign_id.as_bytes());
    let inner: [u8; 32] = inner.finalize().into();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    outer.finalize().into()
}

fn hex_prefix(bytes: &[u8], length: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(length * 2);
    for byte in bytes.iter().take(length) {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_key_roundtrips_with_private_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rng.key");
        let first = SeedVault::load_or_create(&path).unwrap();
        let second = SeedVault::load_or_create(&path).unwrap();
        assert_eq!(
            first.derive_campaign_seed("campaign:one").unwrap(),
            second.derive_campaign_seed("campaign:one").unwrap()
        );
        assert_eq!(fs::read(path).unwrap().len(), KEY_BYTES);
    }

    #[test]
    fn campaign_derivation_is_stable_separated_and_redacted() {
        let vault = SeedVault::from_key([7; KEY_BYTES]);
        let one = vault.derive_campaign_seed("campaign:one").unwrap();
        let replay = vault.derive_campaign_seed("campaign:one").unwrap();
        let two = vault.derive_campaign_seed("campaign:two").unwrap();
        assert_eq!(one, replay);
        assert_ne!(one, two);
        assert!(is_valid_opaque_id(one.reference()));
        assert!(!format!("{vault:?}{one:?}").contains("7, 7"));
        assert!(!format!("{vault:?}{one:?}").contains(&hex_prefix(&[7; 32], 8)));
    }

    #[test]
    fn malformed_key_and_campaign_ids_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rng.key");
        fs::write(&path, b"too short").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(SeedVault::load_or_create(&path).is_err());
        assert!(
            SeedVault::from_key([0; KEY_BYTES])
                .derive_campaign_seed("../bad")
                .is_err()
        );
    }
}
