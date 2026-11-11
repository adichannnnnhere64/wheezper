//! TranscriptService handlers: idempotent publish, ordered live streaming and
//! bounded catch-up reads.

use super::AppState;
use crate::error::{Error, parse_uuid};
use crate::transcript::repository::{NewEvent, StoredEvent};
use crate::transcript::service::SyncPage;
use futures_util::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tonic::{Request, Response, Status};
use uuid::Uuid;
use wheezper_proto as proto;
use wheezper_proto::transcript_service_server::TranscriptService;

const PAGE_SIZE: i64 = 500;
const MAX_CONTENT_CHARS: usize = 8_000;
const MAX_SPEAKER_CHARS: usize = 64;

pub struct TranscriptGrpc {
    pub state: Arc<AppState>,
}

type TranscriptStream = Pin<Box<dyn Stream<Item = Result<proto::TranscriptEvent, Status>> + Send>>;

#[tonic::async_trait]
impl TranscriptService for TranscriptGrpc {
    async fn publish_transcript(
        &self,
        request: Request<proto::TranscriptEvent>,
    ) -> Result<Response<proto::PublishTranscriptResponse>, Status> {
        let metadata = request.metadata().clone();
        let event = request.into_inner();
        let session_id = parse_uuid(&event.session_id, "session_id").map_err(Status::from)?;
        let event_id = parse_uuid(&event.event_id, "event_id").map_err(Status::from)?;

        // A session token can only publish into its own session.
        self.state
            .auth
            .authorize_publish(&metadata, session_id)
            .await
            .map_err(Status::from)?;

        let content = event.content.trim();
        if content.is_empty() {
            return Err(Status::from(Error::invalid("content is empty")));
        }
        if content.chars().count() > MAX_CONTENT_CHARS {
            return Err(Status::from(Error::invalid(format!(
                "content is longer than {MAX_CONTENT_CHARS} characters"
            ))));
        }
        let speaker: String = event
            .speaker
            .trim()
            .chars()
            .take(MAX_SPEAKER_CHARS)
            .collect();
        let started_at = crate::time::from_proto_ts(event.started_at);

        let outcome = self
            .state
            .transcripts
            .publish(NewEvent {
                event_id,
                session_id,
                speaker,
                content: content.to_string(),
                started_at,
            })
            .await
            .map_err(Status::from)?;

        Ok(Response::new(proto::PublishTranscriptResponse {
            event_id: outcome.event.event_id.to_string(),
            sequence_number: outcome.event.sequence_number,
            duplicate: outcome.duplicate,
        }))
    }

    type StreamTranscriptsStream = TranscriptStream;

    async fn stream_transcripts(
        &self,
        request: Request<proto::StreamTranscriptsRequest>,
    ) -> Result<Response<Self::StreamTranscriptsStream>, Status> {
        self.state
            .auth
            .authenticate_device(request.metadata())
            .await
            .map_err(Status::from)?;
        let req = request.into_inner();
        let session_id = parse_uuid(&req.session_id, "session_id").map_err(Status::from)?;
        self.state
            .sessions
            .get(session_id)
            .await
            .map_err(Status::from)?;

        // Subscribe before reading the backlog: no event can slip between the
        // two steps. Duplicates are filtered by sequence number.
        let mut live = self.state.transcripts.broadcaster().subscribe();
        let transcripts = self.state.transcripts.clone();
        let mut last = req.from_sequence.max(0);

        let stream = async_stream::try_stream! {
            // 1. backlog, page by page (bounded memory)
            loop {
                let page = transcripts.sync(session_id, last, PAGE_SIZE).await.map_err(Status::from)?;
                if page.events.is_empty() {
                    break;
                }
                for event in page.events {
                    last = event.sequence_number;
                    yield proto::TranscriptEvent::from(event);
                }
                if !page.has_more {
                    break;
                }
            }

            // 2. live fan-out
            loop {
                match live.recv().await {
                    Ok(event) if event.session_id != session_id => continue,
                    Ok(event) if event.sequence_number <= last => continue,
                    Ok(event) => {
                        // A gap means the channel handed us a later event
                        // first (or the row lock let a commit land between
                        // subscribe and backlog). Re-read the missing range
                        // from the database; it is the source of truth.
                        if event.sequence_number > last + 1 {
                            for event in catch_up(&transcripts, session_id, &mut last).await? {
                                yield proto::TranscriptEvent::from(event);
                            }
                            continue;
                        }
                        last = event.sequence_number;
                        yield proto::TranscriptEvent::from(event);
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        // The viewer was slower than the channel. Recover from
                        // PostgreSQL instead of dropping transcript lines.
                        tracing::warn!(skipped, "viewer lagged; resyncing from database");
                        for event in catch_up(&transcripts, session_id, &mut last).await? {
                            yield proto::TranscriptEvent::from(event);
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        };

        Ok(Response::new(
            Box::pin(stream) as Self::StreamTranscriptsStream
        ))
    }

    async fn sync_transcripts(
        &self,
        request: Request<proto::SyncTranscriptsRequest>,
    ) -> Result<Response<proto::SyncTranscriptsResponse>, Status> {
        self.state
            .auth
            .authenticate_device(request.metadata())
            .await
            .map_err(Status::from)?;
        let req = request.into_inner();
        let session_id = parse_uuid(&req.session_id, "session_id").map_err(Status::from)?;
        self.state
            .sessions
            .get(session_id)
            .await
            .map_err(Status::from)?;
        let page: SyncPage = self
            .state
            .transcripts
            .sync(session_id, req.from_sequence.max(0), req.limit as i64)
            .await
            .map_err(Status::from)?;
        Ok(Response::new(proto::SyncTranscriptsResponse {
            events: page.events.into_iter().map(Into::into).collect(),
            latest_sequence: page.latest_sequence,
            has_more: page.has_more,
        }))
    }
}

/// Read every missing page after `last` and advance it.
#[allow(clippy::result_large_err)]
async fn catch_up(
    transcripts: &crate::transcript::TranscriptService,
    session_id: Uuid,
    last: &mut i64,
) -> Result<Vec<StoredEvent>, Status> {
    let mut out = Vec::new();
    loop {
        let page = transcripts
            .sync(session_id, *last, PAGE_SIZE)
            .await
            .map_err(Status::from)?;
        if page.events.is_empty() {
            break;
        }
        for event in page.events {
            *last = event.sequence_number;
            out.push(event);
        }
        if !page.has_more {
            break;
        }
    }
    Ok(out)
}
