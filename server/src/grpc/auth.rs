//! Device pairing RPC.

use super::AppState;
use tonic::{Request, Response, Status};
use wheezper_proto as proto;
use wheezper_proto::auth_service_server::AuthService;

pub struct AuthGrpc {
    pub state: std::sync::Arc<AppState>,
}

#[tonic::async_trait]
impl AuthService for AuthGrpc {
    async fn pair_device(
        &self,
        request: Request<proto::PairDeviceRequest>,
    ) -> Result<Response<proto::PairDeviceResponse>, Status> {
        let req = request.into_inner();
        let (token, expires_at) = self
            .state
            .auth
            .pair_device(
                &req.pairing_code,
                &self.state.config.pairing_code,
                &req.device_name,
                self.state.config.device_ttl,
            )
            .await
            .map_err(Status::from)?;
        tracing::info!(device = %req.device_name, "device paired");
        Ok(Response::new(proto::PairDeviceResponse {
            device_token: token,
            expires_at: Some(crate::time::proto_ts(expires_at)),
        }))
    }
}
