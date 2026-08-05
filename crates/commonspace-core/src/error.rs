//! Core error type shared across crates.

use thiserror::Error;

/// Errors originating in core domain logic.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Transition(#[from] crate::task::TransitionError),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("{0}")]
    Invalid(String),
}
