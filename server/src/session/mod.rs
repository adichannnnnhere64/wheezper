//! Meeting session lifecycle. Sessions are durable rows, independent of the
//! devices currently connected to them.

pub mod repository;
pub mod service;

pub use repository::SessionRow;
pub use service::SessionService;

use wheezper_proto as proto;

impl From<SessionRow> for proto::Session {
    fn from(row: SessionRow) -> Self {
        Self {
            id: row.id.to_string(),
            title: row.title,
            status: match row.status.as_str() {
                "active" => proto::SessionStatus::Active as i32,
                "completed" => proto::SessionStatus::Completed as i32,
                _ => proto::SessionStatus::Created as i32,
            },
            started_at: Some(crate::time::proto_ts_opt(row.started_at).unwrap_or_default()),
            ended_at: crate::time::proto_ts_opt(row.ended_at),
            created_at: Some(crate::time::proto_ts(row.created_at)),
            transcript_count: row.transcript_count,
            ai_help: row.ai_help,
        }
    }
}
