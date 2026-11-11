//! SessionService handlers.

use super::AppState;
use crate::auth::Principal;
use crate::error::{Error, parse_uuid};
use std::sync::Arc;
use tonic::{Request, Response, Status};
use wheezper_proto as proto;
use wheezper_proto::session_service_server::SessionService as SessionServiceGrpc;

pub struct SessionGrpc {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl SessionServiceGrpc for SessionGrpc {
    async fn create_session(
        &self,
        request: Request<proto::CreateSessionRequest>,
    ) -> Result<Response<proto::CreateSessionResponse>, Status> {
        self.state
            .auth
            .authenticate_device(request.metadata())
            .await
            .map_err(Status::from)?;
        let req = request.into_inner();
        let (row, token) = self
            .state
            .sessions
            .create(
                &req.title,
                req.ai_help,
                &req.ai_context,
                &req.ai_instructions,
            )
            .await
            .map_err(Status::from)?;
        tracing::info!(
            session = %row.id,
            title = %row.title,
            ai_help = row.ai_help,
            "session created"
        );
        Ok(Response::new(proto::CreateSessionResponse {
            session: Some(row.into()),
            session_token: token,
        }))
    }

    async fn update_session_guidance(
        &self,
        request: Request<proto::UpdateSessionGuidanceRequest>,
    ) -> Result<Response<proto::Session>, Status> {
        // The brief describes what the user is doing, so only the paired phone
        // writes it — a session publish token may read its session, not edit it.
        self.state
            .auth
            .authenticate_device(request.metadata())
            .await
            .map_err(Status::from)?;
        let req = request.into_inner();
        let session_id = parse_uuid(&req.session_id, "session_id").map_err(Status::from)?;
        let row = self
            .state
            .sessions
            .update_guidance(session_id, &req.ai_context, &req.ai_instructions)
            .await
            .map_err(Status::from)?;
        tracing::info!(
            session = %row.id,
            context_chars = row.ai_context.chars().count(),
            instruction_chars = row.ai_instructions.chars().count(),
            "session guidance updated"
        );
        Ok(Response::new(row.into()))
    }

    async fn get_session(
        &self,
        request: Request<proto::GetSessionRequest>,
    ) -> Result<Response<proto::Session>, Status> {
        let principal = self
            .state
            .auth
            .authenticate(request.metadata())
            .await
            .map_err(Status::from)?;
        let req = request.into_inner();
        let session_id = parse_uuid(&req.session_id, "session_id").map_err(Status::from)?;

        // A session-scoped token may read exactly its own session.
        if let Principal::Session { session_id: scoped } = principal
            && scoped != session_id
        {
            return Err(Status::from(Error::Unauthorized));
        }

        let row = self
            .state
            .sessions
            .get(session_id)
            .await
            .map_err(Status::from)?;
        Ok(Response::new(row.into()))
    }

    async fn list_sessions(
        &self,
        request: Request<proto::ListSessionsRequest>,
    ) -> Result<Response<proto::ListSessionsResponse>, Status> {
        self.state
            .auth
            .authenticate_device(request.metadata())
            .await
            .map_err(Status::from)?;
        let req = request.into_inner();
        let rows = self
            .state
            .sessions
            .list(req.limit as i64, req.active_only)
            .await
            .map_err(Status::from)?;
        Ok(Response::new(proto::ListSessionsResponse {
            sessions: rows.into_iter().map(Into::into).collect(),
        }))
    }

    async fn start_session(
        &self,
        request: Request<proto::StartSessionRequest>,
    ) -> Result<Response<proto::Session>, Status> {
        self.state
            .auth
            .authenticate_device(request.metadata())
            .await
            .map_err(Status::from)?;
        let req = request.into_inner();
        let session_id = parse_uuid(&req.session_id, "session_id").map_err(Status::from)?;
        let row = self
            .state
            .sessions
            .start(session_id)
            .await
            .map_err(Status::from)?;
        Ok(Response::new(row.into()))
    }

    async fn end_session(
        &self,
        request: Request<proto::EndSessionRequest>,
    ) -> Result<Response<proto::EndSessionResponse>, Status> {
        self.state
            .auth
            .authenticate_device(request.metadata())
            .await
            .map_err(Status::from)?;
        let req = request.into_inner();
        let session_id = parse_uuid(&req.session_id, "session_id").map_err(Status::from)?;
        let row = self
            .state
            .sessions
            .end(session_id)
            .await
            .map_err(Status::from)?;
        tracing::info!(session = %row.id, "session ended");
        // Mail the notes after the RPC is answered: a slow SMTP relay must not
        // hold up the phone, and a failure must not fail the end of a meeting
        // that is already stored.
        if self.state.email.is_some() {
            let state = self.state.clone();
            let completed = row.clone();
            tokio::spawn(async move {
                if state.email.is_some() {
                    match crate::email::send_notes(&state, &completed).await {
                        Ok(()) => tracing::info!(session = %completed.id, "meeting notes emailed"),
                        Err(err) => tracing::warn!(
                            session = %completed.id,
                            error = %err,
                            "could not email notes"
                        ),
                    }
                }
            });
        }
        Ok(Response::new(proto::EndSessionResponse {
            session: Some(row.into()),
        }))
    }
}
