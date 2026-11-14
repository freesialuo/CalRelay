//! Read-only view of management changes across every audit table (#196, #218). Only
//! identifiers, revisions, actions, times and a short actor digest prefix leave storage.
use crate::Storage;
use calendar_application::configuration::ConfigurationError as Error;

/// Entries returned per read; older entries are reached through `next_before`.
pub const AUDIT_PAGE: i64 = 100;
/// Every audit kind, in the tie-break order used within one millisecond.
pub const AUDIT_KINDS: [&str; 6] = [
    "deletion_approval",
    "output",
    "producer_token",
    "rule",
    "source",
    "subscription",
];

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
    /// Write order within the entry's table; only used for the cursor.
    seq: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditPage {
    pub items: Vec<AuditEntry>,
    /// Opaque cursor for the next older page; `None` on the last page.
    pub next_before: Option<String>,
}

/// The total order key `(at DESC, kind ASC, seq DESC)`, written as `<at>.<kind>.<seq>`.
struct Cursor<'a> {
    at: i64,
    kind: &'a str,
    seq: i64,
}
impl<'a> Cursor<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let mut parts = text.split('.');
        let (at, kind, seq) = (parts.next()?, parts.next()?, parts.next()?);
        let number = |s: &str| {
            let n: i64 = s.parse().ok()?;
            (n.to_string() == s).then_some(n)
        };
        let cursor = Self {
            at: number(at).filter(|n| *n >= 0)?,
            kind: AUDIT_KINDS.into_iter().find(|k| *k == kind)?,
            seq: number(seq).filter(|n| *n > 0)?,
        };
        parts.next().is_none().then_some(cursor)
    }
}

impl Storage {
    /// Newest first; `before` continues after a previous page and `kind` keeps one kind.
    pub async fn audit_page(
        &self,
        before: Option<&str>,
        kind: Option<&str>,
    ) -> Result<AuditPage, Error> {
        let cursor = before
            .map(|b| Cursor::parse(b).ok_or(Error::Invalid))
            .transpose()?;
        if kind.is_some_and(|k| !AUDIT_KINDS.contains(&k)) {
            return Err(Error::Invalid);
        }
        let (at, ckind, seq) = cursor.map_or((None, None, None), |c| {
            (Some(c.at), Some(c.kind), Some(c.seq))
        });
        let mut items: Vec<AuditEntry> = sqlx::query_as(concat!(
            "SELECT kind,subject_id,source_id,revision,action,substr(actor_digest,1,12) AS actor,at,seq FROM (",
            "SELECT 'source' AS kind,source_id AS subject_id,NULL AS source_id,config_revision AS revision,action,actor_digest,changed_at AS at,rowid AS seq FROM configuration_audit ",
            "UNION ALL SELECT entity_kind,entity_id,NULL,generation,action,actor_digest,changed_at,rowid FROM reminder_configuration_audit ",
            "UNION ALL SELECT 'subscription',subscription_id,NULL,revision,action,actor_digest,changed_at,rowid FROM aggregate_subscription_audit ",
            "UNION ALL SELECT 'producer_token',token_id,source_id,NULL,action,actor_digest,changed_at,rowid FROM producer_token_audit ",
            "UNION ALL SELECT 'deletion_approval',source_id,NULL,source_revision,'approve',actor_digest,approved_at,rowid FROM deletion_approval_audit",
            ") WHERE (?1 IS NULL OR kind=?1) AND (?2 IS NULL OR at<?2 OR (at=?2 AND (kind>?3 OR (kind=?3 AND seq<?4)))) ",
            "ORDER BY at DESC,kind ASC,seq DESC LIMIT ?5"
        ))
        .bind(kind)
        .bind(at)
        .bind(ckind)
        .bind(seq)
        .bind(AUDIT_PAGE + 1)
        .fetch_all(self.pool())
        .await
        .map_err(|_| Error::Storage)?;
        let more = items.len() as i64 > AUDIT_PAGE;
        items.truncate(AUDIT_PAGE as usize);
        let next_before = more
            .then(|| items.last())
            .flatten()
            .map(|e| format!("{}.{}.{}", e.at, e.kind, e.seq));
        Ok(AuditPage { items, next_before })
    }
}
