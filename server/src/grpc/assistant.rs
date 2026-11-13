//! AssistantService handlers.
//!
//! The request may carry a model, a provider token and a backend choice. The
//! token is forwarded to the provider for this call and dropped — it is never
//! stored or logged. The backend is an allowlist, not a URL.

use super::AppState;
use crate::assistant::service::AssistantOptions;
use crate::error::{Error, parse_uuid};
use futures_util::{Stream, StreamExt};
use std::pin::Pin;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use wheezper_proto as proto;
use wheezper_proto::assistant_service_server::AssistantService;

pub struct AssistantGrpc {
    pub state: Arc<AppState>,
}

type AnswerStream = Pin<Box<dyn Stream<Item = Result<proto::AnswerChunk, Status>> + Send>>;

#[tonic::async_trait]
impl AssistantService for AssistantGrpc {
    async fn ask_question(
        &self,
        request: Request<proto::AskQuestionRequest>,
    ) -> Result<Response<proto::AskQuestionResponse>, Status> {
        let (device_id, _) = self
            .state
            .auth
            .authenticate_device(request.metadata())
            .await
            .map_err(Status::from)?;
        let req = request.into_inner();
        let session_id = parse_uuid(&req.session_id, "session_id").map_err(Status::from)?;
        let mut options = options(&req)?;
        options.device_id = Some(device_id);
        let answer = self
            .state
            .assistant
            .ask(session_id, req.question, &options)
            .await
            .map_err(Status::from)?;
        Ok(Response::new(proto::AskQuestionResponse {
            answer: answer.text,
            provider: answer.provider,
            model: answer.model,
        }))
    }

    type StreamAnswerStream = AnswerStream;

    async fn stream_answer(
        &self,
        request: Request<proto::AskQuestionRequest>,
    ) -> Result<Response<Self::StreamAnswerStream>, Status> {
        let (device_id, _) = self
            .state
            .auth
            .authenticate_device(request.metadata())
            .await
            .map_err(Status::from)?;
        let req = request.into_inner();
        let session_id = parse_uuid(&req.session_id, "session_id").map_err(Status::from)?;

        let assistant = self.state.assistant.clone();
        // Resolve before returning the stream so a bad model/token is a plain
        // gRPC error instead of a failure after the first chunk.
        let mut options = options(&req)?;
        options.device_id = Some(device_id);
        let provider = assistant.resolve(&options).map_err(Status::from)?;
        let provider_name = provider.name.to_string();
        let model = provider.model.clone();
        let mut deltas = assistant.stream(
            session_id,
            req.question,
            provider,
            Some(device_id),
            crate::assistant::service::uses_own_cloud_model(&options),
        );

        let stream = async_stream::try_stream! {
            while let Some(delta) = deltas.next().await {
                let delta = delta.map_err(Status::from)?;
                yield proto::AnswerChunk {
                    delta,
                    done: false,
                    provider: provider_name.clone(),
                    model: model.clone(),
                };
            }
            yield proto::AnswerChunk {
                delta: String::new(),
                done: true,
                provider: provider_name,
                model,
            };
        };

        Ok(Response::new(Box::pin(stream) as Self::StreamAnswerStream))
    }
}

#[allow(clippy::result_large_err)]
fn options(request: &proto::AskQuestionRequest) -> Result<AssistantOptions, Status> {
    let backend = proto::AssistantBackend::try_from(request.backend)
        .map_err(|_| Status::from(Error::invalid("unknown assistant backend")))?;
    Ok(AssistantOptions {
        model: Some(request.model.clone()).filter(|m| !m.trim().is_empty()),
        api_key: Some(request.api_key.clone()).filter(|k| !k.trim().is_empty()),
        cloud: backend == proto::AssistantBackend::OllamaCloud,
        // Filled by the handler from the authenticated device.
        device_id: None,
    })
}
