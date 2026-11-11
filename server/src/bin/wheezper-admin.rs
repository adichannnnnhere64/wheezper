//! Operator/CI helper for wheezper-server: pair a device, create a session,
//! inspect or sync a transcript, ask the assistant.
//!
//! The Android app does the same calls; this binary exists so the backend can
//! be exercised from a shell (and by `scripts/test-session-sync.sh`).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::time::Duration;
use tonic::transport::{Channel, ClientTlsConfig};
use wheezper_proto as proto;

#[derive(Parser)]
#[command(version, about = "wheezper-server operator CLI")]
struct Cli {
    /// gRPC endpoint, e.g. http://127.0.0.1:50051
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    server: String,

    /// Device token from `pair` (ignored by `pair` itself)
    #[arg(long, global = true)]
    token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Exchange the server's pairing code for a short-lived device token
    Pair {
        #[arg(long)]
        code: String,
        #[arg(long, default_value = "admin-cli")]
        name: String,
    },
    /// Create a meeting session and print its id + session token
    Create {
        #[arg(long, default_value = "Untitled meeting")]
        title: String,
        /// Mark the session as interview-assist (the app auto-answers questions)
        #[arg(long)]
        ai_help: bool,
    },
    /// List sessions
    List {
        #[arg(long, default_value_t = 20)]
        limit: i32,
        #[arg(long)]
        active_only: bool,
    },
    /// Show one session
    Show {
        #[arg(long)]
        session: String,
    },
    /// End a session
    End {
        #[arg(long)]
        session: String,
    },
    /// Print committed transcript lines (catch-up sync)
    Sync {
        #[arg(long)]
        session: String,
        #[arg(long, default_value_t = 0)]
        from: i64,
        #[arg(long, default_value_t = 500)]
        limit: i32,
    },
    /// Ask the meeting assistant a question
    Ask {
        #[arg(long)]
        session: String,
        #[arg(long)]
        question: String,
        /// Model name override (default: the server's configured model)
        #[arg(long)]
        model: Option<String>,
        /// Per-request provider token (Ollama Cloud). Never stored server-side.
        #[arg(long, value_name = "TOKEN")]
        assistant_token: Option<String>,
        /// Answer through https://ollama.com/api instead of the server URL
        #[arg(long)]
        cloud: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cli {
        server,
        token,
        command,
    } = Cli::parse();
    let mut endpoint = Channel::from_shared(server.clone())
        .with_context(|| format!("invalid --server URL: {server}"))?
        .connect_timeout(Duration::from_secs(10));
    // A remote deployment is reachable only over TLS (the server itself may be
    // plaintext behind a terminator, e.g. a Cloudflare tunnel).
    if server.starts_with("https://") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .context("configure TLS")?;
    }
    let channel = endpoint
        .connect()
        .await
        .with_context(|| format!("connecting to {server}"))?;

    match command {
        Command::Pair { code, name } => {
            let response = proto::auth_service_client::AuthServiceClient::new(channel)
                .pair_device(proto::PairDeviceRequest {
                    pairing_code: code,
                    device_name: name,
                })
                .await?
                .into_inner();
            println!("{}", response.device_token);
            if let Some(expires) = response.expires_at {
                eprintln!("device token expires at unix {}", expires.seconds);
            }
        }
        Command::Create { title, ai_help } => {
            let token = device_token(&token)?;
            let response = proto::session_service_client::SessionServiceClient::new(channel)
                .create_session(with_token(
                    proto::CreateSessionRequest { title, ai_help },
                    token,
                ))
                .await?
                .into_inner();
            let session = response.session.context("server returned no session")?;
            println!("session={}", session.id);
            println!("token={}", response.session_token);
        }
        Command::List { limit, active_only } => {
            let token = device_token(&token)?;
            let response = proto::session_service_client::SessionServiceClient::new(channel)
                .list_sessions(with_token(
                    proto::ListSessionsRequest { limit, active_only },
                    token,
                ))
                .await?
                .into_inner();
            for session in response.sessions {
                println!(
                    "{}  {:<10}  {:>6} lines  {}{}",
                    session.id,
                    status(&session),
                    session.transcript_count,
                    if session.ai_help { "[ai] " } else { "" },
                    session.title
                );
            }
        }
        Command::Show { session } => {
            let token = device_token(&token)?;
            let response = proto::session_service_client::SessionServiceClient::new(channel)
                .get_session(with_token(
                    proto::GetSessionRequest {
                        session_id: session,
                    },
                    token,
                ))
                .await?
                .into_inner();
            println!(
                "{}  {:<10}  {} lines  {}",
                response.id,
                status(&response),
                response.transcript_count,
                response.title
            );
        }
        Command::End { session } => {
            let token = device_token(&token)?;
            let response = proto::session_service_client::SessionServiceClient::new(channel)
                .end_session(with_token(
                    proto::EndSessionRequest {
                        session_id: session,
                    },
                    token,
                ))
                .await?
                .into_inner();
            let session = response.session.context("server returned no session")?;
            println!("{}  {}", session.id, status(&session));
        }
        Command::Sync {
            session,
            from,
            limit,
        } => {
            let token = device_token(&token)?;
            let mut from = from;
            loop {
                let page =
                    proto::transcript_service_client::TranscriptServiceClient::new(channel.clone())
                        .sync_transcripts(with_token(
                            proto::SyncTranscriptsRequest {
                                session_id: session.clone(),
                                from_sequence: from,
                                limit,
                            },
                            token.clone(),
                        ))
                        .await?
                        .into_inner();
                for event in &page.events {
                    println!(
                        "[{:>3}] {} {}: {}",
                        event.sequence_number,
                        event
                            .started_at
                            .as_ref()
                            .map(|t| timestamp(t.seconds))
                            .unwrap_or_default(),
                        if event.speaker.is_empty() {
                            "-"
                        } else {
                            &event.speaker
                        },
                        event.content
                    );
                }
                if !page.has_more {
                    eprintln!("latest_sequence={}", page.latest_sequence);
                    break;
                }
                from = page
                    .events
                    .last()
                    .map(|e| e.sequence_number)
                    .unwrap_or(from);
            }
        }
        Command::Ask {
            session,
            question,
            model,
            assistant_token,
            cloud,
        } => {
            let token = device_token(&token)?;
            let response = proto::assistant_service_client::AssistantServiceClient::new(channel)
                .ask_question(with_token(
                    proto::AskQuestionRequest {
                        session_id: session,
                        question,
                        model: model.unwrap_or_default(),
                        api_key: assistant_token.unwrap_or_default(),
                        backend: if cloud {
                            proto::AssistantBackend::OllamaCloud as i32
                        } else {
                            proto::AssistantBackend::Default as i32
                        },
                    },
                    token,
                ))
                .await?
                .into_inner();
            println!("{}", response.answer);
            eprintln!("({} / {})", response.provider, response.model);
        }
    }
    Ok(())
}

fn device_token(token: &Option<String>) -> Result<String> {
    token
        .clone()
        .context("--token is required for this command (run `pair` first)")
}

fn with_token<T>(message: T, token: String) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    let value = format!("Bearer {token}")
        .parse()
        .expect("token must be ASCII");
    request.metadata_mut().insert("authorization", value);
    request
}

fn status(session: &proto::Session) -> &'static str {
    match proto::SessionStatus::try_from(session.status) {
        Ok(proto::SessionStatus::Created) => "created",
        Ok(proto::SessionStatus::Active) => "active",
        Ok(proto::SessionStatus::Completed) => "completed",
        _ => "unknown",
    }
}

fn timestamp(seconds: i64) -> String {
    match chrono::DateTime::from_timestamp(seconds, 0) {
        Some(dt) => dt.format("%H:%M:%S").to_string(),
        None => String::new(),
    }
}
