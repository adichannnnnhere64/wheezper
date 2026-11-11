//! wheezper-server: durable session, transcript and assistant backend.
//!
//! The laptop publishes committed transcript events over gRPC; the server
//! persists them before acknowledging, then streams them to connected clients.
//! The assistant answers questions about a meeting without touching the
//! transcript record.

pub mod assistant;
pub mod auth;
pub mod config;
pub mod db;
pub mod email;
pub mod error;
pub mod grpc;
pub mod session;
pub mod time;
pub mod transcript;

use crate::assistant::AssistantService;
use crate::assistant::provider::Ollama;
use crate::auth::Authenticator;
use crate::config::Config;
use crate::grpc::{
    AppState, assistant::AssistantGrpc, auth::AuthGrpc, session::SessionGrpc,
    transcript::TranscriptGrpc,
};
use crate::session::SessionService;
use crate::transcript::{TranscriptService, broadcaster::Broadcaster};
use anyhow::{Context, Result};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tonic::transport::{Identity, Server, ServerTlsConfig};
use wheezper_proto::{
    assistant_service_server::AssistantServiceServer, auth_service_server::AuthServiceServer,
    session_service_server::SessionServiceServer,
    transcript_service_server::TranscriptServiceServer,
};

/// Wire every dependency once; tests reuse this instead of the socket path.
pub async fn build_state(config: Config) -> Result<Arc<AppState>> {
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    let broadcaster = Broadcaster::new(1024);
    let transcripts = TranscriptService::new(pool.clone(), broadcaster);
    let sessions = SessionService::new(pool.clone());
    let provider = Ollama::new(
        &config.assistant_url,
        &config.assistant_model,
        config.assistant_api_key.clone(),
    );
    let assistant = AssistantService::new(
        transcripts.clone(),
        sessions.clone(),
        provider,
        config.assistant_context.clone(),
    );

    let email = email::Email::from_env();
    if let Some(email_config) = &email {
        tracing::info!(relay = %email_config.relay_name(), "meeting notes are emailed on EndSession");
    }

    Ok(Arc::new(AppState {
        auth: Authenticator::new(pool.clone()),
        config: Arc::new(config),
        sessions,
        transcripts,
        assistant,
        email,
    }))
}

/// Run the gRPC server until `shutdown` resolves.
pub async fn serve(
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let addr = config.bind;
    let tls = if config.tls_enabled() {
        Some((
            config.tls_cert.clone().unwrap(),
            config.tls_key.clone().unwrap(),
        ))
    } else {
        None
    };
    let retention_days = config.retention_days;
    let remote_assistant = config.assistant_is_remote();
    let assistant_url = config.assistant_url.clone();

    let state = build_state(config).await?;
    spawn_retention(Arc::clone(&state), retention_days);

    if tls.is_none() {
        tracing::warn!(
            "serving plaintext gRPC; put it behind a TLS terminator or set \
             WHEEZPER_TLS_CERT / WHEEZPER_TLS_KEY"
        );
    }
    if remote_assistant {
        tracing::warn!(
            assistant = %assistant_url,
            "assistant sends transcript text to a remote provider"
        );
    }

    let mut builder = Server::builder();
    if let Some((cert, key)) = tls {
        let cert =
            std::fs::read(&cert).with_context(|| format!("reading TLS cert {}", cert.display()))?;
        let key =
            std::fs::read(&key).with_context(|| format!("reading TLS key {}", key.display()))?;
        builder =
            builder.tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert, key)))?;
    }

    tracing::info!(%addr, "wheezper-server listening");
    builder
        .add_service(SessionServiceServer::new(SessionGrpc {
            state: state.clone(),
        }))
        .add_service(AuthServiceServer::new(AuthGrpc {
            state: state.clone(),
        }))
        .add_service(TranscriptServiceServer::new(TranscriptGrpc {
            state: state.clone(),
        }))
        .add_service(AssistantServiceServer::new(AssistantGrpc { state }))
        .serve_with_shutdown(addr, shutdown)
        .await
        .context("gRPC server")?;
    tracing::info!("wheezper-server stopped");
    Ok(())
}

fn spawn_retention(state: Arc<AppState>, retention_days: i64) {
    tokio::spawn(async move {
        // Clean once at startup (a restart is a natural daily tick) and then
        // daily while the process lives.
        loop {
            if let Err(err) = state.auth.purge_expired_devices().await {
                tracing::warn!(error = %err, "purging expired devices failed");
            }
            if retention_days > 0 {
                match state.sessions.purge_old_completed(retention_days).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(deleted = n, "retention: old sessions deleted"),
                    Err(err) => tracing::warn!(error = %err, "retention cleanup failed"),
                }
            }
            tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
        }
    });
}
