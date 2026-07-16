//! Chunked authenticated encryption for complete local recovery bundles.
//!
//! This format is intentionally separate from `source_vault`: source vaults
//! accept only small reviewed Markdown trees, while database dumps and image
//! artifacts can be much larger. Recovery keys belong to the offline operator
//! and are never loaded by the game or image worker.

use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Write},
    path::Path,
};

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use manchester_dnd_core::Sha256Digest;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

const RECOVERY_VAULT_MAGIC: &[u8; 16] = b"MA-RECOVERY-VLT\0";
const RECOVERY_VAULT_FORMAT_VERSION: u16 = 1;
const RECOVERY_VAULT_HEADER_BYTES: usize = 16 + 2 + 8 + 8 + 4 + 32 + 16;
const RECOVERY_KEY_BYTES: usize = 32;
const RECOVERY_CHUNK_BYTES: usize = 4 * 1024 * 1024;
const AEAD_TAG_BYTES: u64 = 16;
const RECORD_HEADER_BYTES: u64 = 4;
const MAX_RECOVERY_PLAINTEXT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const RECOVERY_BACKUP_RETENTION_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Error)]
pub enum RecoveryVaultError {
    #[error("the recovery-vault input is invalid: {0}")]
    Invalid(&'static str),
    #[error("the recovery-vault key is invalid")]
    InvalidKey,
    #[error("the recovery vault failed authentication")]
    Authentication,
    #[error("recovery-vault storage failed")]
    Io(#[source] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryVaultReceipt {
    pub schema_version: u16,
    pub vault_id: Sha256Digest,
    pub created_at_epoch: u64,
    pub encrypted_byte_count: u64,
    pub plaintext_byte_count: u64,
    pub plaintext_digest: Sha256Digest,
    pub chunk_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryVaultExpiryReceipt {
    pub schema_version: u16,
    pub vault_id: Sha256Digest,
    pub created_at_epoch: u64,
    pub expired_at_or_before_epoch: u64,
}

#[derive(Debug, Clone)]
struct VaultHeader {
    bytes: Vec<u8>,
    created_at_epoch: u64,
    plaintext_byte_count: u64,
    plaintext_digest: Sha256Digest,
    plaintext_digest_bytes: [u8; 32],
    nonce_prefix: [u8; 16],
}

pub fn create_recovery_key(path: &Path) -> Result<(), RecoveryVaultError> {
    ensure_safe_new_path(path)?;
    let mut key = Zeroizing::new([0_u8; RECOVERY_KEY_BYTES]);
    rand::rng().fill_bytes(key.as_mut());
    write_new_private_file(path, key.as_ref())
}

pub fn seal_recovery_bundle(
    input_path: &Path,
    vault_path: &Path,
    key_path: &Path,
    created_at_epoch: u64,
) -> Result<RecoveryVaultReceipt, RecoveryVaultError> {
    ensure_safe_new_path(vault_path)?;
    let (plaintext_byte_count, plaintext_digest, plaintext_digest_bytes) =
        scan_plaintext(input_path)?;
    let mut nonce_prefix = [0_u8; 16];
    rand::rng().fill_bytes(&mut nonce_prefix);
    let header = build_header(
        created_at_epoch,
        plaintext_byte_count,
        plaintext_digest_bytes,
        nonce_prefix,
    );
    let key = read_key(key_path)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| RecoveryVaultError::InvalidKey)?;
    let mut input = open_regular_file(input_path, "recovery input file")?;
    let mut output = open_new_private_file(vault_path)?;
    let result = (|| {
        output.write_all(&header).map_err(RecoveryVaultError::Io)?;
        let mut buffer = Zeroizing::new(vec![0_u8; RECOVERY_CHUNK_BYTES]);
        let mut remaining = plaintext_byte_count;
        let mut chunk_index = 0_u64;
        let mut verification_digest = Sha256::new();
        while remaining > 0 {
            let expected = usize::try_from(remaining.min(RECOVERY_CHUNK_BYTES as u64))
                .map_err(|_| RecoveryVaultError::Invalid("recovery input size"))?;
            input
                .read_exact(&mut buffer[..expected])
                .map_err(RecoveryVaultError::Io)?;
            verification_digest.update(&buffer[..expected]);
            let plaintext_length = u32::try_from(expected)
                .map_err(|_| RecoveryVaultError::Invalid("recovery chunk size"))?;
            let aad = chunk_aad(&header, chunk_index, plaintext_length);
            let nonce = chunk_nonce(nonce_prefix, chunk_index);
            let ciphertext = cipher
                .encrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &buffer[..expected],
                        aad: &aad,
                    },
                )
                .map_err(|_| RecoveryVaultError::Authentication)?;
            output
                .write_all(&plaintext_length.to_be_bytes())
                .and_then(|_| output.write_all(&ciphertext))
                .map_err(RecoveryVaultError::Io)?;
            remaining -= expected as u64;
            chunk_index = chunk_index
                .checked_add(1)
                .ok_or(RecoveryVaultError::Invalid("recovery chunk count"))?;
        }
        let mut trailing = [0_u8; 1];
        if input.read(&mut trailing).map_err(RecoveryVaultError::Io)? != 0 {
            return Err(RecoveryVaultError::Invalid(
                "recovery input changed while sealing",
            ));
        }
        let verified: [u8; 32] = verification_digest.finalize().into();
        if verified != plaintext_digest_bytes {
            return Err(RecoveryVaultError::Invalid(
                "recovery input changed while sealing",
            ));
        }
        output.sync_all().map_err(RecoveryVaultError::Io)?;
        Ok(chunk_index)
    })();
    let chunk_count = match result {
        Ok(value) => value,
        Err(error) => {
            drop(output);
            let _ = fs::remove_file(vault_path);
            return Err(error);
        }
    };
    drop(output);
    set_private_file_permissions(vault_path)?;
    let (encrypted_byte_count, vault_id) = digest_regular_file(vault_path, "recovery vault")?;
    let expected_maximum = maximum_vault_bytes(plaintext_byte_count)?;
    if encrypted_byte_count > expected_maximum {
        let _ = fs::remove_file(vault_path);
        return Err(RecoveryVaultError::Invalid("recovery vault size"));
    }
    Ok(RecoveryVaultReceipt {
        schema_version: RECOVERY_VAULT_FORMAT_VERSION,
        vault_id,
        created_at_epoch,
        encrypted_byte_count,
        plaintext_byte_count,
        plaintext_digest,
        chunk_count,
    })
}

pub fn inspect_recovery_vault(
    vault_path: &Path,
    key_path: &Path,
) -> Result<RecoveryVaultReceipt, RecoveryVaultError> {
    process_vault(vault_path, key_path, None)
}

pub fn open_recovery_vault(
    vault_path: &Path,
    output_path: &Path,
    key_path: &Path,
) -> Result<RecoveryVaultReceipt, RecoveryVaultError> {
    ensure_safe_new_path(output_path)?;
    let output = open_new_private_file(output_path)?;
    match process_vault(vault_path, key_path, Some(output)) {
        Ok(receipt) => {
            set_private_file_permissions(output_path)?;
            Ok(receipt)
        }
        Err(error) => {
            let _ = fs::remove_file(output_path);
            Err(error)
        }
    }
}

/// Deletes authenticated recovery vaults at the exact 30-day boundary.
/// Authentication prevents a forged cleartext timestamp from causing deletion.
pub fn expire_recovery_vaults(
    backup_root: &Path,
    key_path: &Path,
    now_epoch: u64,
) -> Result<Vec<RecoveryVaultExpiryReceipt>, RecoveryVaultError> {
    require_real_directory(backup_root)?;
    let created_at_or_before_epoch = now_epoch
        .checked_sub(RECOVERY_BACKUP_RETENTION_SECONDS)
        .ok_or(RecoveryVaultError::Invalid("backup expiry clock"))?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(backup_root).map_err(RecoveryVaultError::Io)? {
        let entry = entry.map_err(RecoveryVaultError::Io)?;
        let file_type = entry.file_type().map_err(RecoveryVaultError::Io)?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(RecoveryVaultError::Invalid("unsupported backup entry"));
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("marv") {
            return Err(RecoveryVaultError::Invalid("unexpected backup file"));
        }
        paths.push(path);
    }
    paths.sort();
    let mut expired = Vec::new();
    for path in paths {
        let receipt = inspect_recovery_vault(&path, key_path)?;
        if receipt.created_at_epoch <= created_at_or_before_epoch {
            fs::remove_file(&path).map_err(RecoveryVaultError::Io)?;
            expired.push(RecoveryVaultExpiryReceipt {
                schema_version: RECOVERY_VAULT_FORMAT_VERSION,
                vault_id: receipt.vault_id,
                created_at_epoch: receipt.created_at_epoch,
                expired_at_or_before_epoch: created_at_or_before_epoch,
            });
        }
    }
    Ok(expired)
}

fn process_vault(
    vault_path: &Path,
    key_path: &Path,
    mut output: Option<File>,
) -> Result<RecoveryVaultReceipt, RecoveryVaultError> {
    let metadata = fs::symlink_metadata(vault_path).map_err(RecoveryVaultError::Io)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() <= RECOVERY_VAULT_HEADER_BYTES as u64
    {
        return Err(RecoveryVaultError::Invalid("recovery vault file"));
    }
    let mut vault = File::open(vault_path).map_err(RecoveryVaultError::Io)?;
    let header = read_header(&mut vault)?;
    if metadata.len() > maximum_vault_bytes(header.plaintext_byte_count)? {
        return Err(RecoveryVaultError::Invalid("recovery vault size"));
    }
    let key = read_key(key_path)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| RecoveryVaultError::InvalidKey)?;
    let mut remaining = header.plaintext_byte_count;
    let mut chunk_index = 0_u64;
    let mut hasher = Sha256::new();
    while remaining > 0 {
        let expected = usize::try_from(remaining.min(RECOVERY_CHUNK_BYTES as u64))
            .map_err(|_| RecoveryVaultError::Invalid("recovery chunk size"))?;
        let mut length_bytes = [0_u8; 4];
        read_exact_body(&mut vault, &mut length_bytes)?;
        let plaintext_length = u32::from_be_bytes(length_bytes);
        if usize::try_from(plaintext_length).ok() != Some(expected) {
            return Err(RecoveryVaultError::Invalid("recovery chunk length"));
        }
        let ciphertext_length = expected
            .checked_add(AEAD_TAG_BYTES as usize)
            .ok_or(RecoveryVaultError::Invalid("recovery chunk size"))?;
        let mut ciphertext = vec![0_u8; ciphertext_length];
        read_exact_body(&mut vault, &mut ciphertext)?;
        let aad = chunk_aad(&header.bytes, chunk_index, plaintext_length);
        let nonce = chunk_nonce(header.nonce_prefix, chunk_index);
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| RecoveryVaultError::Authentication)?,
        );
        if plaintext.len() != expected {
            return Err(RecoveryVaultError::Authentication);
        }
        hasher.update(plaintext.as_slice());
        if let Some(file) = output.as_mut() {
            file.write_all(plaintext.as_slice())
                .map_err(RecoveryVaultError::Io)?;
        }
        remaining -= expected as u64;
        chunk_index = chunk_index
            .checked_add(1)
            .ok_or(RecoveryVaultError::Invalid("recovery chunk count"))?;
    }
    let mut trailing = [0_u8; 1];
    if vault.read(&mut trailing).map_err(RecoveryVaultError::Io)? != 0 {
        return Err(RecoveryVaultError::Invalid("recovery vault trailing bytes"));
    }
    let actual_digest_bytes: [u8; 32] = hasher.finalize().into();
    if actual_digest_bytes != header.plaintext_digest_bytes {
        return Err(RecoveryVaultError::Authentication);
    }
    if let Some(file) = output.as_mut() {
        file.sync_all().map_err(RecoveryVaultError::Io)?;
    }
    let (encrypted_byte_count, vault_id) = digest_regular_file(vault_path, "recovery vault")?;
    Ok(RecoveryVaultReceipt {
        schema_version: RECOVERY_VAULT_FORMAT_VERSION,
        vault_id,
        created_at_epoch: header.created_at_epoch,
        encrypted_byte_count,
        plaintext_byte_count: header.plaintext_byte_count,
        plaintext_digest: header.plaintext_digest,
        chunk_count: chunk_index,
    })
}

fn scan_plaintext(input_path: &Path) -> Result<(u64, Sha256Digest, [u8; 32]), RecoveryVaultError> {
    let metadata = fs::symlink_metadata(input_path).map_err(RecoveryVaultError::Io)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_RECOVERY_PLAINTEXT_BYTES
    {
        return Err(RecoveryVaultError::Invalid("recovery input file"));
    }
    let mut file = File::open(input_path).map_err(RecoveryVaultError::Io)?;
    let mut buffer = vec![0_u8; RECOVERY_CHUNK_BYTES];
    let mut hasher = Sha256::new();
    let mut length = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(RecoveryVaultError::Io)?;
        if read == 0 {
            break;
        }
        length = length
            .checked_add(read as u64)
            .ok_or(RecoveryVaultError::Invalid("recovery input size"))?;
        if length > MAX_RECOVERY_PLAINTEXT_BYTES {
            return Err(RecoveryVaultError::Invalid("recovery input size"));
        }
        hasher.update(&buffer[..read]);
    }
    if length == 0 || length != metadata.len() {
        return Err(RecoveryVaultError::Invalid("recovery input size"));
    }
    let digest_bytes: [u8; 32] = hasher.finalize().into();
    Ok((length, Sha256Digest::from_bytes(digest_bytes), digest_bytes))
}

fn build_header(
    created_at_epoch: u64,
    plaintext_byte_count: u64,
    plaintext_digest: [u8; 32],
    nonce_prefix: [u8; 16],
) -> Vec<u8> {
    let mut header = Vec::with_capacity(RECOVERY_VAULT_HEADER_BYTES);
    header.extend_from_slice(RECOVERY_VAULT_MAGIC);
    header.extend_from_slice(&RECOVERY_VAULT_FORMAT_VERSION.to_be_bytes());
    header.extend_from_slice(&created_at_epoch.to_be_bytes());
    header.extend_from_slice(&plaintext_byte_count.to_be_bytes());
    header.extend_from_slice(&(RECOVERY_CHUNK_BYTES as u32).to_be_bytes());
    header.extend_from_slice(&plaintext_digest);
    header.extend_from_slice(&nonce_prefix);
    header
}

fn read_header(file: &mut File) -> Result<VaultHeader, RecoveryVaultError> {
    let mut bytes = vec![0_u8; RECOVERY_VAULT_HEADER_BYTES];
    read_exact_body(file, &mut bytes)?;
    if &bytes[..16] != RECOVERY_VAULT_MAGIC {
        return Err(RecoveryVaultError::Invalid("recovery vault header"));
    }
    let version = u16::from_be_bytes(
        bytes[16..18]
            .try_into()
            .map_err(|_| RecoveryVaultError::Invalid("recovery vault header"))?,
    );
    if version != RECOVERY_VAULT_FORMAT_VERSION {
        return Err(RecoveryVaultError::Invalid("recovery vault version"));
    }
    let created_at_epoch = u64::from_be_bytes(
        bytes[18..26]
            .try_into()
            .map_err(|_| RecoveryVaultError::Invalid("recovery vault header"))?,
    );
    let plaintext_byte_count = u64::from_be_bytes(
        bytes[26..34]
            .try_into()
            .map_err(|_| RecoveryVaultError::Invalid("recovery vault header"))?,
    );
    let chunk_bytes = u32::from_be_bytes(
        bytes[34..38]
            .try_into()
            .map_err(|_| RecoveryVaultError::Invalid("recovery vault header"))?,
    );
    if plaintext_byte_count == 0
        || plaintext_byte_count > MAX_RECOVERY_PLAINTEXT_BYTES
        || chunk_bytes as usize != RECOVERY_CHUNK_BYTES
    {
        return Err(RecoveryVaultError::Invalid("recovery vault header"));
    }
    let plaintext_digest_bytes: [u8; 32] = bytes[38..70]
        .try_into()
        .map_err(|_| RecoveryVaultError::Invalid("recovery vault header"))?;
    let nonce_prefix: [u8; 16] = bytes[70..86]
        .try_into()
        .map_err(|_| RecoveryVaultError::Invalid("recovery vault header"))?;
    Ok(VaultHeader {
        bytes,
        created_at_epoch,
        plaintext_byte_count,
        plaintext_digest: Sha256Digest::from_bytes(plaintext_digest_bytes),
        plaintext_digest_bytes,
        nonce_prefix,
    })
}

fn chunk_aad(header: &[u8], chunk_index: u64, plaintext_length: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header.len() + 12);
    aad.extend_from_slice(header);
    aad.extend_from_slice(&chunk_index.to_be_bytes());
    aad.extend_from_slice(&plaintext_length.to_be_bytes());
    aad
}

fn chunk_nonce(prefix: [u8; 16], chunk_index: u64) -> [u8; 24] {
    let mut nonce = [0_u8; 24];
    nonce[..16].copy_from_slice(&prefix);
    nonce[16..].copy_from_slice(&chunk_index.to_be_bytes());
    nonce
}

fn maximum_vault_bytes(plaintext_bytes: u64) -> Result<u64, RecoveryVaultError> {
    let chunks = plaintext_bytes
        .checked_add(RECOVERY_CHUNK_BYTES as u64 - 1)
        .ok_or(RecoveryVaultError::Invalid("recovery vault size"))?
        / RECOVERY_CHUNK_BYTES as u64;
    (RECOVERY_VAULT_HEADER_BYTES as u64)
        .checked_add(plaintext_bytes)
        .and_then(|value| {
            value.checked_add(chunks.checked_mul(AEAD_TAG_BYTES + RECORD_HEADER_BYTES)?)
        })
        .ok_or(RecoveryVaultError::Invalid("recovery vault size"))
}

fn digest_regular_file(
    path: &Path,
    invalid_reason: &'static str,
) -> Result<(u64, Sha256Digest), RecoveryVaultError> {
    let metadata = fs::symlink_metadata(path).map_err(RecoveryVaultError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RecoveryVaultError::Invalid(invalid_reason));
    }
    let mut file = File::open(path).map_err(RecoveryVaultError::Io)?;
    let mut buffer = vec![0_u8; RECOVERY_CHUNK_BYTES];
    let mut hasher = Sha256::new();
    let mut length = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(RecoveryVaultError::Io)?;
        if read == 0 {
            break;
        }
        length = length
            .checked_add(read as u64)
            .ok_or(RecoveryVaultError::Invalid(invalid_reason))?;
        hasher.update(&buffer[..read]);
    }
    if length != metadata.len() {
        return Err(RecoveryVaultError::Invalid(invalid_reason));
    }
    Ok((length, Sha256Digest::from_bytes(hasher.finalize().into())))
}

fn open_regular_file(path: &Path, reason: &'static str) -> Result<File, RecoveryVaultError> {
    let metadata = fs::symlink_metadata(path).map_err(RecoveryVaultError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RecoveryVaultError::Invalid(reason));
    }
    File::open(path).map_err(RecoveryVaultError::Io)
}

fn read_key(path: &Path) -> Result<Zeroizing<Vec<u8>>, RecoveryVaultError> {
    let metadata = fs::symlink_metadata(path).map_err(RecoveryVaultError::Io)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != RECOVERY_KEY_BYTES as u64
    {
        return Err(RecoveryVaultError::InvalidKey);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(RecoveryVaultError::InvalidKey);
        }
    }
    let mut key = Zeroizing::new(Vec::with_capacity(RECOVERY_KEY_BYTES));
    File::open(path)
        .map_err(RecoveryVaultError::Io)?
        .take((RECOVERY_KEY_BYTES + 1) as u64)
        .read_to_end(&mut key)
        .map_err(RecoveryVaultError::Io)?;
    if key.len() != RECOVERY_KEY_BYTES {
        return Err(RecoveryVaultError::InvalidKey);
    }
    Ok(key)
}

fn read_exact_body(file: &mut File, bytes: &mut [u8]) -> Result<(), RecoveryVaultError> {
    file.read_exact(bytes).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            RecoveryVaultError::Invalid("recovery vault body")
        } else {
            RecoveryVaultError::Io(error)
        }
    })
}

fn require_real_directory(path: &Path) -> Result<(), RecoveryVaultError> {
    let metadata = fs::symlink_metadata(path).map_err(RecoveryVaultError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RecoveryVaultError::Invalid("directory boundary"));
    }
    Ok(())
}

fn ensure_safe_new_path(path: &Path) -> Result<(), RecoveryVaultError> {
    if path.exists() {
        return Err(RecoveryVaultError::Invalid("destination exists"));
    }
    let parent = path
        .parent()
        .ok_or(RecoveryVaultError::Invalid("destination has no parent"))?;
    require_real_directory(parent)
}

fn open_new_private_file(path: &Path) -> Result<File, RecoveryVaultError> {
    ensure_safe_new_path(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(RecoveryVaultError::Io)
}

fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<(), RecoveryVaultError> {
    let mut file = open_new_private_file(path)?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(RecoveryVaultError::Io(error));
    }
    set_private_file_permissions(path)
}

fn set_private_file_permissions(path: &Path) -> Result<(), RecoveryVaultError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(RecoveryVaultError::Io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn chunked_vault_round_trips_tamper_fails_and_expiry_is_exact() {
        let root = tempfile::tempdir().unwrap();
        let backup_root = root.path().join("backups");
        fs::create_dir(&backup_root).unwrap();
        let input = root.path().join("recovery.tar");
        let mut bytes = vec![0x5a; RECOVERY_CHUNK_BYTES + 137];
        let canary = b"RECOVERY_RAW_CANARY_4bc17f8281c";
        bytes[17..17 + canary.len()].copy_from_slice(canary);
        fs::write(&input, &bytes).unwrap();
        let key = root.path().join("recovery.key");
        create_recovery_key(&key).unwrap();
        let vault = backup_root.join("backup.marv");
        let receipt = seal_recovery_bundle(&input, &vault, &key, NOW).unwrap();
        assert_eq!(receipt.chunk_count, 2);
        assert_eq!(receipt.plaintext_byte_count, bytes.len() as u64);
        let encrypted = fs::read(&vault).unwrap();
        assert!(
            !encrypted
                .windows(12)
                .any(|window| window == b"RECOVERY_RAW")
        );
        assert_eq!(inspect_recovery_vault(&vault, &key).unwrap(), receipt);

        let restored = root.path().join("restored.tar");
        assert_eq!(
            open_recovery_vault(&vault, &restored, &key).unwrap(),
            receipt
        );
        assert_eq!(fs::read(restored).unwrap(), bytes);

        assert!(
            expire_recovery_vaults(
                &backup_root,
                &key,
                NOW + RECOVERY_BACKUP_RETENTION_SECONDS - 1
            )
            .unwrap()
            .is_empty()
        );
        let expired =
            expire_recovery_vaults(&backup_root, &key, NOW + RECOVERY_BACKUP_RETENTION_SECONDS)
                .unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].vault_id, receipt.vault_id);
        assert!(!vault.exists());
    }

    #[test]
    fn wrong_key_tampering_symlinks_and_partial_outputs_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("bundle.tar");
        fs::write(&input, b"bounded recovery bundle").unwrap();
        let key = root.path().join("key");
        let wrong_key = root.path().join("wrong-key");
        create_recovery_key(&key).unwrap();
        create_recovery_key(&wrong_key).unwrap();
        let vault = root.path().join("bundle.marv");
        seal_recovery_bundle(&input, &vault, &key, NOW).unwrap();
        assert!(matches!(
            inspect_recovery_vault(&vault, &wrong_key),
            Err(RecoveryVaultError::Authentication)
        ));

        let mut bytes = fs::read(&vault).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::remove_file(&vault).unwrap();
        write_new_private_file(&vault, &bytes).unwrap();
        let output = root.path().join("partial-must-disappear.tar");
        assert!(matches!(
            open_recovery_vault(&vault, &output, &key),
            Err(RecoveryVaultError::Authentication)
        ));
        assert!(!output.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = root.path().join("input-link");
            symlink(&input, &link).unwrap();
            let other = root.path().join("other.marv");
            assert!(matches!(
                seal_recovery_bundle(&link, &other, &key, NOW),
                Err(RecoveryVaultError::Invalid("recovery input file"))
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn key_and_outputs_require_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("bundle.tar");
        fs::write(&input, b"private recovery bytes").unwrap();
        let key = root.path().join("key");
        create_recovery_key(&key).unwrap();
        assert_eq!(
            fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            seal_recovery_bundle(&input, &root.path().join("bad.marv"), &key, NOW),
            Err(RecoveryVaultError::InvalidKey)
        ));
    }
}
