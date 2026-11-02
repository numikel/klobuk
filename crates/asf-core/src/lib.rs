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

pub mod brief;
pub mod config;
pub mod error;
pub mod ids;
pub mod jsonl;
pub mod lane;
pub mod model;
pub mod ports;
pub mod run;
pub mod runtime;
pub mod scheduler;
pub mod vcs;

pub use brief::{render_brief, render_claude_local, render_settings, BriefInput, SettingsInput};
pub use config::{Config, ConfigError};
pub use error::{ClaimError, StoreError};
pub use ids::{is_valid_prefix, IssueId};
pub use jsonl::{CommentRecord, IssueRecord, Record, FORMAT};
pub use model::*;
pub use ports::{ExportOpts, ImportMode, ImportReport, IssueStore};
pub use run::*;
pub use runtime::*;
pub use vcs::*;
