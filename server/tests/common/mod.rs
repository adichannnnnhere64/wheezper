//! Test harness: a real gRPC server on an ephemeral port.
//!
//! PostgreSQL is required. Set `WHEEZPER_TEST_DATABASE_URL` (or
//! `WHEEZPER_DATABASE_URL`) to a disposable database; without one the tests
//! print a skip line instead of failing, so `cargo test` still works on a
//! laptop with no Postgres. `scripts/test-server.sh` boots a throwaway cluster.

#![allow(dead_code)]

use std::time::Duration;
use wheezper_server::config::Config;

pub const PAIRING_CODE: &str = "integration-test-pairing";

pub struct TestServer {
    pub addr: String,
    pub db_url: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<()>,
}

pub fn database_url() -> Option<String> {
    ["WHEEZPER_TEST_DATABASE_URL", "WHEEZPER_DATABASE_URL"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|url| !url.trim().is_empty())
}

pub fn skip_reason() -> String {
    "skipping integration test: set WHEEZPER_TEST_DATABASE_URL to a disposable PostgreSQL".into()
}

impl TestServer {
    pub async fn start() -> Option<Self> {
        let db_url = database_url()?;
        Self::start_with(db_url, "http://127.0.0.1:1".to_string()).await
    }

    /// Start against a specific assistant endpoint (usually a mock Ollama).
    pub async fn start_with(db_url: String, assistant_url: String) -> Option<Self> {
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");
        let config = Config {
            database_url: db_url.clone(),
            bind: addr.parse().expect("addr"),
            pairing_code: PAIRING_CODE.to_string(),
            device_ttl: Duration::from_secs(3600),
            retention_days: 0,
            assistant_url,
            assistant_model: "test-model".to_string(),
            assistant_api_key: None,
            // Tests hit a mock on 127.0.0.1, so `auto` would keep the short
            // interview window; `full` exercises the whole-meeting prompt.
            assistant_context: "full".to_string(),
            tls_cert: None,
            tls_key: None,
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            if let Err(err) = wheezper_server::serve(config, async {
                let _ = rx.await;
            })
            .await
            {
                eprintln!("test server exited: {err:#}");
            }
        });

        wait_ready(&addr).await;
        Some(Self {
            addr,
            db_url,
            shutdown: Some(tx),
            handle,
        })
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub async fn channel(&self) -> tonic::transport::Channel {
        tonic::transport::Channel::from_shared(self.endpoint())
            .expect("endpoint")
            .connect()
            .await
            .expect("connect to test server")
    }

    pub async fn pool(&self) -> sqlx::PgPool {
        sqlx::PgPool::connect(&self.db_url).await.expect("db pool")
    }

    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), self.handle).await;
    }

    /// Simulate a server restart: same database, new process state.
    pub async fn restart(self) -> Self {
        let db_url = self.db_url.clone();
        self.stop().await;
        Self::start_with(db_url, "http://127.0.0.1:1".to_string())
            .await
            .expect("restart test server")
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

async fn wait_ready(addr: &str) {
    let endpoint = format!("http://{addr}");
    for _ in 0..200 {
        let connected = tokio::time::timeout(
            Duration::from_millis(250),
            tonic::transport::Channel::from_shared(endpoint.clone())
                .expect("endpoint")
                .connect(),
        )
        .await;
        if matches!(connected, Ok(Ok(_))) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("test server at {addr} never became ready");
}

// --- client helpers --------------------------------------------------------

pub fn request<T>(message: T, token: &str) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .expect("ascii bearer token"),
    );
    request
}

pub async fn pair(channel: &tonic::transport::Channel, name: &str) -> String {
    use wheezper_proto::auth_service_client::AuthServiceClient;
    AuthServiceClient::new(channel.clone())
        .pair_device(wheezper_proto::PairDeviceRequest {
            pairing_code: PAIRING_CODE.to_string(),
            device_name: name.to_string(),
        })
        .await
        .expect("pair device")
        .into_inner()
        .device_token
}

pub async fn create_session(
    channel: &tonic::transport::Channel,
    device_token: &str,
    title: &str,
) -> (String, String) {
    create_session_with_ai(channel, device_token, title, false).await
}

pub async fn create_session_with_ai(
    channel: &tonic::transport::Channel,
    device_token: &str,
    title: &str,
    ai_help: bool,
) -> (String, String) {
    use wheezper_proto::session_service_client::SessionServiceClient;
    let response = SessionServiceClient::new(channel.clone())
        .create_session(request(
            wheezper_proto::CreateSessionRequest {
                title: title.to_string(),
                ai_help,
            },
            device_token,
        ))
        .await
        .expect("create session")
        .into_inner();
    (
        response.session.expect("session").id,
        response.session_token,
    )
}

pub fn event(
    session_id: &str,
    event_id: &str,
    speaker: &str,
    content: &str,
) -> wheezper_proto::TranscriptEvent {
    wheezper_proto::TranscriptEvent {
        event_id: event_id.to_string(),
        session_id: session_id.to_string(),
        sequence_number: 0,
        speaker: speaker.to_string(),
        content: content.to_string(),
        started_at: Some(prost_types::Timestamp {
            seconds: 1_760_000_000,
            nanos: 0,
        }),
        received_at: None,
        source: "wheezper".to_string(),
    }
}

pub fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
