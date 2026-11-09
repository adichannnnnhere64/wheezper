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
/// A reasoning model's chain-of-thought would otherwise eat the answer.
const INTERVIEW_MAX_TOKENS: u32 = 96;
/// One conversational sentence; the prompt asks for 10-20 words.
const INTERVIEW_MAX_WORDS: usize = 20;
/// Keep the knowledgebase out of the prompt-budget danger zone.
const KB_MAX_CHARS: usize = 4_000;
/// Annotation inference is best-effort; one worker thread must not steal a
/// core from the provisional tier (its 0.5 s budget is the priority).
const DEFAULT_AGENT_THREADS: i32 = 1;

/// Read an interview knowledgebase, bounded so a large file cannot crowd out
/// the transcript or the model's context window.
pub fn load_knowledgebase(path: &std::path::Path) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading knowledgebase {}", path.display()))?;
    let text = raw.trim();
    if text.is_empty() {
        bail!("knowledgebase {} is empty", path.display());
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.len() + c.len_utf8() > KB_MAX_CHARS {
            out.push('…');
            break;
        }
        out.push(c);
    }
    Ok(out)
}

/// Keep a generated interview answer to one short, quote-free sentence.
fn concise_answer(text: &str) -> String {
    let text = text.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    let text = if text
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("ANS:"))
    {
        text[4..].trim()
    } else {
        text
    };
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= INTERVIEW_MAX_WORDS {
        return text.to_string();
    }
    let clipped = words[..INTERVIEW_MAX_WORDS].join(" ");
    // Prefer a complete sentence, then a clause boundary, so a budget cut
    // does not leave the answer hanging on "and…".
    if let Some(end) = clipped.rfind(['.', '!', '?']) {
        return clipped[..=end].to_string();
    }
    if let Some(end) = clipped.rfind([',', ';', ':']) {
        let clause = clipped[..end].trim_end();
        if clause.split_whitespace().count() >= 5 {
            return format!("{clause}.");
        }
    }
    format!("{clipped}…")
}

fn agent_system_prompt(
    interview: bool,
    knowledgebase: Option<&str>,
    domain_prompt: Option<&str>,
    kb_fallback: bool,
) -> String {
    let mut system = if let Some(kb) = knowledgebase.filter(|kb| !kb.trim().is_empty()) {
        // The default is strict: kb silence means SKIP, so the agent can never
        // invent experience. --kb-fallback relaxes only the general/technical
        // case; personal facts stay kb-bound either way.
        let missing = if kb_fallback {
            "If the knowledgebase does not cover the question, answer general and technical questions from your own knowledge; still never invent facts about your own experience."
        } else {
            "If the question cannot be answered from the knowledgebase, reply exactly SKIP."
        };
        format!(
            "You are the candidate in a live job interview. Answer the interviewer's latest question or interview prompt in the first person, using the knowledgebase and the interview transcript. Reply with one concise, conversational, interview-ready sentence of 10-20 words. Do not add analysis, notes, or explanations; do not invent facts. For greetings, thanks, small talk, or statements that are not interview questions, reply exactly SKIP. {missing}\n\nKnowledgebase:\n{kb}"
        )
    } else {
        String::from(
            "You annotate live meeting transcripts. Give one concise, useful observation, such as a decision, date, owner, action item, or unclear reference. Use only facts in the supplied context; do not invent or restate the transcript. If there is nothing useful to add, reply exactly SKIP.",
        )
    };
    if let Some(prompt) = domain_prompt.filter(|p| !p.trim().is_empty()) {
        system.push_str(if interview { "\n\nInterview context: " } else { "\nMeeting context: " });
        system.push_str(prompt);
    }
    system
}

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
    /// Knowledgebase mode: answer the interviewer instead of annotating.
    interview: bool,
    recent_calls: VecDeque<Instant>,
    warned_drop: bool,
}

impl Agent {
    pub fn new(
        raw_spec: &str,
        domain_prompt: Option<&str>,
        threads: i32,
        knowledgebase: Option<&str>,
        kb_fallback: bool,
    ) -> Result<Self> {
        let spec = AgentSpec::parse(raw_spec)?;
        let knowledgebase = knowledgebase.filter(|kb| !kb.trim().is_empty());
        let interview = knowledgebase.is_some();
        let budget = if interview { spec.budget.min(INTERVIEW_MAX_TOKENS) } else { spec.budget };
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

        let system = agent_system_prompt(interview, knowledgebase, domain_prompt, kb_fallback);

        eprintln!(
            "wheezper: agent={endpoint} (max {}/min, {} output tokens{})",
            CALLS_PER_MINUTE,
            budget,
            if interview && kb_fallback {
                ", interview knowledgebase +general fallback"
            } else if interview {
                ", interview knowledgebase"
            } else {
                ""
            }
        );
        let (tx, rx) = mpsc::sync_channel::<Job>(QUEUE_SIZE);
        let (completed_tx, completed) = mpsc::sync_channel::<Annotation>(QUEUE_SIZE);
        let warned_failure = Arc::new(AtomicBool::new(false));
        let worker_warned = Arc::clone(&warned_failure);
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = Arc::clone(&pending);
        let model = spec.model.clone();
        let interview_mode = interview;
        std::thread::Builder::new()
            .name("annotation-agent".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let _guard = PendingGuard(Arc::clone(&worker_pending));
                    let prompt = if interview_mode {
                        format!(
                            "Interview transcript (oldest to newest):\n{}\n\nAnswer the latest line as the candidate.",
                            job.context
                        )
                    } else {
                        job.context.clone()
                    };
                    let response = match backend.generate(&system, &prompt, budget) {
                        Ok(response) => response,
                        Err(error) => {
                            warn_once(&worker_warned, format!("agent request failed: {error:#}"));
                            continue;
                        }
                    };
                    let response = response.split_whitespace().collect::<Vec<_>>().join(" ");
                    let response = if interview_mode { concise_answer(&response) } else { response };
                    let skip = response.trim().trim_end_matches(['.', '!']).trim();
                    if response.is_empty() || skip.eq_ignore_ascii_case("skip") {
                        continue;
                    }
                    let channel = if interview_mode {
                        "candidate"
                    } else {
                        job.channel.as_deref().unwrap_or("meeting")
                    };
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
            interview,
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
        let min_words = if self.interview { 1 } else { 3 };
        if line.split_whitespace().count() < min_words {
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
    use super::{AgentSpec, agent_system_prompt, concise_answer, load_knowledgebase};

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

    #[test]
    fn interview_answers_are_short_and_unquoted() {
        assert_eq!(
            concise_answer("\"I have five years of Spring Boot experience.\""),
            "I have five years of Spring Boot experience."
        );
        let long = (1..=40).map(|n| format!("w{n}")).collect::<Vec<_>>().join(" ");
        let short = concise_answer(&long);
        assert_eq!(short.split_whitespace().count(), super::INTERVIEW_MAX_WORDS);
        assert!(short.ends_with('…'));
        assert_eq!(
            concise_answer("ANS: I use Spring Boot and Kafka daily."),
            "I use Spring Boot and Kafka daily."
        );
        let clause = "one two three four five six seven eight nine ten, eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twentyone";
        assert_eq!(
            concise_answer(clause),
            "one two three four five six seven eight nine ten."
        );
    }

    #[test]
    fn interview_prompt_has_persona_and_word_limit() {
        let prompt = agent_system_prompt(true, Some("Position: Java Spring developer"), None, false);
        assert!(prompt.contains("reply exactly SKIP.\n\nKnowledgebase"));
        let prompt = agent_system_prompt(true, Some("Position: Java Spring developer"), None, true);
        assert!(!prompt.contains("cannot be answered from the knowledgebase"));
        assert!(prompt.contains("general and technical questions from your own knowledge"));
        let prompt = agent_system_prompt(true, Some("Position: Java Spring developer"), None, false);
        assert!(prompt.contains("candidate"));
        assert!(prompt.contains("10-20 words"));
        assert!(prompt.contains("Java Spring developer"));
        assert!(prompt.contains("SKIP"));
        assert!(prompt.contains("small talk"));
    }

    #[test]
    fn knowledgebase_is_loaded_and_bounded() {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("wheezper-kb-{id}.md"));
        std::fs::write(&path, "Role: Java developer\n").unwrap();
        assert_eq!(load_knowledgebase(&path).unwrap(), "Role: Java developer");
        let big = "x".repeat(super::KB_MAX_CHARS + 20);
        std::fs::write(&path, &big).unwrap();
        let loaded = load_knowledgebase(&path).unwrap();
        assert!(loaded.len() <= super::KB_MAX_CHARS + '…'.len_utf8());
        assert!(loaded.ends_with('…'));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    #[ignore = "requires OLLAMA_API_KEY and network access"]
    fn cloud_interview_answer_uses_knowledgebase_and_stays_short() {
        let kb = std::fs::read_to_string("knowledgebase.example.md").expect("sample knowledgebase");
        let model = std::env::var("WHEEZPER_CLOUD_MODEL")
            .unwrap_or_else(|_| "deepseek-v4.1-flash".to_string());
        let server = crate::llm::Ollama::new("https://ollama.com/api", &model);
        server.check().expect("cloud model available");
        let system = agent_system_prompt(
            true,
            Some(&kb),
            Some("Java Spring Boot developer interview"),
            false,
        );
        let user = "Interview transcript (oldest to newest):\nInterviewer: What is your experience with Spring Boot?\n\nAnswer the latest line as the candidate.";
        let answer = server
            .generate(&system, user, super::INTERVIEW_MAX_TOKENS)
            .expect("cloud interview answer");
        let answer = concise_answer(&answer);
        assert!(!answer.is_empty(), "cloud model returned no answer");
        assert!(
            answer.split_whitespace().count() <= super::INTERVIEW_MAX_WORDS,
            "answer too long: {answer}"
        );
        println!("ANS: {answer}");
    }
}
