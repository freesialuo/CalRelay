//! ICS and constrained JSON-pointer mapping, independently of SQLite.
use crate::ics::{normalize_ics, IcsLimits};
use calendar_application::{
    IngestionError as E, RemoteAuthentication, RemoteFetchOutcome, RemoteFetchRequest,
    RemoteFetcher, RemoteValidators, SourceAdapter, SourcePage, SourcePageOutcome, SourceState,
    SyncRecord, MAX_BATCH_RECORDS,
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonMapping {
    /// JSON Pointer to the page's item array.
    pub items: String,
    /// EventV1 top-level field -> item JSON Pointer; no scripts/conversions.
    pub fields: BTreeMap<String, String>,
    /// Required pointer: a string next URL or explicit null marking a complete result.
    pub next: String,
}
pub enum SourceFormat {
    Ics(IcsLimits),
    Json(JsonMapping),
}
pub struct HttpSource<F> {
    pub fetcher: Arc<F>,
    pub url: String,
    pub authentication: RemoteAuthentication,
    pub calendar_id: String,
    pub format: SourceFormat,
}
#[derive(Default, serde::Deserialize, serde::Serialize)]
struct Checkpoint {
    #[serde(default)]
    binding: String,
    etag: Option<String>,
    last_modified: Option<String>,
}
impl<F: RemoteFetcher> SourceAdapter for HttpSource<F> {
    async fn page(
        &self,
        state: &SourceState,
        cursor: Option<&str>,
        now: i64,
    ) -> Result<SourcePageOutcome, E> {
        let base = reqwest::Url::parse(&self.url).map_err(|_| E::Adapter)?;
        let url = match cursor {
            Some(cursor) => base.join(cursor).map_err(|_| E::Adapter)?,
            None => base.clone(),
        };
        if url.origin() != base.origin()
            || url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(E::Adapter);
        }
        if matches!(self.format, SourceFormat::Ics(_)) && cursor.is_some() {
            return Err(E::InvalidSnapshot);
        }
        use sha2::{Digest, Sha256};
        let auth = match &self.authentication {
            RemoteAuthentication::None => ("none", ""),
            RemoteAuthentication::Basic { credential_id } => ("basic", credential_id.as_str()),
            RemoteAuthentication::Bearer { credential_id } => ("bearer", credential_id.as_str()),
        };
        let format = match &self.format {
            SourceFormat::Ics(l) => serde_json::json!([
                "ics",
                l.max_bytes,
                l.max_properties,
                l.max_events,
                l.max_parse_steps
            ]),
            SourceFormat::Json(m) => serde_json::json!(["json", m]),
        };
        let binding = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    "http-source-v1",
                    base.as_str(),
                    &self.calendar_id,
                    auth,
                    format
                ))
                .map_err(|_| E::Adapter)?
            )
        );
        let checkpoint: Checkpoint = state
            .checkpoint
            .as_deref()
            .and_then(|s| serde_json::from_str::<Checkpoint>(s).ok())
            .filter(|c| c.binding == binding)
            .unwrap_or_default();
        let validators = if cursor.is_none() && state.has_snapshot {
            RemoteValidators {
                etag: checkpoint.etag,
                last_modified: checkpoint.last_modified,
            }
        } else {
            RemoteValidators::default()
        };
        let can_validate = validators.etag.is_some() || validators.last_modified.is_some();
        let outcome = self
            .fetcher
            .fetch(RemoteFetchRequest {
                url: url.to_string(),
                authentication: self.authentication.clone(),
                validators,
                has_committed_snapshot: cursor.is_none() && state.has_snapshot && can_validate,
                now_ms: now,
            })
            .await
            .map_err(|_| E::Adapter)?;
        let doc = match outcome {
            RemoteFetchOutcome::NotModified(_) => return Ok(SourcePageOutcome::NotModified),
            RemoteFetchOutcome::Modified(doc) => doc,
        };
        let bytes = doc.body.len();
        // Defense for alternate fetcher implementations; the production transport also bounds bytes.
        if bytes > 8 * 1024 * 1024 {
            return Err(E::Budget);
        }
        let response_checkpoint = Checkpoint {
            binding: binding.clone(),
            etag: doc.validators.etag,
            last_modified: doc.validators.last_modified,
        };
        let (events, next, snapshot_token) = match &self.format {
            SourceFormat::Ics(limits) => {
                let text = std::str::from_utf8(&doc.body).map_err(|_| E::Adapter)?;
                let normalized = normalize_ics(text, limits).map_err(|_| E::Adapter)?;
                (normalized.events, None, None)
            }
            SourceFormat::Json(mapping) => {
                const FIELDS: &[&str] = &[
                    "external_uid",
                    "title",
                    "status",
                    "start",
                    "end",
                    "duration",
                    "description",
                    "location",
                    "url",
                    "classification",
                    "transparency",
                    "recurrence",
                    "organizer",
                    "attendees",
                    "alarms",
                    "attachments",
                    "source_sequence",
                    "source_modified_at",
                    "timezones",
                    "extensions",
                ];
                if mapping.fields.is_empty()
                    || mapping.fields.len() > FIELDS.len()
                    || mapping
                        .fields
                        .iter()
                        .any(|(k, v)| !FIELDS.contains(&k.as_str()) || !pointer(v))
                    || !pointer(&mapping.items)
                    || !pointer(&mapping.next)
                {
                    return Err(E::Adapter);
                }
                let value: serde_json::Value =
                    serde_json::from_slice(&doc.body).map_err(|_| E::Adapter)?;
                let items = value
                    .pointer(&mapping.items)
                    .and_then(|v| v.as_array())
                    .ok_or(E::InvalidSnapshot)?;
                if items.len() > MAX_BATCH_RECORDS {
                    return Err(E::Budget);
                }
                let mut mapped_bytes = 0usize;
                let mut events = Vec::with_capacity(items.len());
                for item in items {
                    let mut mapped = serde_json::Map::new();
                    mapped.insert("schema_version".into(), "1.0".into());
                    for (field, path) in &mapping.fields {
                        let v = item.pointer(path).ok_or(E::InvalidSnapshot)?;
                        mapped_bytes = mapped_bytes
                            .checked_add(serde_json::to_vec(v).map_err(|_| E::Adapter)?.len())
                            .ok_or(E::Budget)?;
                        if mapped_bytes > 8 * 1024 * 1024 {
                            return Err(E::Budget);
                        }
                        mapped.insert(field.clone(), v.clone());
                    }
                    let event: calendar_application::EventV1 =
                        serde_json::from_value(mapped.into()).map_err(|_| E::InvalidSnapshot)?;
                    calendar_domain::validate_event(&event).map_err(|_| E::InvalidSnapshot)?;
                    events.push(event);
                }
                let next = match value.pointer(&mapping.next) {
                    Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(s)) if !s.is_empty() && s.len() <= 8192 => {
                        Some(s.clone())
                    }
                    _ => return Err(E::InvalidSnapshot),
                };
                let snapshot_token = match value.get("snapshot_id") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(s)) if !s.is_empty() && s.len() <= 256 => {
                        Some(s.clone())
                    }
                    _ => return Err(E::InvalidSnapshot),
                };
                (events, next, snapshot_token)
            }
        };
        // A first-page validator cannot validate a whole paginated collection.
        let checkpoint = if next.is_some() {
            Checkpoint {
                binding,
                ..Checkpoint::default()
            }
        } else {
            response_checkpoint
        };
        let checkpoint = Some(serde_json::to_string(&checkpoint).map_err(|_| E::Adapter)?);
        Ok(SourcePageOutcome::Modified(SourcePage {
            snapshot_token,
            records: events
                .into_iter()
                .map(|event| SyncRecord::Upsert {
                    calendar_id: self.calendar_id.clone(),
                    event: Box::new(event),
                })
                .collect(),
            next,
            checkpoint,
            bytes,
        }))
    }
}
fn pointer(raw: &str) -> bool {
    raw.len() <= 256 && (raw.is_empty() || raw.starts_with('/'))
}
