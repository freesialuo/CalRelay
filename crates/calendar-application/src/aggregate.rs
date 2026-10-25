//! Managed aggregate policy and read port. No SQL, HTTP, credentials or renderer here.
use crate::{configuration::valid_id, VisibleField};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, future::Future};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateSource {
    pub source_id: String,
    pub calendar_ids: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateStalePolicy {
    Reject,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateConfiguration {
    pub version: u8,
    pub name: String,
    pub enabled: bool,
    pub sources: Vec<AggregateSource>,
    pub visible_fields: Vec<VisibleField>,
    pub stale_policy: AggregateStalePolicy,
    #[serde(deserialize_with = "required_optional")]
    pub expires_at: Option<i64>,
}
fn required_optional<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Option::<i64>::deserialize(d)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AggregateError {
    #[error("invalid aggregate configuration")]
    Invalid,
    #[error("aggregate not found")]
    NotFound,
    #[error("aggregate revision conflict or retired")]
    Conflict,
    #[error("aggregate access denied")]
    Denied,
    #[error("aggregate data unavailable or budget exceeded")]
    Unavailable,
    #[error("aggregate persistence failed")]
    Storage,
}
impl AggregateConfiguration {
    pub fn validate(&self) -> Result<(), AggregateError> {
        if self.version != 1
            || self.name.trim().is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
            || self.sources.len() > 32
            || self.visible_fields.len() > 5
            || self.expires_at.is_some_and(|v| v < 0)
            || self
                .visible_fields
                .iter()
                .enumerate()
                .any(|(i, v)| self.visible_fields[..i].contains(v))
        {
            return Err(AggregateError::Invalid);
        }
        let mut sources = HashSet::new();
        let mut calendars = 0usize;
        for source in &self.sources {
            if !valid_id(&source.source_id)
                || !sources.insert(&source.source_id)
                || source.calendar_ids.len() > 256
            {
                return Err(AggregateError::Invalid);
            }
            let mut ids = HashSet::new();
            for id in &source.calendar_ids {
                if !valid_id(id) || !ids.insert(id) {
                    return Err(AggregateError::Invalid);
                }
            }
            calendars = calendars
                .checked_add(source.calendar_ids.len())
                .ok_or(AggregateError::Invalid)?;
            if calendars > 256 {
                return Err(AggregateError::Invalid);
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateRecord {
    pub subscription_id: String,
    pub public_id: String,
    pub revision: i64,
    pub revoked_at: Option<i64>,
    pub configuration: AggregateConfiguration,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateSummary {
    pub subscription_id: String,
    pub public_id: String,
    pub revision: i64,
    pub name: String,
    pub enabled: bool,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregatePage {
    pub items: Vec<AggregateSummary>,
    pub next_after: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateSourceFence {
    pub source_id: String,
    pub configuration_revision: i64,
    pub data_revision: i64,
    pub configuration_digest: [u8; 32],
    pub calendar_ids: Vec<String>,
    pub snapshot_revision: Option<i64>,
    pub last_verified_at: Option<i64>,
    pub fresh_until: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateKey {
    pub subscription_id: String,
    pub revision: i64,
    pub policy_digest: [u8; 32],
    pub sources: Vec<AggregateSourceFence>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AggregateAuthorization {
    pub key: AggregateKey,
    pub configuration: AggregateConfiguration,
}
pub struct AggregateEvent {
    pub event_id: String,
    pub revision: i64,
    pub changed_at: i64,
    pub deleted: bool,
    pub document_json: String,
}
pub struct AggregateSnapshot {
    pub authorization: AggregateAuthorization,
    pub events: Vec<AggregateEvent>,
}
pub trait AggregateReadRepository: Send + Sync {
    fn authorize_aggregate(
        &self,
        public_id: &str,
        secret: &[u8],
        now: i64,
    ) -> impl Future<Output = Result<AggregateAuthorization, AggregateError>> + Send;
    fn aggregate_snapshot(
        &self,
        public_id: &str,
        secret: &[u8],
        now: i64,
    ) -> impl Future<Output = Result<AggregateSnapshot, AggregateError>> + Send;
}
