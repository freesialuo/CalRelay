//! Read-only view of recent management changes across every audit table (#196). Only
//! identifiers, revisions, actions, times and a short actor digest prefix leave storage.
use crate::Storage;
use calendar_application::configuration::ConfigurationError as Error;

/// Entries returned per read; older entries are reported through `more`.
pub const AUDIT_PAGE: i64 = 100;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AuditEntry {
    /// `source`, `rule`, `output`, `subscription`, `producer_token` or `deletion_approval`.
    pub kind: String,
    pub subject_id: String,
    /// The owning source of a producer token.
    pub source_id: Option<String>,
    pub revision: Option<i64>,
    pub action: String,
    /// First 12 hex digits of the administrator digest; `None` for system changes.
    pub actor: Option<String>,
    pub at: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditPage {
    pub items: Vec<AuditEntry>,
    pub more: bool,
}

impl Storage {
    pub async fn recent_audit(&self) -> Result<AuditPage, Error> {
        let mut items: Vec<AuditEntry> = sqlx::query_as(concat!(
            "SELECT kind,subject_id,source_id,revision,action,substr(actor_digest,1,12) AS actor,at FROM (",
            "SELECT 'source' AS kind,source_id AS subject_id,NULL AS source_id,config_revision AS revision,action,actor_digest,changed_at AS at,rowid AS seq FROM configuration_audit ",
            "UNION ALL SELECT entity_kind,entity_id,NULL,generation,action,actor_digest,changed_at,rowid FROM reminder_configuration_audit ",
            "UNION ALL SELECT 'subscription',subscription_id,NULL,revision,action,actor_digest,changed_at,rowid FROM aggregate_subscription_audit ",
            "UNION ALL SELECT 'producer_token',token_id,source_id,NULL,action,actor_digest,changed_at,rowid FROM producer_token_audit ",
            "UNION ALL SELECT 'deletion_approval',source_id,NULL,source_revision,'approve',actor_digest,approved_at,rowid FROM deletion_approval_audit",
            ") ORDER BY at DESC,revision DESC,seq DESC LIMIT ?"
        ))
        .bind(AUDIT_PAGE + 1)
        .fetch_all(self.pool())
        .await
        .map_err(|_| Error::Storage)?;
        let more = items.len() as i64 > AUDIT_PAGE;
        items.truncate(AUDIT_PAGE as usize);
        Ok(AuditPage { items, more })
    }
}
