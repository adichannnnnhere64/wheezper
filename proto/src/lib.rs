//! Versioned gRPC contract shared by the wheezper client, the backend and the
//! Android app. Bump the package (`wheezper.v2`) for breaking changes; never
//! reuse a field number.

/// Generated messages and service stubs for `wheezper.v1`.
///
/// Both client and server stubs are generated so the same crate can be linked
/// into the lightweight CLI (client only) and the backend.
#[allow(clippy::result_large_err)]
pub mod v1 {
    tonic::include_proto!("wheezper.v1");
}

pub use v1::*;
