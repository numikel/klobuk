//! Error model, exit codes and the JSON envelope.

use asf_core::scheduler::SchedulerError;
use asf_core::{ClaimError, ConfigError, StoreError};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Usage,
    Conflict,
    NotFound,
    /// A worker is waiting for a human (exit 1).
    Blocked,
    Validation,
    Io,
    Backend,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        AppError {
            code,
            message: message.into(),
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Usage, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Validation, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Io, message)
    }

    /// 0 ok, 1 error, 2 usage, 3 conflict.
    pub fn exit_code(&self) -> i32 {
        match self.code {
            ErrorCode::Usage => 2,
            ErrorCode::Conflict => 3,
            _ => 1,
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl From<StoreError> for AppError {
    fn from(e: StoreError) -> Self {
        let code = match e {
            StoreError::NotFound(_) => ErrorCode::NotFound,
            StoreError::Conflict(_) => ErrorCode::Conflict,
            StoreError::Validation(_) => ErrorCode::Validation,
            StoreError::Io(_) => ErrorCode::Io,
            StoreError::Backend(_) => ErrorCode::Backend,
        };
        AppError::new(code, e.to_string())
    }
}

impl From<ClaimError> for AppError {
    fn from(e: ClaimError) -> Self {
        match e {
            ClaimError::AlreadyClaimed { .. } | ClaimError::NotReady { .. } => {
                AppError::conflict(e.to_string())
            }
            ClaimError::Store(s) => s.into(),
        }
    }
}

impl From<ConfigError> for AppError {
    fn from(e: ConfigError) -> Self {
        AppError::validation(e.to_string())
    }
}

impl From<SchedulerError> for AppError {
    fn from(e: SchedulerError) -> Self {
        AppError::validation(e.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::io(e.to_string())
    }
}

/// What a command produces: a JSON value and its human rendering.
pub struct Rendered {
    pub json: Value,
    pub text: String,
}

impl Rendered {
    pub fn new<T: Serialize>(value: &T, text: impl Into<String>) -> Result<Rendered, AppError> {
        let json =
            serde_json::to_value(value).map_err(|e| AppError::new(ErrorCode::Backend, e.to_string()))?;
        Ok(Rendered {
            json,
            text: text.into(),
        })
    }
}

pub fn print_ok(json: bool, quiet: bool, r: &Rendered) {
    if json {
        let env = serde_json::json!({ "ok": true, "result": r.json });
        println!("{env}");
    } else if !quiet && !r.text.is_empty() {
        println!("{}", r.text);
    }
}

pub fn print_err(json: bool, e: &AppError) {
    if json {
        let env = serde_json::json!({ "ok": false, "error": { "code": e.code, "message": e.message } });
        println!("{env}");
    } else {
        eprintln!("error: {}", e.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes() {
        assert_eq!(AppError::usage("x").exit_code(), 2);
        assert_eq!(AppError::conflict("x").exit_code(), 3);
        assert_eq!(AppError::not_found("x").exit_code(), 1);
        assert_eq!(AppError::from(StoreError::Backend("b".into())).exit_code(), 1);
        assert_eq!(
            AppError::from(ClaimError::AlreadyClaimed { by: "w".into() }).exit_code(),
            3
        );
    }

    #[test]
    fn error_code_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ErrorCode::NotFound).unwrap(),
            "\"not_found\""
        );
    }
}
