//! Committed transcript events: idempotent ingestion, ordered reads and the
//! in-memory fan-out to connected viewers.

pub mod broadcaster;
pub mod repository;
pub mod service;

pub use repository::StoredEvent;
pub use service::TranscriptService;

use wheezper_proto as proto;

impl From<StoredEvent> for proto::TranscriptEvent {
    fn from(e: StoredEvent) -> Self {
        Self {
            event_id: e.event_id.to_string(),
            session_id: e.session_id.to_string(),
            sequence_number: e.sequence_number,
            speaker: e.speaker,
            content: e.content,
            started_at: Some(crate::time::proto_ts(e.started_at)),
            received_at: Some(crate::time::proto_ts(e.received_at)),
            source: "wheezper".to_string(),
        }
    }
}
