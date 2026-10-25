//! Single source configuration contract, shared by management and future runtime consumers.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfigurationVersion {
    #[serde(rename = "v1")]
    V1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceMode {
    Pull,
    ManualStatic,
    PushOnly,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdapterKind {
    Ics,
    StandardCalendar,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCapabilitiesV1 {
    pub snapshot: bool,
    pub delta: bool,
    pub upsert: bool,
    pub delete: bool,
    pub conditional_read: bool,
    pub pagination: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarMappingV1 {
    pub id: String,
    pub external_id: String,
    pub selected: bool,
    pub timezone: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfigurationV1 {
    pub version: ConfigurationVersion,
    pub name: String,
    pub enabled: bool,
    pub adapter: AdapterKind,
    pub mode: SourceMode,
    pub endpoint: Option<String>,
    pub credential_id: Option<String>,
    pub refresh_interval_ms: Option<u64>,
    pub freshness_sla_ms: Option<u64>,
    pub capabilities: SourceCapabilitiesV1,
    pub calendars: Vec<CalendarMappingV1>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfigurationRecord {
    pub source_id: String,
    pub config_revision: i64,
    /// Also fences existing #29 fetch / #23 planning ports.
    pub source_revision: i64,
    pub retired_at: Option<i64>,
    pub configuration: SourceConfigurationV1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationCondition {
    Absent,
    Revision(i64),
}
#[derive(Debug, thiserror::Error)]
pub enum ConfigurationError {
    #[error("invalid source configuration")]
    Invalid,
    #[error("source configuration not found")]
    NotFound,
    #[error("source configuration conflict or retired")]
    Conflict,
    #[error("configuration persistence failed")]
    Storage,
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
impl SourceConfigurationV1 {
    pub fn validate(&self) -> Result<(), ConfigurationError> {
        let c = &self.capabilities;
        let invalid = || ConfigurationError::Invalid;
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
            || self.endpoint.as_ref().is_some_and(|s| s.len() > 4096)
            || self.calendars.is_empty()
            || self.calendars.len() > 256
            || !c.upsert
            || (!c.snapshot && !c.delta)
            || (c.delete && !c.delta)
            || self.credential_id.as_ref().is_some_and(|s| !valid_id(s))
            || self
                .freshness_sla_ms
                .is_some_and(|n| !(60_000..=2_592_000_000).contains(&n))
        {
            return Err(invalid());
        }
        match self.mode {
            SourceMode::Pull => {
                if self.adapter != AdapterKind::Ics
                    || !c.snapshot
                    || c.delta
                    || c.delete
                    || c.pagination
                    || !self
                        .refresh_interval_ms
                        .is_some_and(|n| (60_000..=604_800_000).contains(&n))
                    || self.freshness_sla_ms.is_none()
                {
                    return Err(invalid());
                }
                let url = url::Url::parse(self.endpoint.as_deref().ok_or_else(invalid)?)
                    .map_err(|_| invalid())?;
                // URLs cannot carry credentials/secrets; remote-fetch still enforces DNS/SSRF at use.
                if url.scheme() != "https"
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err(invalid());
                }
            }
            SourceMode::ManualStatic | SourceMode::PushOnly => {
                if self.endpoint.is_some()
                    || self.refresh_interval_ms.is_some()
                    || c.conditional_read
                    || c.pagination
                    || self.credential_id.is_some()
                    || (self.mode == SourceMode::ManualStatic && self.freshness_sla_ms.is_some())
                    || (self.mode == SourceMode::PushOnly
                        && self.adapter != AdapterKind::StandardCalendar)
                {
                    return Err(invalid());
                }
            }
        }
        if self.adapter == AdapterKind::Ics && (!c.snapshot || c.delta || c.delete) {
            return Err(invalid());
        }
        let mut ids = HashSet::new();
        let mut external = HashSet::new();
        for calendar in &self.calendars {
            if !valid_id(&calendar.id)
                || calendar.external_id.is_empty()
                || calendar.external_id.len() > 256
                || calendar.external_id.chars().any(char::is_control)
                || !ids.insert(&calendar.id)
                || !external.insert(&calendar.external_id)
                || calendar
                    .timezone
                    .as_ref()
                    .is_some_and(|s| s.parse::<chrono_tz::Tz>().is_err())
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

/// Bounded discovery; fetch the full configuration by stable source_id after selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfigurationSummary {
    pub source_id: String,
    pub name: String,
    pub enabled: bool,
    pub config_revision: i64,
    pub source_revision: i64,
    pub retired_at: Option<i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfigurationPage {
    pub items: Vec<SourceConfigurationSummary>,
    pub next_after: Option<String>,
}
