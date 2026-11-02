//! Domain types, ports and the pure scheduler for auto-software-factory.
//!
//! This crate performs no IO. Adapters (`asf-store`, the `asf` binary) depend on it.

pub mod ids;
pub mod model;

pub use ids::{is_valid_prefix, IssueId};
pub use model::*;
