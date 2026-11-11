//! One error type for the domain layer, mapped to gRPC status codes at the
//! edge. Keeping the mapping in one place means a database detail never leaks
//! to a mobile client.

use tonic::Status;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("not authorized")]
    Unauthorized,
    #[error("session has ended")]
    SessionEnded,
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("assistant provider error: {0}")]
    Provider(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }

    pub fn provider(msg: impl Into<String>) -> Self {
        Self::Provider(msg.into())
    }
}

impl From<Error> for Status {
    fn from(err: Error) -> Self {
        match err {
            Error::NotFound => Status::not_found("not found"),
            Error::Unauthorized => Status::unauthenticated("invalid or expired credential"),
            Error::SessionEnded => Status::failed_precondition("session has ended"),
            Error::Invalid(msg) => Status::invalid_argument(msg),
            Error::Provider(msg) => Status::unavailable(format!("assistant provider: {msg}")),
            Error::Database(e) => {
                tracing::error!(error = %e, "database error");
                Status::internal("database error")
            }
            Error::Other(e) => {
                tracing::error!(error = %e, "internal error");
                Status::internal("internal error")
            }
        }
    }
}

/// Parse a client-supplied UUID; a malformed id is a client error, not a 500.
pub fn parse_uuid(value: &str, field: &str) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(value.trim())
        .map_err(|_| Error::Invalid(format!("{field} is not a valid UUID")))
}
