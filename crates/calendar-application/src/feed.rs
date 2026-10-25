//! Finite feed policy; a stored capability always carries its validated scope.
use crate::SecurityError;
use calendar_domain::{Change, EventV1};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibleField {
    Summary,
    Description,
    Location,
    Url,
    Categories,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedScopeV1 {
    version: u8,
    calendar_ids: Vec<String>,
    visible_fields: Vec<VisibleField>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedScope {
    Legacy,
    V1(FeedScopeV1),
}
impl FeedScope {
    /// `{}` retains the legacy whole-source behavior. V1 requires both arrays;
    /// there are no implicit grants, wildcard calendars or expression evaluation.
    pub fn parse(json: &str) -> Result<Self, SecurityError> {
        if json.len() > 65536 || !json.trim_start().starts_with('{') {
            return Err(SecurityError::InvalidInput);
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Legacy {}
        if serde_json::from_str::<Legacy>(json).is_ok() {
            return Ok(Self::Legacy);
        }
        let v: FeedScopeV1 = serde_json::from_str(json).map_err(|_| SecurityError::InvalidInput)?;
        if v.version != 1
            || v.calendar_ids.len() > 256
            || v.visible_fields.len() > 5
            || v.calendar_ids.iter().any(|s| s.is_empty() || s.len() > 256)
            || v.calendar_ids
                .iter()
                .enumerate()
                .any(|(i, s)| v.calendar_ids[..i].contains(s))
            || v.visible_fields
                .iter()
                .enumerate()
                .any(|(i, s)| v.visible_fields[..i].contains(s))
        {
            return Err(SecurityError::InvalidInput);
        }
        Ok(Self::V1(v))
    }
    pub fn to_json(&self) -> String {
        match self {
            Self::Legacy => "{}".into(),
            Self::V1(v) => serde_json::to_string(v).expect("finite scope"),
        }
    }
    pub fn calendar_ids(&self) -> Option<&[String]> {
        match self {
            Self::Legacy => None,
            Self::V1(v) => Some(&v.calendar_ids),
        }
    }
    /// Remove only the five supported content fields. Other capabilities remain in the
    /// document so the renderer still rejects unsupported input instead of hiding loss.
    pub fn project(&self, event: &mut EventV1) {
        let Self::V1(v) = self else { return };
        let visible = |field| v.visible_fields.contains(&field);
        if !visible(VisibleField::Summary) {
            event.title = "Busy".into();
        }
        if !visible(VisibleField::Description) {
            event.description = None;
        }
        if !visible(VisibleField::Location) {
            event.location = None;
        }
        if !visible(VisibleField::Url) {
            event.url = None;
        }
        if !visible(VisibleField::Categories) {
            event.categories = None;
        }
        if let Some(recurrence) = &mut event.recurrence {
            for exception in recurrence.exceptions.iter_mut().flatten() {
                if let Change::Override { patch } = &mut exception.change {
                    if !visible(VisibleField::Summary) {
                        patch.title = None;
                    }
                    if !visible(VisibleField::Description) {
                        patch.description = None;
                    }
                    if !visible(VisibleField::Location) {
                        patch.location = None;
                    }
                    if !visible(VisibleField::Url) {
                        patch.url = None;
                    }
                    if !visible(VisibleField::Categories) {
                        patch.categories = None;
                    }
                }
            }
        }
    }
}
