//! Bounded asynchronous meeting annotations.

use crate::llm::{self, Llm};
use crate::notepad::AnnotationKey;
use anyhow::{Context, Result, bail};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const DEFAULT_BUDGET: u32 = 256;
const QUEUE_SIZE: usize = 4;
const CALLS_PER_MINUTE: usize = 10;
/// Annotation inference is best-effort; one worker thread must not steal a
/// core from the provisional tier (its 0.5 s budget is the priority).
const DEFAULT_AGENT_THREADS: i32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub struct AgentSpec {
    pub provider: String,
    pub model: String,
    pub budget: u32,
}

impl AgentSpec {
    pub fn parse(value: &str) -> Result<Self> {
        let (provider, model_and_budget) = value
            .split_once(':')
            .context("--with-agent must be PROVIDER:MODEL[:BUDGET]")?;
        if !matches!(provider, "ollama" | "ollama-cloud" | "builtin") {
            bail!(
                "unknown --with-agent provider '{provider}' (use ollama, ollama-cloud, or builtin)"
            );
        }
        let (model, budget) = match model_and_budget.rsplit_once(':') {
            Some((model, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
                let budget = tail
                    .parse::<u32>()
                    .context("agent token budget is too large")?;
                if budget == 0 {
                    bail!("agent token budget must be positive");
                }
                (model, budget)
            }
            _ => (model_and_budget, DEFAULT_BUDGET),
        };
        if model.trim().is_empty() {
            bail!("--with-agent model must not be empty");
        }
        Ok(Self {
            provider: provider.to_string(),
            model: model.to_string(),
            budget,
        })
    }
}

struct Job {
    target: AnnotationKey,
    channel: Option<String>,
    context: String,
}

pub struct Annotation {
    pub target: AnnotationKey,
    pub text: String,
}

pub struct Agent {
    tx: mpsc::SyncSender<Job>,
    completed: mpsc::Receiver<Annotation>,
    /// Jobs queued or running, including those whose answer is not yet
    /// collected. Used to drain briefly on stop without waiting forever.
    pending: Arc<AtomicUsize>,
    recent_calls: VecDeque<Instant>,
    warned_drop: bool,
}

impl Agent {
    pub fn new(
        raw_spec: &str,
        domain_prompt: Option<&str>,
        threads: i32,
    ) -> Result<Self> {
        let spec = AgentSpec::parse(raw_spec)?;
        let agent_threads = std::env::var("WHEEZPER_AGENT_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_AGENT_THREADS)
            .clamp(1, threads.max(1));
        let (backend, endpoint) = match spec.provider.as_str() {
            "ollama" => {
                let server = llm::Ollama::new("http://localhost:11434", &spec.model);
                server
                    .check()
                    .context("validating annotation Ollama model")?;
                let endpoint = format!("{} @ {}", server.model, server.url);
                (Llm::Server(server), endpoint)
            }
            "ollama-cloud" => {
                if llm::ollama_api_key().is_none() {
                    bail!("--with-agent ollama-cloud requires OLLAMA_API_KEY (environment or ./.env)");
                }
                let server = llm::Ollama::new("https://ollama.com/api", &spec.model);
                server
                    .check()
                    .context("validating annotation Ollama Cloud model")?;
                let endpoint = format!("{} @ {}", server.model, server.url);
                (Llm::Server(server), endpoint)
            }
            "builtin" => {
                #[cfg(feature = "llm")]
                {
                    let model = llm::Builtin::new(&spec.model, agent_threads)
                        .context("validating built-in annotation model")?;
                    let endpoint = format!("{} (built-in llama.cpp)", model.model_name);
                    (Llm::Builtin(model), endpoint)
                }
                #[cfg(not(feature = "llm"))]
                {
                    let _ = agent_threads;
                    bail!(
                        "--with-agent provider 'builtin' is unavailable in this build (compiled with --no-default-features)"
                    );
                }
            }
            _ => bail!("unknown --with-agent provider '{}'", spec.provider),
        };

        let mut system = String::from(
            "You annotate live meeting transcripts. Give one concise, useful observation, such as a decision, date, owner, action item, or unclear reference. Use only facts in the supplied context; do not invent or restate the transcript. If there is nothing useful to add, reply exactly SKIP.",
        );
        if let Some(prompt) = domain_prompt.filter(|p| !p.trim().is_empty()) {
            system.push_str("\nMeeting context: ");
            system.push_str(prompt);
        }

        eprintln!(
            "wheezper: agent={endpoint} (max {}/min, {} output tokens)",
            CALLS_PER_MINUTE, spec.budget
        );
        let (tx, rx) = mpsc::sync_channel::<Job>(QUEUE_SIZE);
        let (completed_tx, completed) = mpsc::sync_channel::<Annotation>(QUEUE_SIZE);
        let warned_failure = Arc::new(AtomicBool::new(false));
        let worker_warned = Arc::clone(&warned_failure);
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = Arc::clone(&pending);
        let model = spec.model.clone();
        let budget = spec.budget;
        std::thread::Builder::new()
            .name("annotation-agent".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let _guard = PendingGuard(Arc::clone(&worker_pending));
                    let response = match backend.generate(&system, &job.context, budget) {
                        Ok(response) => response,
                        Err(error) => {
                            warn_once(&worker_warned, format!("agent request failed: {error:#}"));
                            continue;
                        }
                    };
                    let response = response.split_whitespace().collect::<Vec<_>>().join(" ");
                    if response.is_empty() || response.eq_ignore_ascii_case("skip") {
                        continue;
                    }
                    let channel = job.channel.as_deref().unwrap_or("meeting");
                    let annotation = format!("  ↳ 🤖 {channel} / {model}: ANS: {response}");
                    if completed_tx
                        .send(Annotation {
                            target: job.target,
                            text: annotation,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .context("spawning annotation agent")?;

        Ok(Self {
            tx,
            completed,
            pending,
            recent_calls: VecDeque::new(),
            warned_drop: false,
        })
    }

    /// Collect finished calls without waiting for the worker.
    pub fn drain_completed(&self) -> Vec<Annotation> {
        self.completed.try_iter().collect()
    }

    /// Jobs queued or running; zero means every answer has been handed back.
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }

    /// Queue a useful-sized transcript line without waiting for inference.
    pub fn submit(
        &mut self,
        target: AnnotationKey,
        channel: Option<&str>,
        line: &str,
        context: &str,
    ) {
        if line.split_whitespace().count() < 3 {
            return;
        }
        let now = Instant::now();
        while self
            .recent_calls
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            self.recent_calls.pop_front();
        }
        if self.recent_calls.len() >= CALLS_PER_MINUTE {
            self.warn_drop("call limit reached");
            return;
        }
        let job = Job {
            target,
            channel: channel.map(str::to_string),
            context: context.to_string(),
        };
        // Count before sending: the worker may finish before try_send returns.
        self.pending.fetch_add(1, Ordering::Relaxed);
        match self.tx.try_send(job) {
            Ok(()) => self.recent_calls.push_back(now),
            Err(mpsc::TrySendError::Full(_)) => {
                self.pending.fetch_sub(1, Ordering::Relaxed);
                self.warn_drop("queue full");
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.pending.fetch_sub(1, Ordering::Relaxed);
                self.warn_drop("worker stopped");
            }
        }
    }

    fn warn_drop(&mut self, reason: &str) {
        if !self.warned_drop {
            eprintln!("wheezper: warning: annotation dropped ({reason}); transcription continues");
            self.warned_drop = true;
        }
    }
}

/// Decrements the pending count however the worker leaves a job (answer,
/// failure, SKIP, or a panic).
struct PendingGuard(Arc<AtomicUsize>);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn warn_once(warned: &AtomicBool, message: String) {
    if !warned.swap(true, Ordering::Relaxed) {
        eprintln!("wheezper: warning: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::AgentSpec;

    #[test]
    fn parses_provider_model_and_budget_without_splitting_model_colons() {
        assert_eq!(
            AgentSpec::parse("ollama-cloud:deepseek-v4.1-flash:1017").unwrap(),
            AgentSpec {
                provider: "ollama-cloud".into(),
                model: "deepseek-v4.1-flash".into(),
                budget: 1017
            }
        );
        assert_eq!(
            AgentSpec::parse("ollama:namespace:model:42").unwrap(),
            AgentSpec {
                provider: "ollama".into(),
                model: "namespace:model".into(),
                budget: 42
            }
        );
        assert_eq!(AgentSpec::parse("builtin:gemma3-1b").unwrap().budget, 256);
    }

    #[test]
    fn rejects_invalid_provider_model_and_budget() {
        assert!(AgentSpec::parse("unknown:model").is_err());
        assert!(AgentSpec::parse("ollama:").is_err());
        assert!(AgentSpec::parse("ollama:model:0").is_err());
        assert!(AgentSpec::parse("ollama:model:999999999999").is_err());
    }
}
