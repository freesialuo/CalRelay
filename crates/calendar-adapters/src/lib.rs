//! External protocol adapters for CalRelay.

pub mod admin_auth;
pub mod bark;
pub mod ics;
pub mod ingestion;
pub mod remote;
pub mod renderer;

pub use ics::*;
