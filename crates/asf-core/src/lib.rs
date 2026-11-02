//! Domain types, ports and the pure scheduler for auto-software-factory.
//!
//! This crate performs no IO. Adapters (`asf-store`, the `asf` binary) depend on it.
//!
//! # Inbox relevance rule
//!
//! For a lane `L` other than `orchestrator`, a comment is relevant when any of these holds:
//! it sits on an issue currently claimed by worker `L`; its `to_lane` is `L` or `all`; its author
//! is `human`. The `orchestrator` lane sees every comment. Only comments above the lane's read
//! cursor for that issue are returned.

pub mod config;
pub mod error;
pub mod ids;
pub mod lane;
pub mod model;
pub mod ports;
pub mod scheduler;

pub use config::{Config, ConfigError};
pub use error::{ClaimError, StoreError};
pub use ids::{is_valid_prefix, IssueId};
pub use model::*;
pub use ports::{ExportOpts, ImportMode, ImportReport, IssueStore};
