//! Live fan-out of persisted events to connected viewers.
//!
//! Capacity is bounded on purpose: a slow viewer must not make the server hold
//! unbounded memory. When it lags, the streaming handler re-reads the backlog
//! from PostgreSQL.

use super::repository::StoredEvent;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct Broadcaster {
    tx: broadcast::Sender<StoredEvent>,
}

impl Broadcaster {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(16));
        Self { tx }
    }

    /// Returns the number of live subscribers. A publish with no subscribers
    /// is still durable — the event is already in the database.
    pub fn publish(&self, event: StoredEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// All sessions share one channel; each event carries its session id.
    pub fn subscribe(&self) -> broadcast::Receiver<StoredEvent> {
        self.tx.subscribe()
    }
}
