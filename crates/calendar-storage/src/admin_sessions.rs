//! Independent browser sessions; machine capability tables are never read or modified.
use crate::Storage;
use calendar_application::{SecretBytes, SecurityError};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

pub struct SessionGrant {
    pub session: SecretBytes,
    pub csrf: SecretBytes,
    pub nonce: SecretBytes,
    pub expires_at: i64,
}
fn digest(kind: &str, value: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"CalRelay/admin/v1/");
    hash.update(kind.as_bytes());
    hash.update(b"/");
    hash.update(value.as_bytes());
    format!("{:x}", hash.finalize())
}
fn random() -> Result<String, SecurityError> {
    let mut bytes = zeroize::Zeroizing::new([0; 32]);
    OsRng
        .try_fill_bytes(bytes.as_mut())
        .map_err(|_| SecurityError::Randomness)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Storage {
    /// Atomically consume a verified challenge and issue a session. No external I/O here.
    pub async fn create_admin_session(
        &self,
        identity: &str,
        policy: &str,
        challenge: &str,
        now: i64,
        expires: i64,
    ) -> Result<SessionGrant, SecurityError> {
        if identity.is_empty()
            || policy.is_empty()
            || challenge.is_empty()
            || challenge.len() > 2048
            || now < 0
            || expires <= now
            || expires.saturating_sub(now) > 900
        {
            return Err(SecurityError::InvalidInput);
        }
        let session = random()?;
        let csrf = random()?;
        let nonce = random()?;
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|_| SecurityError::Storage)?;
        sqlx::query("DELETE FROM admin_challenge_uses WHERE expires_at <= ?")
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|_| SecurityError::Storage)?;
        sqlx::query("DELETE FROM admin_sessions WHERE expires_at <= ?")
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|_| SecurityError::Storage)?;
        // Retain challenge digests for the complete provider lifetime, independent of session TTL.
        let inserted = sqlx::query("INSERT INTO admin_challenge_uses(digest,expires_at) VALUES (?,?) ON CONFLICT DO NOTHING")
            .bind(digest("challenge", challenge)).bind(now.checked_add(300).ok_or(SecurityError::InvalidInput)?)
            .execute(&mut *tx).await.map_err(|_| SecurityError::Storage)?;
        if inserted.rows_affected() != 1 {
            return Err(SecurityError::Denied);
        }
        sqlx::query("INSERT INTO admin_sessions(digest,identity,policy,csrf_digest,nonce_digest,created_at,expires_at) VALUES (?,?,?,?,?,?,?)")
            .bind(digest("session", &session)).bind(identity).bind(policy).bind(digest("csrf", &csrf)).bind(digest("nonce", &nonce)).bind(now).bind(expires)
            .execute(&mut *tx).await.map_err(|_| SecurityError::Storage)?;
        tx.commit().await.map_err(|_| SecurityError::Storage)?;
        Ok(SessionGrant {
            session: SecretBytes::new(session.into_bytes()),
            csrf: SecretBytes::new(csrf.into_bytes()),
            nonce: SecretBytes::new(nonce.into_bytes()),
            expires_at: expires,
        })
    }
    pub async fn check_admin_session(
        &self,
        session: &str,
        identity: &str,
        policy: &str,
        now: i64,
    ) -> Result<(), SecurityError> {
        if !valid(session) {
            return Err(SecurityError::Denied);
        }
        let found: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM admin_sessions WHERE digest=? AND identity=? AND policy=? AND created_at<=? AND expires_at>? AND revoked_at IS NULL)")
            .bind(digest("session",session)).bind(identity).bind(policy).bind(now).bind(now).fetch_one(self.pool()).await.map_err(|_| SecurityError::Storage)?;
        if found {
            Ok(())
        } else {
            Err(SecurityError::Denied)
        }
    }
    /// CAS consumes a per-session nonce; exactly one concurrent write can succeed.
    #[allow(clippy::too_many_arguments)] // All predicates participate in the same atomic authorization CAS.
    pub async fn consume_admin_write(
        &self,
        session: &str,
        identity: &str,
        policy: &str,
        csrf: &str,
        nonce: &str,
        now: i64,
        revoke: bool,
    ) -> Result<SecretBytes, SecurityError> {
        if !valid(session) || !valid(csrf) || !valid(nonce) {
            return Err(SecurityError::Denied);
        }
        let next = random()?;
        let result = sqlx::query("UPDATE admin_sessions SET nonce_digest=?, revoked_at=CASE WHEN ? THEN ? ELSE revoked_at END WHERE digest=? AND identity=? AND policy=? AND csrf_digest=? AND nonce_digest=? AND created_at<=? AND expires_at>? AND revoked_at IS NULL")
            .bind(digest("nonce", &next)).bind(revoke).bind(now).bind(digest("session",session)).bind(identity).bind(policy).bind(digest("csrf",csrf)).bind(digest("nonce",nonce)).bind(now).bind(now)
            .execute(self.pool()).await.map_err(|_| SecurityError::Storage)?;
        if result.rows_affected() != 1 {
            return Err(SecurityError::Denied);
        }
        Ok(SecretBytes::new(next.into_bytes()))
    }
}
