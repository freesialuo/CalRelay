//! CalRelay event domain: typed `EventV1` and its semantic validator.
//!
//! No I/O and no network access; pure data and validation.

mod model;
mod validate;

pub use model::*;
pub use validate::{validate_event, Code, ValidationError};

/// Embedded IANA time zone database version provided by `chrono-tz`.
pub const TZDB_VERSION: &str = chrono_tz::IANA_TZDB_VERSION;
