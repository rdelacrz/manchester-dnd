//! Authenticated encrypted storage for private source trees and backups.
//!
//! Only the offline operator binary uses this module. The ordinary game and
//! image workers consume minimized PostgreSQL projections and never receive a
//! vault key or decrypted source mount.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use manchester_dnd_core::Sha256Digest;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const VAULT_MAGIC: &[u8; 16] = b"MA-SOURCE-VAULT\0";
const VAULT_FORMAT_VERSION: u16 = 1;
const VAULT_PAYLOAD_SCHEMA_VERSION: u16 = 1;
const VAULT_HEADER_BYTES: usize = 16 + 2 + 8 + 24;
const VAULT_KEY_BYTES: usize = 32;
const MAX_VAULT_BYTES: u64 = 96 * 1024 * 1024;
const MAX_PLAINTEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_SOURCE_FILE_BYTES: u64 = 64 * 1024;
const MAX_SOURCE_FILES: usize = 10_000;
const MAX_TREE_ENTRIES: usize = 20_000;
const MAX_TREE_DEPTH: usize = 16;
const MAX_RELATIVE_PATH_BYTES: usize = 512;
pub const SOURCE_BACKUP_RETENTION_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Debug, Error)]
pub enum SourceVaultError {
    #[error("the source-vault input is invalid: {0}")]
    Invalid(&'static str),
    #[error("the source-vault key is invalid")]
    InvalidKey,
    #[error("the source vault failed authentication")]
    Authentication,
    #[error("source-vault storage failed")]
    Io(#[source] std::io::Error),
    #[error("the source-vault payload is invalid")]
    Serialization(#[source] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceVaultReceipt {
    pub schema_version: u16,
    pub vault_id: Sha256Digest,
    pub created_at_epoch: u64,
    pub encrypted_byte_count: u64,
    pub source_file_count: u32,
    pub source_tree_digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceVaultExpiryReceipt {
    pub schema_version: u16,
    pub vault_id: Sha256Digest,
    pub created_at_epoch: u64,
    pub expired_at_or_before_epoch: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultPayloadV1 {
    schema_version: u16,
    files: Vec<VaultFileV1>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultFileV1 {
    relative_path: String,
    content_base64: String,
    content_digest: Sha256Digest,
}

impl Drop for VaultFileV1 {
    fn drop(&mut self) {
        self.relative_path.zeroize();
        self.content_base64.zeroize();
    }
}

#[derive(Debug)]
struct VerifiedVault {
    receipt: SourceVaultReceipt,
    payload: VaultPayloadV1,
}

pub fn create_key(path: &Path) -> Result<(), SourceVaultError> {
    ensure_safe_new_path(path)?;
    let mut key = Zeroizing::new([0_u8; VAULT_KEY_BYTES]);
    rand::rng().fill_bytes(key.as_mut());
    write_new_private_file(path, key.as_ref())
}

pub fn seal_source_tree(
    source_root: &Path,
    vault_path: &Path,
    key_path: &Path,
    created_at_epoch: u64,
) -> Result<SourceVaultReceipt, SourceVaultError> {
    let payload = read_source_tree(source_root)?;
    let source_file_count = u32::try_from(payload.files.len())
        .map_err(|_| SourceVaultError::Invalid("source file count"))?;
    let plaintext =
        Zeroizing::new(serde_json::to_vec(&payload).map_err(SourceVaultError::Serialization)?);
    if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(SourceVaultError::Invalid("source payload size"));
    }
    let source_tree_digest = digest(&plaintext);
    let key = read_key(key_path)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| SourceVaultError::InvalidKey)?;
    let mut nonce_bytes = [0_u8; 24];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let mut header = Vec::with_capacity(VAULT_HEADER_BYTES);
    header.extend_from_slice(VAULT_MAGIC);
    header.extend_from_slice(&VAULT_FORMAT_VERSION.to_be_bytes());
    header.extend_from_slice(&created_at_epoch.to_be_bytes());
    header.extend_from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: &plaintext,
                aad: &header,
            },
        )
        .map_err(|_| SourceVaultError::Authentication)?;
    let total_length = header
        .len()
        .checked_add(ciphertext.len())
        .ok_or(SourceVaultError::Invalid("vault size"))?;
    if total_length as u64 > MAX_VAULT_BYTES {
        return Err(SourceVaultError::Invalid("vault size"));
    }
    let mut vault = Vec::with_capacity(total_length);
    vault.extend_from_slice(&header);
    vault.extend_from_slice(&ciphertext);
    write_new_private_file(vault_path, &vault)?;
    Ok(SourceVaultReceipt {
        schema_version: VAULT_FORMAT_VERSION,
        vault_id: digest(&vault),
        created_at_epoch,
        encrypted_byte_count: vault.len() as u64,
        source_file_count,
        source_tree_digest,
    })
}

pub fn inspect_source_vault(
    vault_path: &Path,
    key_path: &Path,
) -> Result<SourceVaultReceipt, SourceVaultError> {
    Ok(verify_vault(vault_path, key_path)?.receipt)
}

pub fn restore_source_vault(
    vault_path: &Path,
    output_root: &Path,
    key_path: &Path,
) -> Result<SourceVaultReceipt, SourceVaultError> {
    if output_root.exists() {
        return Err(SourceVaultError::Invalid("restore destination exists"));
    }
    let verified = verify_vault(vault_path, key_path)?;
    let parent = output_root.parent().ok_or(SourceVaultError::Invalid(
        "restore destination has no parent",
    ))?;
    require_real_directory(parent)?;
    let temporary = parent.join(format!(".source-vault-restore-{}", Uuid::new_v4()));
    fs::create_dir(&temporary).map_err(SourceVaultError::Io)?;
    set_private_directory_permissions(&temporary)?;
    let restore_result = restore_payload(&temporary, &verified.payload);
    if let Err(error) = restore_result {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, output_root) {
        let _ = fs::remove_dir_all(&temporary);
        return Err(SourceVaultError::Io(error));
    }
    Ok(verified.receipt)
}

/// Deletes authenticated immutable vaults at or before an explicit cutoff.
/// A key is required so a forged timestamp cannot trigger deletion.
pub fn expire_source_vaults(
    backup_root: &Path,
    key_path: &Path,
    now_epoch: u64,
) -> Result<Vec<SourceVaultExpiryReceipt>, SourceVaultError> {
    require_real_directory(backup_root)?;
    let created_at_or_before_epoch = now_epoch
        .checked_sub(SOURCE_BACKUP_RETENTION_SECONDS)
        .ok_or(SourceVaultError::Invalid("backup expiry clock"))?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(backup_root).map_err(SourceVaultError::Io)? {
        let entry = entry.map_err(SourceVaultError::Io)?;
        let file_type = entry.file_type().map_err(SourceVaultError::Io)?;
        if file_type.is_symlink() || (!file_type.is_file() && !file_type.is_dir()) {
            return Err(SourceVaultError::Invalid("unsupported backup entry"));
        }
        if file_type.is_dir() {
            return Err(SourceVaultError::Invalid("nested backup directory"));
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("mavlt") {
            return Err(SourceVaultError::Invalid("unexpected backup file"));
        }
        paths.push(path);
    }
    paths.sort();
    let mut expired = Vec::new();
    for path in paths {
        let verified = verify_vault(&path, key_path)?;
        if verified.receipt.created_at_epoch <= created_at_or_before_epoch {
            fs::remove_file(&path).map_err(SourceVaultError::Io)?;
            expired.push(SourceVaultExpiryReceipt {
                schema_version: VAULT_FORMAT_VERSION,
                vault_id: verified.receipt.vault_id,
                created_at_epoch: verified.receipt.created_at_epoch,
                expired_at_or_before_epoch: created_at_or_before_epoch,
            });
        }
    }
    Ok(expired)
}

fn read_source_tree(root: &Path) -> Result<VaultPayloadV1, SourceVaultError> {
    require_real_directory(root)?;
    let canonical_root = fs::canonicalize(root).map_err(SourceVaultError::Io)?;
    let mut directories = vec![(canonical_root.clone(), 0_usize)];
    let mut paths = Vec::new();
    let mut entry_count = 0_usize;
    while let Some((directory, depth)) = directories.pop() {
        for entry in fs::read_dir(&directory).map_err(SourceVaultError::Io)? {
            entry_count = entry_count
                .checked_add(1)
                .ok_or(SourceVaultError::Invalid("source entry count"))?;
            if entry_count > MAX_TREE_ENTRIES {
                return Err(SourceVaultError::Invalid("source entry count"));
            }
            let entry = entry.map_err(SourceVaultError::Io)?;
            let file_type = entry.file_type().map_err(SourceVaultError::Io)?;
            let path = entry.path();
            if file_type.is_symlink() {
                return Err(SourceVaultError::Invalid("source symlink"));
            }
            if file_type.is_dir() {
                if depth >= MAX_TREE_DEPTH {
                    return Err(SourceVaultError::Invalid("source tree depth"));
                }
                directories.push((path, depth + 1));
            } else if file_type.is_file() {
                if path.extension().and_then(|value| value.to_str()) != Some("md") {
                    return Err(SourceVaultError::Invalid("source file extension"));
                }
                paths.push(path);
            } else {
                return Err(SourceVaultError::Invalid("unsupported source entry"));
            }
        }
    }
    if paths.is_empty() || paths.len() > MAX_SOURCE_FILES {
        return Err(SourceVaultError::Invalid("source file count"));
    }
    paths.sort();
    let mut files = Vec::with_capacity(paths.len());
    let mut total_bytes = 0_usize;
    for path in paths {
        let canonical = fs::canonicalize(&path).map_err(SourceVaultError::Io)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(SourceVaultError::Invalid("source path escaped root"));
        }
        let relative = canonical
            .strip_prefix(&canonical_root)
            .map_err(|_| SourceVaultError::Invalid("source relative path"))?;
        let relative_path = normalized_relative_path(relative)?;
        let metadata = fs::symlink_metadata(&canonical).map_err(SourceVaultError::Io)?;
        if !metadata.is_file() || metadata.len() > MAX_SOURCE_FILE_BYTES {
            return Err(SourceVaultError::Invalid("source file size"));
        }
        let mut file = File::open(&canonical).map_err(SourceVaultError::Io)?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        Read::by_ref(&mut file)
            .take(MAX_SOURCE_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(SourceVaultError::Io)?;
        if bytes.len() as u64 > MAX_SOURCE_FILE_BYTES {
            return Err(SourceVaultError::Invalid("source file size"));
        }
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .ok_or(SourceVaultError::Invalid("source payload size"))?;
        if total_bytes > MAX_PLAINTEXT_BYTES / 2 {
            return Err(SourceVaultError::Invalid("source payload size"));
        }
        files.push(VaultFileV1 {
            relative_path,
            content_digest: digest(&bytes),
            content_base64: BASE64_STANDARD.encode(bytes),
        });
    }
    Ok(VaultPayloadV1 {
        schema_version: VAULT_PAYLOAD_SCHEMA_VERSION,
        files,
    })
}

fn verify_vault(vault_path: &Path, key_path: &Path) -> Result<VerifiedVault, SourceVaultError> {
    let metadata = fs::symlink_metadata(vault_path).map_err(SourceVaultError::Io)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() <= VAULT_HEADER_BYTES as u64
        || metadata.len() > MAX_VAULT_BYTES
    {
        return Err(SourceVaultError::Invalid("vault file"));
    }
    let mut vault = Vec::with_capacity(metadata.len() as usize);
    File::open(vault_path)
        .map_err(SourceVaultError::Io)?
        .take(MAX_VAULT_BYTES + 1)
        .read_to_end(&mut vault)
        .map_err(SourceVaultError::Io)?;
    if vault.len() as u64 != metadata.len() || &vault[..16] != VAULT_MAGIC {
        return Err(SourceVaultError::Invalid("vault header"));
    }
    let version = u16::from_be_bytes(
        vault[16..18]
            .try_into()
            .map_err(|_| SourceVaultError::Invalid("vault header"))?,
    );
    if version != VAULT_FORMAT_VERSION {
        return Err(SourceVaultError::Invalid("vault version"));
    }
    let created_at_epoch = u64::from_be_bytes(
        vault[18..26]
            .try_into()
            .map_err(|_| SourceVaultError::Invalid("vault header"))?,
    );
    let nonce = XNonce::from_slice(&vault[26..VAULT_HEADER_BYTES]);
    let key = read_key(key_path)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| SourceVaultError::InvalidKey)?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &vault[VAULT_HEADER_BYTES..],
                    aad: &vault[..VAULT_HEADER_BYTES],
                },
            )
            .map_err(|_| SourceVaultError::Authentication)?,
    );
    if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(SourceVaultError::Invalid("vault payload size"));
    }
    let payload: VaultPayloadV1 =
        serde_json::from_slice(&plaintext).map_err(SourceVaultError::Serialization)?;
    validate_payload(&payload)?;
    Ok(VerifiedVault {
        receipt: SourceVaultReceipt {
            schema_version: version,
            vault_id: digest(&vault),
            created_at_epoch,
            encrypted_byte_count: vault.len() as u64,
            source_file_count: u32::try_from(payload.files.len())
                .map_err(|_| SourceVaultError::Invalid("source file count"))?,
            source_tree_digest: digest(&plaintext),
        },
        payload,
    })
}

fn validate_payload(payload: &VaultPayloadV1) -> Result<(), SourceVaultError> {
    if payload.schema_version != VAULT_PAYLOAD_SCHEMA_VERSION
        || payload.files.is_empty()
        || payload.files.len() > MAX_SOURCE_FILES
    {
        return Err(SourceVaultError::Invalid("vault payload shape"));
    }
    let mut previous: Option<&str> = None;
    let mut total = 0_usize;
    for file in &payload.files {
        validate_relative_path_string(&file.relative_path)?;
        if previous.is_some_and(|value| value >= file.relative_path.as_str()) {
            return Err(SourceVaultError::Invalid("vault path order"));
        }
        previous = Some(&file.relative_path);
        let bytes = Zeroizing::new(
            BASE64_STANDARD
                .decode(&file.content_base64)
                .map_err(|_| SourceVaultError::Invalid("vault file encoding"))?,
        );
        if bytes.len() as u64 > MAX_SOURCE_FILE_BYTES || digest(&bytes) != file.content_digest {
            return Err(SourceVaultError::Invalid("vault file digest"));
        }
        total = total
            .checked_add(bytes.len())
            .ok_or(SourceVaultError::Invalid("vault payload size"))?;
        if total > MAX_PLAINTEXT_BYTES / 2 {
            return Err(SourceVaultError::Invalid("vault payload size"));
        }
    }
    Ok(())
}

fn restore_payload(root: &Path, payload: &VaultPayloadV1) -> Result<(), SourceVaultError> {
    for file in &payload.files {
        validate_relative_path_string(&file.relative_path)?;
        let destination = root.join(&file.relative_path);
        let parent = destination
            .parent()
            .ok_or(SourceVaultError::Invalid("vault destination"))?;
        fs::create_dir_all(parent).map_err(SourceVaultError::Io)?;
        let relative_parent = parent
            .strip_prefix(root)
            .map_err(|_| SourceVaultError::Invalid("vault destination"))?;
        let mut cursor = root.to_path_buf();
        for component in relative_parent.components() {
            let Component::Normal(component) = component else {
                return Err(SourceVaultError::Invalid("vault destination"));
            };
            cursor.push(component);
            set_private_directory_permissions(&cursor)?;
        }
        let bytes = Zeroizing::new(
            BASE64_STANDARD
                .decode(&file.content_base64)
                .map_err(|_| SourceVaultError::Invalid("vault file encoding"))?,
        );
        if digest(&bytes) != file.content_digest {
            return Err(SourceVaultError::Invalid("vault file digest"));
        }
        write_new_private_file(&destination, &bytes)?;
    }
    Ok(())
}

fn read_key(path: &Path) -> Result<Zeroizing<Vec<u8>>, SourceVaultError> {
    let metadata = fs::symlink_metadata(path).map_err(SourceVaultError::Io)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != VAULT_KEY_BYTES as u64
    {
        return Err(SourceVaultError::InvalidKey);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(SourceVaultError::InvalidKey);
        }
    }
    let mut key = Zeroizing::new(Vec::with_capacity(VAULT_KEY_BYTES));
    File::open(path)
        .map_err(SourceVaultError::Io)?
        .take((VAULT_KEY_BYTES + 1) as u64)
        .read_to_end(&mut key)
        .map_err(SourceVaultError::Io)?;
    if key.len() != VAULT_KEY_BYTES {
        return Err(SourceVaultError::InvalidKey);
    }
    Ok(key)
}

fn normalized_relative_path(path: &Path) -> Result<String, SourceVaultError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(SourceVaultError::Invalid("source relative path"));
        };
        let value = value
            .to_str()
            .ok_or(SourceVaultError::Invalid("source path encoding"))?;
        if value.is_empty() || value.contains(['/', '\\']) || value.chars().any(char::is_control) {
            return Err(SourceVaultError::Invalid("source relative path"));
        }
        parts.push(value);
    }
    let value = parts.join("/");
    validate_relative_path_string(&value)?;
    Ok(value)
}

fn validate_relative_path_string(value: &str) -> Result<(), SourceVaultError> {
    if value.is_empty()
        || value.len() > MAX_RELATIVE_PATH_BYTES
        || value.starts_with('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || value.split('/').any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
        || !value.ends_with(".md")
    {
        return Err(SourceVaultError::Invalid("vault relative path"));
    }
    Ok(())
}

fn require_real_directory(path: &Path) -> Result<(), SourceVaultError> {
    let metadata = fs::symlink_metadata(path).map_err(SourceVaultError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SourceVaultError::Invalid("directory boundary"));
    }
    Ok(())
}

fn ensure_safe_new_path(path: &Path) -> Result<(), SourceVaultError> {
    if path.exists() {
        return Err(SourceVaultError::Invalid("destination exists"));
    }
    let parent = path
        .parent()
        .ok_or(SourceVaultError::Invalid("destination has no parent"))?;
    require_real_directory(parent)
}

fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<(), SourceVaultError> {
    ensure_safe_new_path(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(SourceVaultError::Io)?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(SourceVaultError::Io(error));
    }
    set_private_file_permissions(path)
}

fn set_private_directory_permissions(path: &Path) -> Result<(), SourceVaultError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(SourceVaultError::Io)?;
    }
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> Result<(), SourceVaultError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(SourceVaultError::Io)?;
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn authenticated_vault_hides_restores_and_expires_source_bytes() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let backups = root.path().join("backups");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&backups).unwrap();
        set_private_directory_permissions(&source).unwrap();
        set_private_directory_permissions(&backups).unwrap();
        let canary = "VAULT_RAW_CANARY_7d34f0d447d948d5";
        fs::write(
            source.join("private.md"),
            format!("---\n{{\"schema_version\":1}}\n---\n{canary}\n"),
        )
        .unwrap();
        let key = root.path().join("source-vault.key");
        create_key(&key).unwrap();
        let vault = backups.join("backup-1.mavlt");
        let receipt = seal_source_tree(&source, &vault, &key, NOW).unwrap();
        let encrypted = fs::read(&vault).unwrap();
        assert!(
            !encrypted
                .windows(canary.len())
                .any(|window| window == canary.as_bytes())
        );
        assert!(!encrypted.windows(10).any(|window| window == b"private.md"));
        assert_eq!(inspect_source_vault(&vault, &key).unwrap(), receipt);

        let wrong_key = root.path().join("wrong.key");
        create_key(&wrong_key).unwrap();
        assert!(matches!(
            inspect_source_vault(&vault, &wrong_key),
            Err(SourceVaultError::Authentication)
        ));

        let restored = root.path().join("restored");
        assert_eq!(
            restore_source_vault(&vault, &restored, &key).unwrap(),
            receipt
        );
        assert_eq!(
            fs::read_to_string(restored.join("private.md")).unwrap(),
            fs::read_to_string(source.join("private.md")).unwrap()
        );

        assert!(
            expire_source_vaults(&backups, &key, NOW + SOURCE_BACKUP_RETENTION_SECONDS - 1)
                .unwrap()
                .is_empty()
        );
        let expired =
            expire_source_vaults(&backups, &key, NOW + SOURCE_BACKUP_RETENTION_SECONDS).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].vault_id, receipt.vault_id);
        assert!(!vault.exists());
    }

    #[test]
    fn vault_rejects_symlinks_tampering_and_existing_restore_destinations() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let backups = root.path().join("backups");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&backups).unwrap();
        fs::write(source.join("one.md"), "bounded private source").unwrap();
        let key = root.path().join("key");
        create_key(&key).unwrap();
        let vault = backups.join("one.mavlt");
        seal_source_tree(&source, &vault, &key, NOW).unwrap();
        let mut bytes = fs::read(&vault).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::remove_file(&vault).unwrap();
        write_new_private_file(&vault, &bytes).unwrap();
        assert!(matches!(
            inspect_source_vault(&vault, &key),
            Err(SourceVaultError::Authentication)
        ));

        let existing = root.path().join("existing");
        fs::create_dir(&existing).unwrap();
        assert!(matches!(
            restore_source_vault(&vault, &existing, &key),
            Err(SourceVaultError::Invalid("restore destination exists"))
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let external = root.path().join("outside.md");
            fs::write(&external, "outside").unwrap();
            symlink(&external, source.join("escape.md")).unwrap();
            let other = backups.join("other.mavlt");
            assert!(matches!(
                seal_source_tree(&source, &other, &key, NOW),
                Err(SourceVaultError::Invalid("source symlink"))
            ));
        }
    }
}
