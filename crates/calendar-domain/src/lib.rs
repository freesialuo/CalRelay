//! CalRelay event domain: typed `EventV1` and its semantic validator.
//!
//! No I/O and no network access; pure data and validation.

mod model;
mod validate;

pub use model::*;
pub use validate::{validate_event, Code, ValidationError};
