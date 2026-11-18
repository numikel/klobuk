//! Error types shared across the port boundary.

use thiserror::Error;

use crate::model::Status;

/// Errors raised by any `IssueStore` implementation.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("validation: {0}")]
    Validation(String),
    #[error("io: {0}")]
    Io(String),
    #[error("backend: {0}")]
    Backend(String),
}

/// Errors raised when claiming an issue.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ClaimError {
    #[error("already claimed by {by}")]
    AlreadyClaimed { by: String },
    #[error("issue not ready (status {status})")]
    NotReady { status: Status },
    #[error(transparent)]
    Store(#[from] StoreError),
}
