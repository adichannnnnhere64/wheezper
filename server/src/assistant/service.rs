//! Assistant use-cases: validate, retrieve context, generate, persist.
//!
//! Answers are suggestions. They are stored next to the session but never
//! inserted into `transcript_entries`, so a transcript stays an exact record of
//! what was said.
//!
//! Per-request overrides let the phone pick a model (and, for Ollama Cloud, its
//! own API token). The token is used for that call only: it is never persisted
//! and never logged. The endpoint itself is never client-supplied — there is a
//! fixed allowlist (server-configured Ollama or `ollama.com`), so the RPC
//! cannot be turned into an SSRF probe.

use super::context;
use super::provider::{OLLAMA_CLOUD_URL, Ollama, Prompt};
use super::repository;
use crate::error::{Error, Result};
use crate::session::SessionService;
use crate::transcript::service::TranscriptService;
use futures_util::{Stream, StreamExt};
use std::pin::Pin;
use uuid::Uuid;

const MAX_QUESTION_CHARS: usize = 2_000;
const MAX_MODEL_CHARS: usize = 80;
const MAX_TOKEN_CHARS: usize = 512;

#[derive(Clone, Default)]
pub struct AssistantOptions {
    /// `None`/empty = the server's configured model.
    pub model: Option<String>,
    /// Per-request provider token. Never stored or logged.
    pub api_key: Option<String>,
    /// Answer through `https://ollama.com/api` instead of the configured URL.
    pub cloud: bool,
    /// Which paired device is asking; selects that device's profile (multi-user).
    pub device_id: Option<Uuid>,
}

pub struct Answer {
    pub text: String,
    pub provider: String,
    pub model: String,
}

/// A provider resolved for one call.
pub struct AssistantProvider {
    ollama: Ollama,
    pub name: &'static str,
    pub model: String,
}

impl AssistantProvider {
    fn provider_name(&self) -> &'static str {
        self.name
    }
}

#[derive(Clone)]
pub struct AssistantService {
    transcripts: TranscriptService,
    sessions: SessionService,
    provider: Ollama,
}

impl AssistantService {
    pub fn new(transcripts: TranscriptService, sessions: SessionService, provider: Ollama) -> Self {
        Self {
            transcripts,
            sessions,
            provider,
        }
    }

    /// Resolve the model/token/backend for one request. Called by the gRPC edge
    /// first so an invalid token or model is a normal status, not a stream
    /// error halfway through.
    pub fn resolve(&self, options: &AssistantOptions) -> Result<AssistantProvider> {
        resolve_provider(&self.provider, options)
    }

    /// One-shot answer: a single provider call, no stream plumbing.
    pub async fn ask(
        &self,
        session_id: Uuid,
        question: String,
        options: &AssistantOptions,
    ) -> Result<Answer> {
        let provider = self.resolve(options)?;
        let question = validate(question)?;
        let session = self.sessions.get(session_id).await?;
        let mode = context::Mode::for_session(session.ai_help);
        let ctx = context::build(
            &self.transcripts,
            session_id,
            &session.title,
            &question,
            mode,
            options.device_id,
        )
        .await?;

        // Card-first: a curated answer is better than anything this model will
        // compose, and it cannot invent experience. The keyword gate in the
        // query already decided the card is relevant.
        if let Some(card) = ctx.cards.first() {
            let answer = card.answer.trim().to_string();
            persist(
                &self.transcripts,
                session_id,
                &question,
                &answer,
                "card",
                "curated",
            )
            .await;
            tracing::info!(session = %session_id, rank = card.rank, "answered from answer card");
            return Ok(Answer {
                text: answer,
                provider: "card".to_string(),
                model: "curated".to_string(),
            });
        }

        let (system, user) = ctx.messages(&question, mode);

        // A meeting with no transcript has nothing to answer from: answer that
        // deterministically instead of letting a small model invent content.
        if mode == context::Mode::Meeting && ctx.total_entries == 0 {
            let answer = "The meeting has no transcript yet, so there is nothing to answer from."
                .to_string();
            persist(&self.transcripts, session_id, &question, &answer, "server", "none").await;
            return Ok(Answer {
                text: answer,
                provider: "server".to_string(),
                model: "none".to_string(),
            });
        }

        // No card matched. The local 1.5B model cannot be trusted to answer an
        // open interview question: with the same prompt it invented "I used
        // Kubernetes with Helm and GitOps" and, earlier, CrewAI. So only a model
        // the user chose themselves (their own cloud token) may answer freely;
        // otherwise say there is no prepared answer.
        if mode == context::Mode::Interview && !uses_own_cloud_model(options) {
            let answer = "No prepared answer for that one yet. Add it under Settings -> \
                          Answers (answer cards) and it will be answered reliably."
                .to_string();
            persist(&self.transcripts, session_id, &question, &answer, "server", "none").await;
            return Ok(Answer {
                text: answer,
                provider: "server".to_string(),
                model: "none".to_string(),
            });
        }

        let prompt = match mode {
            context::Mode::Interview => Prompt::interview(system, user),
            context::Mode::Meeting => Prompt::new(system, user),
        };
        tracing::info!(
            session = %session_id,
            entries = ctx.total_entries,
            provider = provider.provider_name(),
            model = %provider.model,
            "assistant question"
        );

        let answer = provider.ollama.generate(&prompt).await?;
        persist(
            &self.transcripts,
            session_id,
            &question,
            &answer,
            provider.provider_name(),
            &provider.model,
        )
        .await;

        Ok(Answer {
            text: answer,
            provider: provider.name.to_string(),
            model: provider.model,
        })
    }

    /// Stream answer deltas. Errors are yielded once, after partial text if
    /// any; the caller maps them to gRPC status codes.
    pub fn stream(
        &self,
        session_id: Uuid,
        question: String,
        provider: AssistantProvider,
        device_id: Option<Uuid>,
        // Caller brought their own (bigger) model, so an uncarded interview
        // question may be answered by it instead of refused.
        own_model: bool,
    ) -> Pin<Box<dyn Stream<Item = Result<String>> + Send>> {
        let transcripts = self.transcripts.clone();
        let sessions = self.sessions.clone();
        let pool = transcripts.pool().clone();
        let provider_name = provider.provider_name();
        let model = provider.model.clone();

        Box::pin(async_stream::try_stream! {
            let question = validate(question)?;
            let session = sessions.get(session_id).await?;
            let mode = context::Mode::for_session(session.ai_help);
            let ctx = context::build(
                &transcripts,
                session_id,
                &session.title,
                &question,
                mode,
                device_id,
            )
            .await?;
            // Card-first, same rule as the non-streaming path.
            if let Some(card) = ctx.cards.first() {
                let answer = card.answer.trim().to_string();
                yield answer.clone();
                if let Err(err) = repository::save(
                    &pool, session_id, &question, &answer, "card", "curated",
                )
                .await
                {
                    tracing::warn!(error = %err, "could not persist assistant message");
                }
                return;
            }
            let (system, user) = ctx.messages(&question, mode);

            // Same deterministic guard as the unary path.
            if mode == context::Mode::Meeting && ctx.total_entries == 0 {
                let answer = "The meeting has no transcript yet, so there is nothing to answer from."
                    .to_string();
                yield answer.clone();
                if let Err(err) =
                    repository::save(&pool, session_id, &question, &answer, "server", "none").await
                {
                    tracing::warn!(error = %err, "could not persist assistant message");
                }
                return;
            }

            let prompt = match mode {
                context::Mode::Interview => Prompt::interview(system, user),
                context::Mode::Meeting => Prompt::new(system, user),
            };
            if mode == context::Mode::Interview && !own_model {
                let answer = "No prepared answer for that one yet. Add it under Settings -> \
                              Answers (answer cards) and it will be answered reliably.";
                yield answer.to_string();
                if let Err(err) = repository::save(
                    &pool, session_id, &question, answer, "server", "none",
                )
                .await
                {
                    tracing::warn!(error = %err, "could not persist assistant message");
                }
                return;
            }
            tracing::info!(
                session = %session_id,
                entries = ctx.total_entries,
                provider = provider_name,
                model = %model,
                "assistant question"
            );

            let mut answer = String::new();
            let mut deltas = Box::pin(provider.ollama.stream(prompt));
            while let Some(delta) = deltas.next().await {
                let delta = delta?;
                answer.push_str(&delta);
                yield delta;
            }
            if answer.trim().is_empty() {
                Err(Error::provider("the model returned an empty answer"))?;
            }

            // Persistence is best-effort: a storage hiccup must not swallow
            // an answer the user already received.
            if let Err(err) = repository::save(
                &pool,
                session_id,
                &question,
                answer.trim(),
                provider_name,
                &model,
            )
            .await
            {
                tracing::warn!(error = %err, "could not persist assistant message");
            }
        })
    }

    pub async fn message_count(&self, session_id: Uuid) -> Result<i64> {
        repository::count(self.transcripts.pool(), session_id).await
    }
}

fn validate(question: String) -> Result<String> {
    let question = question.trim();
    if question.is_empty() {
        return Err(Error::invalid("question is empty"));
    }
    if question.chars().count() > MAX_QUESTION_CHARS {
        return Err(Error::invalid(format!(
            "question is longer than {MAX_QUESTION_CHARS} characters"
        )));
    }
    Ok(question.to_string())
}

fn validate_model(model: &str) -> Result<String> {
    let model = model.trim();
    if model.chars().count() > MAX_MODEL_CHARS
        || !model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'))
    {
        return Err(Error::invalid(
            "model name is invalid (letters, digits and . _ : / - only)",
        ));
    }
    Ok(model.to_string())
}

/// Validate but never echo the token: a rejected credential must not end up in
/// a log line or a client-visible error.
fn validate_token(token: &str) -> Result<String> {
    let token = token.trim();
    if token.is_empty()
        || token.chars().count() > MAX_TOKEN_CHARS
        || !token.is_ascii()
        || !token.chars().all(|c| c.is_ascii_graphic())
    {
        return Err(Error::invalid("API token is invalid"));
    }
    Ok(token.to_string())
}

/// True when the caller brought their own bigger model. Those answers are
/// allowed to go beyond the answer cards; the local model is not.
pub fn uses_own_cloud_model(options: &AssistantOptions) -> bool {
    options.cloud
        && options
            .api_key
            .as_deref()
            .map(|key| !key.trim().is_empty())
            .unwrap_or(false)
}

/// Store the question/answer pair; never fatal for the caller.
async fn persist(
    transcripts: &TranscriptService,
    session_id: Uuid,
    question: &str,
    answer: &str,
    provider: &str,
    model: &str,
) {
    if let Err(err) = repository::save(
        transcripts.pool(),
        session_id,
        question,
        answer.trim(),
        provider,
        model,
    )
    .await
    {
        tracing::warn!(error = %err, "could not persist assistant message");
    }
}

/// Pure model/token/backend resolution; no database, no network.
fn resolve_provider(default: &Ollama, options: &AssistantOptions) -> Result<AssistantProvider> {
    let model = match options
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        Some(model) => validate_model(model)?,
        None => default.model().to_string(),
    };

    let key = match options
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        Some(key) => Some(validate_token(key)?),
        // Fall back to the operator's key (Ollama Cloud deployments).
        None => default.api_key.clone(),
    };

    let (url, name) = if options.cloud {
        if key.is_none() {
            return Err(Error::invalid(
                "Ollama Cloud needs an API token: add one in the app's AI settings",
            ));
        }
        (OLLAMA_CLOUD_URL, "ollama-cloud")
    } else {
        (default.url.as_str(), "ollama")
    };

    Ok(AssistantProvider {
        ollama: Ollama::new(url, &model, key),
        name,
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> Ollama {
        Ollama::new("http://localhost:11434", "gemma3:1b", None)
    }

    #[test]
    fn default_options_use_the_server_provider() {
        let provider = resolve_provider(&service(), &AssistantOptions::default()).unwrap();
        assert_eq!(provider.name, "ollama");
        assert_eq!(provider.model, "gemma3:1b");
        assert_eq!(provider.ollama.url, "http://localhost:11434");
    }

    #[test]
    fn model_and_token_override_per_request() {
        let provider = resolve_provider(
            &service(),
            &AssistantOptions {
                model: Some("deepseek-v4.1-flash".into()),
                api_key: Some("user-token".into()),
                cloud: true,
                device_id: None,
            },
        )
        .unwrap();
        assert_eq!(provider.name, "ollama-cloud");
        assert_eq!(provider.model, "deepseek-v4.1-flash");
        assert_eq!(provider.ollama.url, OLLAMA_CLOUD_URL);
        assert_eq!(provider.ollama.api_key.as_deref(), Some("user-token"));
    }

    #[test]
    fn cloud_without_a_token_is_rejected() {
        let err = match resolve_provider(
            &service(),
            &AssistantOptions {
                cloud: true,
                ..Default::default()
            },
        ) {
            Ok(_) => panic!("cloud needs a token"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("API token"));
    }

    #[test]
    fn invalid_model_and_token_are_rejected_without_echoing_the_token() {
        let default = service();
        for model in ["bad model", &"m".repeat(81)] {
            assert!(
                resolve_provider(
                    &default,
                    &AssistantOptions {
                        model: Some(model.into()),
                        ..Default::default()
                    }
                )
                .is_err()
            );
        }
        let err = match resolve_provider(
            &default,
            &AssistantOptions {
                api_key: Some("has space".into()),
                ..Default::default()
            },
        ) {
            Ok(_) => panic!("invalid token must be rejected"),
            Err(err) => err,
        };
        assert!(!err.to_string().contains("has space"));
    }
}
