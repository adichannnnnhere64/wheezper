//! gRPC edge: request authentication, proto <-> domain conversion, status
//! mapping. No SQL and no provider calls live here.

pub mod assistant;
pub mod auth;
pub mod session;
pub mod transcript;

use crate::assistant::AssistantService;
use crate::auth::Authenticator;
use crate::config::Config;
use crate::session::SessionService;
use crate::transcript::TranscriptService;
use std::sync::Arc;

pub struct AppState {
    pub config: Arc<Config>,
    pub auth: Authenticator,
    pub sessions: SessionService,
    pub transcripts: TranscriptService,
    pub assistant: AssistantService,
    /// Present only when the operator configured SMTP: notes are mailed on
    /// `EndSession`, and never when this is `None`.
    pub email: Option<crate::email::Email>,
    /// Personal knowledge base, when the operator linked one.
    pub kb: Option<crate::kb::Kb>,
}
