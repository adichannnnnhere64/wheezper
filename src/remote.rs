//! Optional remote meeting session (feature `sync`).
//!
//! Committed transcript lines are published to a wheezper-server session. The
//! local notepad stays the source of truth for the person at the laptop and
//! never waits for the network:
//!
//! 1. the line is written to the notepad,
//! 2. the event is appended to a local pending queue (fsynced),
//! 3. a worker thread publishes it and removes it only after the server acked.
//!
//! If the server is unreachable, the queue keeps growing on disk; the next run
//! (or the same run, after the network returns) drains it in order. Event ids
//! make retries harmless: the server ignores an event it already stored.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};
use wheezper_proto as proto;

/// How long a shutdown waits for the pending queue to drain. Anything still
/// queued stays on disk for the next run.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Rewrite the queue file after this many ack markers.
const COMPACT_EVERY: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingEvent {
    event_id: String,
    session_id: String,
    speaker: String,
    content: String,
    /// RFC3339 UTC; the server stores this as `started_at`.
    started_at: String,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Record {
    Ack { ack: String },
    Event(PendingEvent),
}

struct Queue {
    path: PathBuf,
    file: File,
    pending: VecDeque<PendingEvent>,
    acks_since_compact: usize,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    stopped: AtomicBool,
}

impl Shared {
    fn open(session_id: &str) -> Result<Arc<Self>> {
        let path = queue_path(session_id)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating pending-event dir {}", parent.display()))?;
        }
        let (pending, needs_compact) = load(&path)?;
        let file = open_append(&path)?;
        let queue = Queue {
            path,
            file,
            pending,
            acks_since_compact: 0,
        };
        let shared = Arc::new(Self {
            queue: Mutex::new(queue),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
        });
        if needs_compact {
            shared.compact()?;
        }
        Ok(shared)
    }

    /// Persist first, then make it visible to the worker.
    fn enqueue(&self, event: PendingEvent) -> Result<()> {
        let mut queue = self.queue.lock().expect("queue lock");
        let line = serde_json::to_string(&event).context("serializing pending event")?;
        writeln!(queue.file, "{line}")?;
        queue.file.flush()?;
        // The pending queue is the crash-safety story: an event is either in
        // the notes + on disk, or it never happened.
        queue.file.sync_data()?;
        queue.pending.push_back(event);
        drop(queue);
        self.wake.notify_all();
        Ok(())
    }

    fn front(&self) -> Option<PendingEvent> {
        self.queue
            .lock()
            .expect("queue lock")
            .pending
            .front()
            .cloned()
    }

    fn len(&self) -> usize {
        self.queue.lock().expect("queue lock").pending.len()
    }

    fn ack(&self, event_id: &str) {
        let mut queue = self.queue.lock().expect("queue lock");
        // The worker only ever sends the front event, so a matching id is
        // there; tolerate a mismatch instead of corrupting the order.
        let at = queue.pending.iter().position(|e| e.event_id == event_id);
        let Some(index) = at else { return };
        queue.pending.remove(index);
        let marker = serde_json::json!({ "ack": event_id });
        if writeln!(queue.file, "{marker}").is_err() {
            // If the marker cannot be written, the event would be resent after
            // a restart — harmless (the server is idempotent).
            eprintln!("wheezper: warning: could not record acked session event");
        }
        queue.acks_since_compact += 1;
        // An empty queue is compacted immediately so a clean run leaves an
        // empty file rather than a log of event + ack pairs.
        let compact = queue.acks_since_compact >= COMPACT_EVERY || queue.pending.is_empty();
        drop(queue);
        if compact {
            let _ = self.compact();
        }
    }

    fn compact(&self) -> Result<()> {
        let mut queue = self.queue.lock().expect("queue lock");
        let path = queue.path.clone();
        let tmp = path.with_extension("tmp");
        {
            let mut out = File::create(&tmp)
                .with_context(|| format!("rewriting pending queue {}", tmp.display()))?;
            for event in &queue.pending {
                writeln!(out, "{}", serde_json::to_string(event)?)?;
            }
            out.sync_all()?;
        }
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("replacing pending queue {}", path.display()))?;
        queue.file = open_append(&path)?;
        queue.acks_since_compact = 0;
        Ok(())
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Sleep until a new event arrives, the stop flag flips, or the timeout
    /// expires. Used instead of `thread::sleep` so shutdown is immediate.
    fn wait(&self, timeout: Duration) {
        let queue = self.queue.lock().expect("queue lock");
        let _ = self
            .wake
            .wait_timeout_while(queue, timeout, |queue| {
                queue.pending.is_empty() && !self.is_stopped()
            })
            .expect("queue lock");
    }
}

fn load(path: &Path) -> Result<(VecDeque<PendingEvent>, bool)> {
    let mut pending: VecDeque<PendingEvent> = VecDeque::new();
    let mut compact = false;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((pending, false));
        }
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    for (number, line) in BufReader::new(file).lines().enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!(
                    "wheezper: warning: pending queue {} read error: {err}",
                    path.display()
                );
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line) {
            Ok(Record::Ack { ack }) => {
                let before = pending.len();
                pending.retain(|event| event.event_id != ack);
                compact |= pending.len() != before;
            }
            Ok(Record::Event(event)) => pending.push_back(event),
            Err(err) => eprintln!(
                "wheezper: warning: skipping corrupt line {} in {}: {err}",
                number + 1,
                path.display()
            ),
        }
    }
    Ok((pending, compact))
}

fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .with_context(|| format!("opening pending queue {}", path.display()))
}

/// Where unsent events live between runs.
fn queue_path(session_id: &str) -> Result<PathBuf> {
    Ok(state_dir()
        .join("pending")
        .join(format!("{session_id}.jsonl")))
}

fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("WHEEZPER_STATE_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("wheezper");
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(profile).join(".wheezper");
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .unwrap_or_else(std::env::temp_dir);
        return base.join("wheezper");
    }
    #[allow(unreachable_code)]
    std::env::temp_dir().join("wheezper")
}

/// The sink the transcription engine holds. Cheap and non-blocking to call:
/// it writes one small file line and wakes the worker.
pub struct Remote {
    shared: Arc<Shared>,
    worker: Option<std::thread::JoinHandle<()>>,
    finished: Option<mpsc::Receiver<()>>,
    session_id: String,
}

impl Remote {
    pub fn connect(server: &str, session_id: &str, token: &str) -> Result<Self> {
        uuid::Uuid::parse_str(session_id.trim())
            .with_context(|| format!("--session is not a UUID: {session_id}"))?;
        if token.trim().is_empty() || !token.is_ascii() {
            bail!("--session-token must be a non-empty ASCII token");
        }
        let shared = Shared::open(session_id)?;
        let buffered = shared.len();
        let done = start_worker(
            Arc::clone(&shared),
            server.trim_end_matches('/').to_string(),
            token.trim().to_string(),
        )?;
        eprintln!(
            "wheezper: session sync -> {server} (session {session_id}{})",
            if buffered > 0 {
                format!(", {buffered} buffered event(s) to send")
            } else {
                String::new()
            }
        );
        Ok(Self {
            shared,
            worker: Some(done.0),
            finished: Some(done.1),
            session_id: session_id.to_string(),
        })
    }

    /// Build the sink from the CLI flags. `None` means a purely local run.
    pub fn from_cli(
        session: Option<&str>,
        server: Option<&str>,
        token: Option<&str>,
    ) -> Result<Option<Self>> {
        let Some(session) = session else {
            return Ok(None);
        };
        let server = server.context("--session requires --server")?;
        let token = token.context("--session requires --session-token")?;
        Ok(Some(Self::connect(server, session, token)?))
    }

    /// Publish one committed line. Never blocks on the network and never
    /// fails the transcription path.
    pub fn publish(&self, label: Option<&str>, text: &str) {
        if text.trim().is_empty() || self.session_id.is_empty() {
            return;
        }
        let event = PendingEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            session_id: self.session_id.clone(),
            speaker: label.unwrap_or("").to_string(),
            content: text.to_string(),
            started_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        };
        if let Err(err) = self.shared.enqueue(event) {
            eprintln!("wheezper: warning: could not queue session event: {err:#}");
        }
    }

    /// Stop and give the queue a short chance to drain; the rest stays on disk.
    pub fn finish(mut self) {
        self.shutdown(DRAIN_TIMEOUT);
    }

    fn shutdown(&mut self, timeout: Duration) {
        self.shared.stop();
        let stopped = self
            .finished
            .take()
            .map(|done| done.recv_timeout(timeout).is_ok())
            .unwrap_or(false);
        if !stopped {
            let left = self.shared.len();
            if left > 0 {
                eprintln!(
                    "wheezper: {left} session event(s) still queued; they will be sent next run"
                );
            }
        }
        self.worker.take();
    }

    /// Number of events still unacknowledged (tests).
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.shared.len()
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.shutdown(Duration::from_millis(500));
        }
    }
}

fn start_worker(
    shared: Arc<Shared>,
    server: String,
    token: String,
) -> Result<(std::thread::JoinHandle<()>, mpsc::Receiver<()>)> {
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("wheezper-sync".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    eprintln!("wheezper: session sync unavailable: {err}");
                    let _ = done_tx.send(());
                    return;
                }
            };
            runtime.block_on(run_worker(shared, server, token));
            let _ = done_tx.send(());
        })
        .context("spawning session sync worker")?;
    Ok((worker, done_rx))
}

type Client = proto::transcript_service_client::TranscriptServiceClient<tonic::transport::Channel>;

async fn run_worker(shared: Arc<Shared>, server: String, token: String) {
    let mut client: Option<Client> = None;
    let mut backoff = Duration::from_secs(1);
    let mut announced_outage = false;
    let mut first_connect = true;
    let mut stop_deadline: Option<Instant> = None;

    loop {
        if shared.is_stopped() {
            stop_deadline.get_or_insert_with(|| Instant::now() + DRAIN_TIMEOUT);
            if stop_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
        }

        let Some(event) = shared.front() else {
            if shared.is_stopped() {
                break;
            }
            shared.wait(Duration::from_secs(1));
            continue;
        };

        if client.is_none() {
            match connect(&server).await {
                Ok(connected) => {
                    if announced_outage {
                        eprintln!("wheezper: session sync reconnected");
                    } else if first_connect {
                        eprintln!("wheezper: session sync connected");
                    }
                    announced_outage = false;
                    first_connect = false;
                    backoff = Duration::from_secs(1);
                    client = Some(connected);
                }
                Err(err) => {
                    if !announced_outage {
                        eprintln!(
                            "wheezper: session sync offline ({err}); buffering locally, transcription continues"
                        );
                        announced_outage = true;
                    }
                    sleep_backoff(&shared, &mut backoff, stop_deadline).await;
                    continue;
                }
            }
        }

        let Some(active) = client.as_mut() else {
            continue;
        };
        match send_event(active, &token, &event).await {
            Ok((_sequence, duplicate)) => {
                if duplicate {
                    tracing_shim("server already had an event; retry acknowledged");
                }
                shared.ack(&event.event_id);
                backoff = Duration::from_secs(1);
            }
            Err(status) => {
                use tonic::Code;
                match status.code() {
                    Code::InvalidArgument => {
                        // Poison event: dropping it is the only way forward,
                        // and the server told us why.
                        eprintln!(
                            "wheezper: warning: server rejected a transcript event ({}) — dropping it",
                            status.message()
                        );
                        shared.ack(&event.event_id);
                    }
                    Code::Unauthenticated | Code::PermissionDenied => {
                        eprintln!(
                            "wheezper: session sync rejected ({}) — check --session-token",
                            status.message()
                        );
                        // Retrying the same token cannot help; stop this run.
                        break;
                    }
                    Code::FailedPrecondition => {
                        eprintln!(
                            "wheezper: session has ended on the server ({}) — no longer publishing",
                            status.message()
                        );
                        break;
                    }
                    _ => {
                        client = None;
                        if !announced_outage {
                            eprintln!(
                                "wheezper: session sync connection lost ({status}); buffering locally"
                            );
                            announced_outage = true;
                        }
                        sleep_backoff(&shared, &mut backoff, stop_deadline).await;
                    }
                }
            }
        }
    }
}

async fn connect(server: &str) -> Result<Client, tonic::transport::Error> {
    let mut endpoint = tonic::transport::Endpoint::from_shared(server.to_string())?
        .connect_timeout(CONNECT_TIMEOUT);
    // Remote deployments terminate TLS in front of the server (or serve it
    // directly); the public roots are all a client needs.
    if server.starts_with("https://") {
        endpoint = endpoint.tls_config(
            tonic::transport::ClientTlsConfig::new().with_webpki_roots(),
        )?;
    }
    let channel = endpoint.connect().await?;
    Ok(Client::new(channel))
}

async fn send_event(
    client: &mut Client,
    token: &str,
    event: &PendingEvent,
) -> Result<(i64, bool), tonic::Status> {
    let started_at = DateTime::parse_from_rfc3339(&event.started_at)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let proto_event = proto::TranscriptEvent {
        event_id: event.event_id.clone(),
        session_id: event.session_id.clone(),
        sequence_number: 0,
        speaker: event.speaker.clone(),
        content: event.content.clone(),
        started_at: Some(prost_types::Timestamp {
            seconds: started_at.timestamp(),
            nanos: started_at.timestamp_subsec_nanos() as i32,
        }),
        received_at: None,
        source: "wheezper".to_string(),
    };
    let mut request = tonic::Request::new(proto_event);
    let bearer = format!("Bearer {token}");
    let value = bearer
        .parse()
        .map_err(|_| tonic::Status::invalid_argument("session token is not ASCII"))?;
    request.metadata_mut().insert("authorization", value);
    let response = tokio::time::timeout(PUBLISH_TIMEOUT, client.publish_transcript(request))
        .await
        .map_err(|_| tonic::Status::deadline_exceeded("publish timed out"))??
        .into_inner();
    Ok((response.sequence_number, response.duplicate))
}

/// Wait out an outage without ignoring shutdown; the queue keeps filling.
async fn sleep_backoff(shared: &Shared, backoff: &mut Duration, stop_deadline: Option<Instant>) {
    let mut pause = *backoff;
    if let Some(deadline) = stop_deadline {
        pause = pause.min(deadline.saturating_duration_since(Instant::now()));
    }
    // Sleep in slices: a shutdown must not wait out a 30 s backoff.
    let until = Instant::now() + pause;
    while Instant::now() < until && !shared.is_stopped() {
        tokio::time::sleep(Duration::from_millis(100).min(until - Instant::now())).await;
    }
    *backoff = (*backoff * 2).min(MAX_BACKOFF);
}

/// The client binary keeps its diagnostics on stderr; no tracing subscriber.
fn tracing_shim(message: &str) {
    eprintln!("wheezper: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wheezper-remote-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn event(session: &str, text: &str) -> PendingEvent {
        PendingEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            session_id: session.to_string(),
            speaker: "them".into(),
            content: text.into(),
            started_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        }
    }

    fn shared(path: &Path) -> Shared {
        let (pending, compact) = load(path).expect("load");
        Shared {
            queue: Mutex::new(Queue {
                path: path.to_path_buf(),
                file: open_append(path).expect("open"),
                pending,
                acks_since_compact: 0,
            }),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
        }
    }

    #[test]
    fn queue_round_trips_events_and_acks() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("session.jsonl");
        let queue = shared(&path);

        let a = event("s", "first line");
        let b = event("s", "second line");
        queue.enqueue(a.clone()).expect("enqueue a");
        queue.enqueue(b.clone()).expect("enqueue b");
        assert_eq!(queue.len(), 2);
        queue.ack(&a.event_id);
        assert_eq!(queue.len(), 1);

        // A fresh process sees exactly the unacknowledged event, in order.
        let reloaded = shared(&path);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.front().expect("front").content, "second line");
        assert_eq!(reloaded.front().expect("front").event_id, b.event_id);
    }

    #[test]
    fn acked_events_do_not_replay_after_restart() {
        let dir = temp_dir("acked");
        let path = dir.join("session.jsonl");
        let queue = shared(&path);
        let a = event("s", "only line");
        queue.enqueue(a.clone()).expect("enqueue");
        queue.ack(&a.event_id);
        queue.compact().expect("compact");
        assert_eq!(queue.len(), 0);
        assert!(!path.exists() || std::fs::read_to_string(&path).unwrap().trim().is_empty());
        assert_eq!(shared(&path).len(), 0);
    }

    #[test]
    fn corrupt_lines_do_not_lose_the_rest() {
        let dir = temp_dir("corrupt");
        let path = dir.join("session.jsonl");
        let mut file = File::create(&path).expect("create");
        writeln!(file, "not json at all").unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&event("s", "good")).unwrap()
        )
        .unwrap();
        drop(file);
        let queue = shared(&path);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.front().unwrap().content, "good");
    }

    #[test]
    fn publish_writes_to_disk_before_the_worker_sees_it() {
        let dir = temp_dir("durable");
        unsafe { std::env::set_var("WHEEZPER_STATE_DIR", &dir) };
        let session = uuid::Uuid::new_v4().to_string();
        // Port 1 is reliably closed: the worker can never connect.
        let remote = Remote::connect("http://127.0.0.1:1", &session, "token").expect("remote");
        remote.publish(Some("them"), "the server is down");
        let path = queue_path(&session).expect("queue path");
        let content = std::fs::read_to_string(&path).expect("queue file");
        assert!(
            content.contains("the server is down"),
            "event must be on disk: {content}"
        );
        assert_eq!(remote.pending(), 1);
        unsafe { std::env::remove_var("WHEEZPER_STATE_DIR") };
    }

    #[test]
    fn invalid_session_id_is_a_startup_error() {
        assert!(Remote::connect("http://127.0.0.1:1", "not-a-uuid", "token").is_err());
        assert!(
            Remote::connect("http://127.0.0.1:1", &uuid::Uuid::new_v4().to_string(), " ").is_err()
        );
    }
}
