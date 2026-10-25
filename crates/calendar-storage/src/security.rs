//! AEAD credentials and purpose-separated machine capabilities. No HTTP/admin auth here.
use std::{collections::BTreeMap, io::Read, path::PathBuf};

use calendar_application::{CredentialProvider, SecretBytes, SecurityError};
use chacha20poly1305::{
    aead::{Aead, Payload},
    KeyInit, XChaCha20Poly1305, XNonce,
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::Storage;

const MAX_SECRET: usize = 65_536;

/// Loaded from independently mounted read-only secret files; never persisted or logged.
/// Retain old versions until all credentials have been rotated and backups reconciled.
pub struct KeyRing {
    current: i64,
    keys: BTreeMap<i64, Zeroizing<[u8; 32]>>,
}
impl std::fmt::Debug for KeyRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyRing([REDACTED])")
    }
}
impl KeyRing {
    /// Each file contains exactly 32 raw bytes (no hex/base64 or trailing newline).
    pub fn from_files(current: i64, files: &BTreeMap<i64, PathBuf>) -> Result<Self, SecurityError> {
        if current <= 0 || !files.contains_key(&current) || files.keys().any(|v| *v <= 0) {
            return Err(SecurityError::InvalidInput);
        }
        let mut keys = BTreeMap::new();
        for (version, path) in files {
            let file = std::fs::File::open(path).map_err(|_| SecurityError::KeyFile)?;
            if !file
                .metadata()
                .map_err(|_| SecurityError::KeyFile)?
                .is_file()
            {
                return Err(SecurityError::KeyFile);
            }
            let mut bytes = Zeroizing::new(Vec::new());
            file.take(33)
                .read_to_end(&mut bytes)
                .map_err(|_| SecurityError::KeyFile)?;
            if bytes.len() != 32 {
                return Err(SecurityError::KeyFile);
            }
            let mut key = Zeroizing::new([0u8; 32]);
            key.copy_from_slice(&bytes);
            keys.insert(*version, key);
        }
        Ok(Self { current, keys })
    }
    fn cipher(&self, version: i64) -> Result<XChaCha20Poly1305, SecurityError> {
        let key = self.keys.get(&version).ok_or(SecurityError::MissingKey)?;
        XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| SecurityError::MissingKey)
    }
    fn encrypt(
        &self,
        id: &str,
        kind: &str,
        secret: &SecretBytes,
    ) -> Result<(Vec<u8>, Vec<u8>), SecurityError> {
        let mut nonce = [0u8; 24];
        random(&mut nonce)?;
        let aad = aad(id, kind, self.current);
        let ciphertext = self
            .cipher(self.current)?
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: secret.expose(),
                    aad: &aad,
                },
            )
            .map_err(|_| SecurityError::Authentication)?;
        Ok((ciphertext, nonce.to_vec()))
    }
    fn decrypt(&self, id: &str, row: &CredentialRow) -> Result<SecretBytes, SecurityError> {
        if row.nonce.len() != 24
            || row.ciphertext.len() < 17
            || row.ciphertext.len() > MAX_SECRET + 16
        {
            return Err(SecurityError::Authentication);
        }
        let aad = aad(id, &row.kind, row.key_version);
        let clear = self
            .cipher(row.key_version)?
            .decrypt(
                XNonce::from_slice(&row.nonce),
                Payload {
                    msg: &row.ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| SecurityError::Authentication)?;
        Ok(SecretBytes::new(clear))
    }
}
fn aad(id: &str, kind: &str, version: i64) -> Vec<u8> {
    // Length-delimited canonical tuple avoids ambiguous concatenation.
    serde_json::to_vec(&("CalRelay/credential/v1", id, kind, version)).expect("serializable tuple")
}
fn random(bytes: &mut [u8]) -> Result<(), SecurityError> {
    OsRng
        .try_fill_bytes(bytes)
        .map_err(|_| SecurityError::Randomness)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256
}
fn storage_error(_: sqlx::Error) -> SecurityError {
    SecurityError::Storage
}

#[derive(sqlx::FromRow)]
struct CredentialRow {
    kind: String,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
    key_version: i64,
    revision: i64,
}

/// Trusted repository boundary; handles come from validated source/output configuration.
pub struct SecurityStore<'a> {
    pub storage: &'a Storage,
    pub keys: &'a KeyRing,
}
impl CredentialProvider for SecurityStore<'_> {
    async fn credential(
        &self,
        id: &str,
        expected_kind: &str,
    ) -> Result<SecretBytes, SecurityError> {
        let row:CredentialRow=sqlx::query_as("SELECT kind,ciphertext,nonce,key_version,revision FROM credentials WHERE id=? AND kind=?")
            .bind(id).bind(expected_kind).fetch_optional(self.storage.pool()).await.map_err(storage_error)?.ok_or(SecurityError::Denied)?;
        self.keys.decrypt(id, &row)
    }
}
impl SecurityStore<'_> {
    pub async fn create_credential(
        &self,
        id: &str,
        kind: &str,
        secret: &SecretBytes,
        now: i64,
    ) -> Result<(), SecurityError> {
        if !valid_id(id)
            || kind.is_empty()
            || kind.len() > 64
            || secret.expose().is_empty()
            || secret.expose().len() > MAX_SECRET
        {
            return Err(SecurityError::InvalidInput);
        }
        let (ciphertext, nonce) = self.keys.encrypt(id, kind, secret)?;
        sqlx::query("INSERT INTO credentials(id,kind,ciphertext,nonce,key_version,rotated_at) VALUES (?,?,?,?,?,?)")
            .bind(id).bind(kind).bind(ciphertext).bind(nonce).bind(self.keys.current).bind(now).execute(self.storage.pool()).await.map_err(storage_error)?;
        Ok(())
    }
    /// Re-encrypt one credential atomically. Retain old keys until every row is rotated.
    pub async fn rotate_credential(
        &self,
        id: &str,
        expected_revision: i64,
        now: i64,
    ) -> Result<(), SecurityError> {
        self.update_credential(id, expected_revision, None, now)
            .await
    }
    /// Replace an external credential value using the current key and a fresh nonce.
    pub async fn replace_credential(
        &self,
        id: &str,
        expected_revision: i64,
        secret: &SecretBytes,
        now: i64,
    ) -> Result<(), SecurityError> {
        self.update_credential(id, expected_revision, Some(secret), now)
            .await
    }
    async fn update_credential(
        &self,
        id: &str,
        expected_revision: i64,
        replacement: Option<&SecretBytes>,
        now: i64,
    ) -> Result<(), SecurityError> {
        if replacement.is_some_and(|s| s.expose().is_empty() || s.expose().len() > MAX_SECRET) {
            return Err(SecurityError::InvalidInput);
        }
        let row: CredentialRow = sqlx::query_as(
            "SELECT kind,ciphertext,nonce,key_version,revision FROM credentials WHERE id=?",
        )
        .bind(id)
        .fetch_optional(self.storage.pool())
        .await
        .map_err(storage_error)?
        .ok_or(SecurityError::Denied)?;
        if row.revision != expected_revision || expected_revision == i64::MAX {
            return Err(SecurityError::Conflict);
        }
        // A supplied replacement can recover an unusable credential without decrypting it.
        let clear;
        let secret = match replacement {
            Some(s) => s,
            None => {
                clear = self.keys.decrypt(id, &row)?;
                &clear
            }
        };
        let (ciphertext, nonce) = self.keys.encrypt(id, &row.kind, secret)?;
        let updated=sqlx::query("UPDATE credentials SET ciphertext=?,nonce=?,key_version=?,revision=revision+1,rotated_at=? WHERE id=? AND revision=? AND ciphertext=? AND nonce=? AND kind=? AND key_version=?")
            .bind(ciphertext).bind(nonce).bind(self.keys.current).bind(now).bind(id).bind(expected_revision)
            .bind(row.ciphertext).bind(row.nonce).bind(row.kind).bind(row.key_version)
            .execute(self.storage.pool()).await.map_err(storage_error)?;
        if updated.rows_affected() != 1 {
            return Err(SecurityError::Conflict);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiAction {
    Upsert,
    Delete,
}

fn digest(purpose: &str, secret: &[u8]) -> Result<Vec<u8>, SecurityError> {
    if secret.len() != 64
        || !secret
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err(SecurityError::Denied);
    }
    let mut hash = Sha256::new();
    hash.update(b"CalRelay/token/v1/");
    hash.update(purpose.as_bytes());
    hash.update([0]);
    hash.update(secret);
    Ok(hash.finalize().to_vec())
}
fn issue(purpose: &str) -> Result<(SecretBytes, Vec<u8>), SecurityError> {
    let mut raw = Zeroizing::new([0u8; 32]);
    random(raw.as_mut())?;
    let mut hex = Zeroizing::new(Vec::with_capacity(64));
    for b in raw.iter() {
        hex.push(b"0123456789abcdef"[(b >> 4) as usize]);
        hex.push(b"0123456789abcdef"[(b & 15) as usize]);
    }
    let hash = digest(purpose, &hex)?;
    Ok((SecretBytes::new(std::mem::take(&mut *hex)), hash))
}
impl Storage {
    /// Generates a 256-bit capability. Return it once; never recoverable from the database.
    pub async fn issue_api_token(
        &self,
        id: &str,
        source: &str,
        upsert: bool,
        delete: bool,
        expires: Option<i64>,
    ) -> Result<SecretBytes, SecurityError> {
        if !valid_id(id) || !valid_id(source) || (!upsert && !delete) {
            return Err(SecurityError::InvalidInput);
        }
        let (secret, hash) = issue("api")?;
        sqlx::query("INSERT INTO api_tokens(id,token_digest,source_id,can_upsert,can_delete,expires_at) VALUES (?,?,?,?,?,?)")
            .bind(id).bind(hash).bind(source).bind(upsert).bind(delete).bind(expires).execute(self.pool()).await.map_err(storage_error)?;
        Ok(secret)
    }
    pub async fn authenticate_api_token(
        &self,
        secret: &[u8],
        source: &str,
        action: ApiAction,
        now: i64,
    ) -> Result<String, SecurityError> {
        let hash = digest("api", secret)?;
        sqlx::query_scalar("SELECT t.id FROM api_tokens t JOIN sources s ON s.id=t.source_id WHERE t.token_digest=? AND t.source_id=? AND t.revoked_at IS NULL AND (t.expires_at IS NULL OR t.expires_at>?) AND s.enabled=1 AND ((?=1 AND t.can_upsert=1) OR (?=1 AND t.can_delete=1))")
            .bind(hash).bind(source).bind(now).bind(action==ApiAction::Upsert).bind(action==ApiAction::Delete)
            .fetch_optional(self.pool()).await.map_err(storage_error)?.ok_or(SecurityError::Denied)
    }
    /// Source-wide read capability. Rendering, filtering and cache authorization belong to #32.
    pub async fn issue_feed_token(
        &self,
        id: &str,
        public_id: &str,
        source: &str,
        expires: Option<i64>,
    ) -> Result<SecretBytes, SecurityError> {
        if !valid_id(id) || !valid_id(public_id) || !valid_id(source) {
            return Err(SecurityError::InvalidInput);
        }
        let (secret, hash) = issue("feed")?;
        sqlx::query("INSERT INTO feed_subscriptions(id,public_id,source_id,token_digest,expires_at) VALUES (?,?,?,?,?)")
            .bind(id).bind(public_id).bind(source).bind(hash).bind(expires).execute(self.pool()).await.map_err(storage_error)?;
        Ok(secret)
    }
    pub async fn authenticate_feed_token(
        &self,
        public_id: &str,
        secret: &[u8],
        now: i64,
    ) -> Result<String, SecurityError> {
        let hash = digest("feed", secret)?;
        sqlx::query_scalar("SELECT f.source_id FROM feed_subscriptions f JOIN sources s ON s.id=f.source_id WHERE f.public_id=? AND f.token_digest=? AND f.revoked_at IS NULL AND (f.expires_at IS NULL OR f.expires_at>?) AND s.enabled=1 AND f.scope_json='{}'")
            .bind(public_id).bind(hash).bind(now).fetch_optional(self.pool()).await.map_err(storage_error)?.ok_or(SecurityError::Denied)
    }
    pub async fn revoke_api_token(&self, id: &str, now: i64) -> Result<(), SecurityError> {
        let r = sqlx::query("UPDATE api_tokens SET revoked_at=coalesce(revoked_at,?) WHERE id=?")
            .bind(now)
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(storage_error)?;
        if r.rows_affected() != 1 {
            return Err(SecurityError::Denied);
        }
        Ok(())
    }
    /// CAS rotation preserves expiry and read scope. Revoked subscriptions cannot be revived.
    pub async fn rotate_feed_token(
        &self,
        id: &str,
        expected_revision: i64,
    ) -> Result<SecretBytes, SecurityError> {
        if expected_revision < 0 || expected_revision == i64::MAX {
            return Err(SecurityError::InvalidInput);
        }
        let (secret, hash) = issue("feed")?;
        let r=sqlx::query("UPDATE feed_subscriptions SET token_digest=?,revision=revision+1 WHERE id=? AND revision=? AND revoked_at IS NULL")
            .bind(hash).bind(id).bind(expected_revision).execute(self.pool()).await.map_err(storage_error)?;
        if r.rows_affected() != 1 {
            return Err(SecurityError::Conflict);
        }
        Ok(secret)
    }
    pub async fn revoke_feed_token(&self, id: &str, now: i64) -> Result<(), SecurityError> {
        let r = sqlx::query(
            "UPDATE feed_subscriptions SET revoked_at=coalesce(revoked_at,?) WHERE id=?",
        )
        .bind(now)
        .bind(id)
        .execute(self.pool())
        .await
        .map_err(storage_error)?;
        if r.rows_affected() != 1 {
            return Err(SecurityError::Denied);
        }
        Ok(())
    }
}
