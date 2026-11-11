mod agent;
mod capture;
mod config;
mod dsp;
mod llm;
mod models;
mod notepad;
#[cfg(feature = "sync")]
mod remote;
mod segmenter;
mod sink;
mod stream;
mod transcribe;
mod update;
mod vad;

use anyhow::{Context, Result};
use capture::{Capture, Pop, SourceMode, TARGET_RATE};
use chrono::Local;
use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, ValueEnum};
use dsp::{Framer, Resampler};
use notepad::Notepad;
use segmenter::Segmenter;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use transcribe::Transcriber;
use vad::Vad;

/// wheezper — live audio transcription into a notepad.
///
/// Captures your microphone (or system loopback audio) and appends transcribed
/// utterances to a markdown notepad. Local whisper.cpp inference: the audio
/// never leaves your machine.
///
/// Examples:
///   wheezper                                        # default microphone
///   wheezper --source loopback                      # system audio (calls)
///   wheezper --source dual                          # mic = "me", loopback = "them"
///   ffmpeg -i call.wav -ar 16000 -ac 1 -f f32le - | wheezper --source stdin
#[derive(Clone, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to a ggml whisper model (.bin), or a model name that is
    /// auto-downloaded on first use: tiny, base, small, medium, turbo
    #[arg(short, long, env = "WHEEZPER_MODEL", default_value = "base")]
    model: String,

    /// Model for the live preview tier (stdout only; notes stay on --model).
    /// `auto` = keep --model when it is tiny/base, otherwise use base; `same`
    /// = use --model for both tiers; anything else is resolved like --model.
    /// A cheap preview model keeps every word on screen fast even when the
    /// notes model is small/medium/turbo.
    #[arg(long, env = "WHEEZPER_PREVIEW_MODEL", default_value = "auto", value_name = "MODEL")]
    preview_model: String,

    /// Notepad output file [default: notes/YYYY-MM-DD.md]
    #[arg(short, long)]
    notes: Option<PathBuf>,

    /// Known-song mode: reference lyrics (one line per lyric line). The
    /// decoder is prompted with the lyrics around a moving cursor, so it
    /// follows the real words instead of guessing at mumbled/sung ones.
    /// Best with a single audio source (--source stdin, mic or loopback).
    #[arg(long, value_name = "FILE")]
    lyrics: Option<PathBuf>,

    /// With --lyrics: emit the reference lyric words for every segment the
    /// transcription aligns with — karaoke-style lyric sync. Unaligned
    /// segments keep the ASR text; already-emitted passages are dropped.
    #[arg(long, requires = "lyrics")]
    lyric_sync: bool,

    /// Whisper language code, or "auto" to detect per segment
    #[arg(short, long, default_value = "auto")]
    language: String,

    /// Domain context for whisper and the optional annotation agent (first 40 words).
    #[arg(long, value_name = "TEXT")]
    prompt: Option<String>,

    /// Audio source: mic, loopback (system audio), dual (mic + loopback,
    /// labeled "me"/"them"), or stdin (f32le mono PCM).
    /// Default: stdin if stdin is piped, else mic.
    #[arg(long)]
    source: Option<SourceArg>,

    /// Pick device by case-insensitive substring (see --list-devices)
    #[arg(long)]
    device: Option<String>,

    /// --source dual only: substring for the mic ("me") device
    #[arg(long)]
    device_me: Option<String>,

    /// --source dual only: substring for the loopback ("them") device
    #[arg(long)]
    device_them: Option<String>,

    /// Loopback/"them": capture EVERY sink monitor, not just the default
    /// sink's — audio playing on any output device gets recorded
    #[arg(long)]
    all_monitors: bool,

    /// Audio host override, e.g. pulseaudio, wasapi, alsa
    #[arg(long)]
    host: Option<String>,

    /// POST each transcribed utterance to this URL as JSON
    #[arg(long)]
    webhook: Option<String>,

    /// Show rolling live captions of the utterance in progress (stdout only,
    /// extra CPU — best paired with --model tiny on weaker machines)
    #[arg(long)]
    partial: bool,

    /// Disable the Silero voice-activity gate (not recommended: without it
    /// whisper hallucinates on call comfort-noise and quiet system audio)
    #[arg(long)]
    no_vad: bool,

    /// Disable incremental streaming for live capture and fall back to
    /// segment-then-transcribe (wait for silence, one final per segment)
    #[arg(long)]
    no_stream: bool,

    /// Post-process the transcript with a local LLM:
    /// uses an Ollama server if one is running at the URL (default
    /// http://localhost:11434) — otherwise falls back to llama.cpp compiled
    /// INTO wheezper (no Ollama install needed; the LLM model auto-downloads
    /// on first use). A session summary is appended to the notepad on stop.
    /// Ollama Cloud (premium models): --ollama https://ollama.com/api with
    /// OLLAMA_API_KEY set in the environment, e.g. --ollama-model gemma4:31b
    #[arg(long, num_args = 0..=1, default_missing_value = "http://localhost:11434")]
    ollama: Option<String>,

    /// Annotate committed transcript lines asynchronously: PROVIDER:MODEL[:BUDGET].
    /// Providers: ollama, ollama-cloud (OLLAMA_API_KEY), builtin.
    #[arg(long, value_name = "PROVIDER:MODEL[:BUDGET]")]
    with_agent: Option<String>,

    /// Interview knowledgebase (markdown/text). With --with-agent, the agent
    /// answers the interviewer as the candidate using these facts, in one
    /// concise 10-20 word sentence.
    #[arg(long, value_name = "FILE")]
    knowledgebase: Option<PathBuf>,

    /// With --knowledgebase: when the kb does not cover the question, answer
    /// general/technical questions from the model's own knowledge instead of
    /// replying SKIP. Personal experience still comes only from the kb.
    #[arg(long)]
    kb_fallback: bool,

    /// LLM model for summary/polish. Server: anything it hosts.
    /// Built-in: gemma3-1b [default, Google's, ~800 MB GGUF] or a .gguf path
    /// Cloud: names from https://ollama.com/api/tags (e.g. gemma4:31b)
    #[arg(long, default_value = "gemma3:1b")]
    ollama_model: String,

    /// Clean up each utterance with the Ollama model before writing it
    /// (needs --ollama; adds latency per line)
    #[arg(long)]
    polish: bool,

    /// Publish committed lines to a wheezper-server meeting session (UUID).
    /// Omit it and wheezper is exactly as local as before: no network is used.
    #[arg(long, value_name = "SESSION_ID", requires_all = ["server", "session_token"])]
    session: Option<String>,

    /// wheezper-server gRPC endpoint, e.g. https://meeting.example.com:50051
    #[arg(long, value_name = "URL", requires = "session")]
    server: Option<String>,

    /// Session-scoped credential (from the agent that created the session).
    /// Also read from WHEEZPER_SESSION_TOKEN.
    #[arg(long, value_name = "TOKEN", env = "WHEEZPER_SESSION_TOKEN", requires = "session", hide_env_values = true)]
    session_token: Option<String>,

    /// Config file (TOML). Without this flag, ./wheezper.toml then
    /// ~/.config/wheezper/config.toml (or %APPDATA%\wheezper) are used if present.
    #[arg(long)]
    config: Option<PathBuf>,

    /// List available audio devices and exit
    #[arg(long)]
    list_devices: bool,

    /// Update this binary to the latest published release and exit. Downloads
    /// the raw binary + SHA256SUMS from the GitHub release, verifies the hash
    /// and replaces the running executable (on Windows the swap happens as the
    /// process exits, because a running .exe is locked)
    #[arg(long)]
    update: bool,

    /// stdin input sample rate (Hz); stdin must be mono f32le
    #[arg(long, default_value_t = 16000)]
    rate: u32,

    /// RMS level (dBFS) below which audio counts as silence
    #[arg(long, default_value_t = -40.0, allow_hyphen_values = true)]
    threshold_db: f32,

    /// Seconds of silence that close a segment. Lower = final text appears
    /// sooner after you stop talking
    #[arg(long, default_value_t = 0.25)]
    min_silence: f32,

    /// Minimum segment length in seconds
    #[arg(long, default_value_t = 0.2)]
    min_segment: f32,

    /// Maximum segment length in seconds (whisper natively handles 30 s).
    /// Shorter = final text appears sooner during continuous speech (calls)
    #[arg(long, default_value_t = 10.0)]
    max_segment: f32,

    /// CPU threads for whisper (0 = auto: the live-preview tier gets the
    /// share that is left after the notes tier and the capture loop)
    #[arg(long, default_value_t = 0)]
    threads: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SourceArg {
    Stdin,
    Mic,
    Loopback,
    Dual,
}

fn main() -> Result<()> {
    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches)?;
    // whisper.cpp is extremely chatty on stderr; silence it (we own the output)
    whisper_rs::install_logging_hooks();

    if let Some(cfg) = config::load(cli.config.as_deref())? {
        apply_config(&mut cli, &matches, &cfg.table);
        eprintln!("wheezper: config <- {}", cfg.path.display());
    }

    if cli.update {
        return update::run();
    }

    if cli.list_devices {
        let host = capture::resolve_host(cli.host.as_deref())?;
        return capture::list_devices(&host);
    }

    // source default: piped stdin, else microphone
    let source = cli.source.unwrap_or({
        if std::io::stdin().is_terminal() {
            SourceArg::Mic
        } else {
            SourceArg::Stdin
        }
    });

    let notes_path = cli.notes.clone().unwrap_or_else(|| {
        PathBuf::from("notes").join(format!("{}.md", Local::now().format("%Y-%m-%d")))
    });

    let threads = if cli.threads == 0 {
        // whisper.cpp pads every call to a 30 s window, so more threads only
        // shorten bursts — and we run two inference pools (finals + captions).
        // Capping keeps CPU sane on big machines.
        std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4)
            .min(4)
    } else {
        cli.threads
    };

    let model_path = models::resolve(Some(&cli.model))
        .with_context(|| format!("resolving model '{}'", cli.model))?;
    eprintln!("wheezper: model={}", model_path.display());

    let engine = {
        // streaming mode prints live committed words itself; skip the
        // caption-worker thread (and its duplicate whisper model)
        let mut ecli = cli.clone();
        if !cli.no_stream
            && matches!(source, SourceArg::Mic | SourceArg::Loopback | SourceArg::Dual)
        {
            ecli.partial = false;
        }
        Engine::new(&ecli, &notes_path, &model_path, threads)?
    };
    eprintln!("wheezper: lang={}", cli.language);

    let mut engine = match source {
        SourceArg::Stdin => run_stdin(&cli, engine)?,
        SourceArg::Mic | SourceArg::Loopback => {
            let mode = if source == SourceArg::Loopback { SourceMode::Loopback } else { SourceMode::Mic };
            let label = (source == SourceArg::Loopback).then_some("them");
            run_capture(&cli, mode, label, engine)?
        }
        SourceArg::Dual => run_dual(&cli, engine)?,
    };
    // Let annotations for the last lines land before the summary is appended.
    // This runs after capture stopped, so it cannot delay the live path.
    engine.flush_agent_annotations(Duration::from_secs(2));
    summarize_session(&mut engine)?;
    engine.drain_agent_annotations();
    engine.finish_remote();
    Ok(())
}

// --- engine: transcription + sinks (shared across all channels) -----------

/// Message for the live-caption worker thread.
struct PartialMsg {
    samples: Vec<f32>,
    label: Option<&'static str>,
    prompt: Option<String>,
}

struct Engine {
    transcriber: Transcriber,
    /// Neural VAD gate: blocks noise/music (hallucination bait) from
    /// reaching whisper. None = disabled or failed to load (fail open).
    vad: Option<Vad>,
    /// Last written line per channel label: (text, consecutive repeats).
    /// 3+ identical lines in a row are suppressed (hallucination loops);
    /// a genuine repeat is kept.
    last: std::collections::HashMap<String, (String, u32)>,
    notepad: Notepad,
    agent: Option<agent::Agent>,
    domain_prompt: Option<String>,
    recent_lines: std::collections::VecDeque<String>,
    webhook: Option<sink::Webhook>,
    /// Optional remote meeting session (feature `sync`).
    #[cfg(feature = "sync")]
    remote: Option<remote::Remote>,
    partial: bool,
    /// Live-caption channel: Some when --partial. try_send only — if the
    /// worker is still inferring, this preview is dropped (whisper pads
    /// every call to 30 s, so each one costs ~3-5 s; previews must never
    /// block finals or audio).
    partial_tx: Option<std::sync::mpsc::SyncSender<PartialMsg>>,
    /// Local LLM post-processing: session summary on stop, optional polish.
    llm: Option<llm::Llm>,
    polish: bool,
    /// Reference-lyrics follower (--lyrics), shared by all channels.
    lyrics: Option<LyricFollower>,
    /// --lyric-sync: output the reference words where the ASR aligns.
    lyric_sync: bool,
}

impl Engine {
    fn new(cli: &Cli, notes_path: &std::path::Path, model: &std::path::Path, threads: i32) -> Result<Self> {
        let notepad = Notepad::open(notes_path)?;
        let transcriber = Transcriber::new(model, &cli.language, threads)?;
        eprintln!("wheezper: notes -> {}", notes_path.display());
        let domain_prompt = cli.prompt.as_deref().map(|p| first_words(p, 40)).filter(|p| !p.is_empty());
        let knowledgebase = if cli.with_agent.is_some() {
            cli.knowledgebase
                .as_deref()
                .map(agent::load_knowledgebase)
                .transpose()?
        } else {
            if cli.knowledgebase.is_some() {
                eprintln!("wheezper: warning: --knowledgebase has no effect without --with-agent");
            }
            None
        };
        let agent = cli
            .with_agent
            .as_deref()
            .map(|spec| {
                agent::Agent::new(
                    spec,
                    domain_prompt.as_deref(),
                    threads,
                    knowledgebase.as_deref(),
                    cli.kb_fallback,
                )
            })
            .transpose()?;
        let webhook = cli.webhook.as_deref().map(sink::Webhook::new);
        #[cfg(feature = "sync")]
        let remote = remote::Remote::from_cli(
            cli.session.as_deref(),
            cli.server.as_deref(),
            cli.session_token.as_deref(),
        )?;
        #[cfg(not(feature = "sync"))]
        if cli.session.is_some() {
            anyhow::bail!(
                "this build has no remote-session support; rebuild with `--features sync`"
            );
        }

        let llm: Option<llm::Llm> = if let Some(url) = cli.ollama.as_deref() {
            let server = llm::Ollama::new(url, &cli.ollama_model);
            match server.check() {
                Ok(()) => {
                    eprintln!("wheezper: ollama server ok — model {} @ {}", server.model, server.url);
                    Some(llm::Llm::Server(server))
                }
                Err(e) => {
                    // no server: hardwired llama.cpp, model auto-downloads
                    #[cfg(feature = "llm")]
                    {
                        eprintln!("wheezper: no Ollama server ({e}); using built-in llama.cpp");
                        match llm::Builtin::new(&cli.ollama_model, threads) {
                            Ok(b) => Some(llm::Llm::Builtin(b)),
                            Err(e2) => {
                                eprintln!("wheezper: warning: built-in LLM unavailable: {e2:#}");
                                None
                            }
                        }
                    }
                    #[cfg(not(feature = "llm"))]
                    {
                        eprintln!(
                            "wheezper: no Ollama server ({e}) and this build has no built-in \
                             LLM (compiled with --no-default-features); continuing without it"
                        );
                        None
                    }
                }
            }
        } else {
            None
        };

        let vad = if cli.no_vad {
            None
        } else {
            match Vad::new(2) {
                Ok(v) => {
                    eprintln!("wheezper: vad=silero (noise gate on)");
                    Some(v)
                }
                Err(e) => {
                    eprintln!("wheezper: warning: VAD unavailable, transcripts may hallucinate: {e:#}");
                    None
                }
            }
        };

        let lyrics = match cli.lyrics.as_deref() {
            Some(path) => {
                let follower = LyricFollower::load(path)?;
                eprintln!(
                    "wheezper: lyrics <- {} ({} words, mode: {})",
                    path.display(),
                    follower.len(),
                    if cli.lyric_sync { "sync" } else { "bias" }
                );
                Some(follower)
            }
            None => None,
        };

        let partial_tx = if cli.partial {
            let (tx, rx) = std::sync::mpsc::sync_channel::<PartialMsg>(1);
            let model = model.to_path_buf();
            let lang = cli.language.clone();
            let use_vad = !cli.no_vad;
            std::thread::Builder::new()
                .name("caption-worker".into())
                .spawn(move || {
                    // its own model instance: whisper.cpp states sharing one
                    // context are NOT safely concurrent (empirically corrupt
                    // each other); the extra weights cost ~30-60 MB.
                    // Half the threads: previews must never starve the main
                    // (final-transcript) inference pool.
                    let caption_threads = (threads / 2).max(1);
                    let mut worker = match Transcriber::new(&model, &lang, caption_threads) {
                        Ok(w) => w,
                        Err(e) => {
                            eprintln!("wheezper: caption worker failed to start: {e:#}");
                            return;
                        }
                    };
                    let mut last_text = String::new();
                    // its own VAD instance: previews of noise are dropped
                    // before spending a full 30 s-window inference
                    let mut vader = if use_vad { Vad::new(1).ok() } else { None };
                    while let Ok(msg) = rx.recv() {
                        if let Some(v) = &mut vader
                            && !v.has_speech(&msg.samples, 0.05)
                        {
                            continue;
                        }
                        let Ok(text) = worker.transcribe_greedy(&msg.samples, msg.prompt.as_deref())
                        else { continue };
                        let text = text.trim().to_string();
                        if text.is_empty() || text == last_text {
                            continue;
                        }
                        last_text = text.clone();
                        let ts = Local::now().format("%H:%M:%S");
                        let prefix = match msg.label {
                            Some(ch) => format!("{ch}: "),
                            None => String::new(),
                        };
                        print!("\r[{ts}] {prefix}{text}");
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                    }
                })
                .context("spawning caption worker")?;
            Some(tx)
        } else {
            None
        };

        Ok(Self {
            transcriber,
            vad,
            last: Default::default(),
            notepad,
            agent,
            domain_prompt,
            recent_lines: Default::default(),
            webhook,
            #[cfg(feature = "sync")]
            remote,
            partial: cli.partial,
            partial_tx,
            llm,
            polish: cli.polish && cli.ollama.is_some(),
            lyrics,
            lyric_sync: cli.lyric_sync,
        })
    }

    /// Queue a live-caption preview (never blocks; drops when busy).
    fn partial(&self, samples: &[f32], label: Option<&'static str>, tail: &Option<String>) {
        let Some(tx) = &self.partial_tx else { return };
        let prompt = self.whisper_prompt(tail.as_deref());
        let msg = PartialMsg { samples: samples.to_vec(), label, prompt };
        let _ = tx.try_send(msg);
    }

    /// Clear any partial-caption line before a final line is printed.
    fn clear_partial(&self) {
        if self.partial {
            print!("\r{}\r", " ".repeat(90));
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
    }

    /// Prompt from the reference lyrics (--lyrics). The bias plus
    /// --lyric-sync's "never emit a span twice" rule is the combination that
    /// measured best: the prompt keeps the decoder's wording on the lyrics,
    /// and re-sung/re-prompted passages cannot be emitted twice.
    fn lyric_prompt(&self) -> Option<String> {
        self.lyrics.as_ref().map(|l| l.prompt())
    }

    fn whisper_prompt(&self, context: Option<&str>) -> Option<String> {
        combine_prompt(self.domain_prompt.as_deref(), context)
    }

    /// Write transcript text immediately, then enqueue its optional annotation.
    fn append_line(&mut self, label: Option<&str>, text: &str) -> Result<bool> {
        let target = self.notepad.append_with_key(label, text)?;
        let Some(target) = target else { return Ok(false) };
        // Committed text only: the provisional preview never reaches the
        // server, exactly as it never reaches the notes.
        #[cfg(feature = "sync")]
        if let Some(remote) = &self.remote {
            remote.publish(label, text);
        }
        if let Some(agent) = &mut self.agent {
            let current = match label {
                Some(channel) => format!("{channel}: {text}"),
                None => text.to_string(),
            };
            let context = self
                .recent_lines
                .iter()
                .cloned()
                .chain(std::iter::once(current.clone()))
                .collect::<Vec<_>>()
                .join("\n");
            agent.submit(target, label, text, &context);
            self.recent_lines.push_back(current);
            while self.recent_lines.len() > 3 {
                self.recent_lines.pop_front();
            }
        }
        Ok(true)
    }

    /// Apply completed worker output on the orchestration thread, between
    /// inference calls, so annotation writes can never hold up Whisper.
    fn drain_agent_annotations(&mut self) {
        let completed = self
            .agent
            .as_ref()
            .map(agent::Agent::drain_completed)
            .unwrap_or_default();
        for annotation in completed {
            if let Err(error) = self
                .notepad
                .append_annotation(&annotation.target, &annotation.text)
            {
                eprintln!("wheezper: warning: writing agent annotation failed: {error:#}");
            }
        }
    }

    /// Give the remote session a short chance to drain; unsent events stay on
    /// disk and are retried on the next run.
    fn finish_remote(&mut self) {
        #[cfg(feature = "sync")]
        if let Some(remote) = self.remote.take() {
            remote.finish();
        }
    }

    /// Briefly drain queued annotations on stop. The worker may be mid-call,
    /// so the timeout bounds shutdown; anything still running is abandoned.
    fn flush_agent_annotations(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain_agent_annotations();
            let pending = self.agent.as_ref().map_or(0, agent::Agent::pending);
            if pending == 0 || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.drain_agent_annotations();
    }

    /// Move the lyric cursor past words just committed (live/stream mode).
    fn lyric_remember(&mut self, text: &str) {
        if let Some(l) = &mut self.lyrics {
            l.remember(text);
        }
    }

    fn lyric_advance(&mut self, text: &str) {
        if let Some(l) = &mut self.lyrics {
            let _ = l.advance(text);
        }
    }

    /// Transcribe one segment and write it to every sink.
    /// `tail` carries per-channel prompt continuity between calls.
    fn handle(&mut self, samples: &[f32], label: Option<&str>, tail: &mut Option<String>) {
        self.drain_agent_annotations();
        // neural VAD gate: call comfort-noise / loopback dither that passed
        // the RMS gate is dropped here, before it can hallucinate a line
        if let Some(v) = &mut self.vad
            && !v.has_speech(samples, 0.1)
        {
            return;
        }
        let context = self.lyrics.as_ref().map(|l| l.prompt());
        let prompt = self.whisper_prompt(context.as_deref().or(tail.as_deref()));
        let result = self.transcriber.transcribe(samples, prompt.as_deref());
        self.drain_agent_annotations();
        match result {
            Ok(raw) => {
                let raw = match &mut self.lyrics {
                    Some(l) => {
                        let (snapped, duplicate) = l.advance(&raw);
                        if duplicate && self.lyric_sync {
                            return; // already transcribed this passage
                        }
                        match snapped {
                            Some(lyric) if self.lyric_sync && !lyric.is_empty() => lyric,
                            _ => raw,
                        }
                    }
                    None => raw,
                };
                // optional LLM cleanup per utterance (keeps raw text on failure)
                let text = match (&self.llm, self.polish) {
                    (Some(o), true) => o.polish(&raw).unwrap_or(raw),
                    _ => raw,
                };
                // consecutive-duplicate suppression: whisper's classic
                // failure mode is looping the same hallucinated line. Real
                // speech does repeat ("I love you" twice!), so keep the first
                // two occurrences and only choke off from the third onward.
                let key = label.unwrap_or("");
                if !text.is_empty() {
                    let entry = self.last.entry(key.to_string()).or_default();
                    if entry.0 == text {
                        entry.1 += 1;
                        if entry.1 >= 2 {
                            return; // 3rd+ identical line in a row: hallucination
                        }
                    } else {
                        *entry = (text.clone(), 0);
                    }
                }
                match self.append_line(label, &text) {
                    Ok(true) => {
                        self.lyric_remember(&text);
                        *tail = Some(last_words(&text, 12));
                        self.clear_partial();
                        let ts = Local::now().format("%H:%M:%S");
                        match label {
                            Some(ch) => println!("[{ts}] {ch}: {text}"),
                            None => println!("[{ts}] {text}"),
                        }
                        if let Some(hook) = &self.webhook {
                            hook.post(label.unwrap_or(""), &text);
                        }
                    }
                    Ok(false) => {} // dropped: empty text
                    Err(e) => eprintln!("wheezper: notepad write failed: {e:#}"),
                }
            }
            Err(e) => eprintln!("wheezper: transcription failed: {e:#}"),
        }
    }
}

/// Apply config-file values to args the user did NOT pass on the command line.
fn apply_config(cli: &mut Cli, matches: &ArgMatches, cfg: &toml::Table) {
    let given = |id: &str| matches.value_source(id) == Some(ValueSource::CommandLine);
    let get = |key: &str| cfg.get(key);

    if !given("model")
        && let Some(v) = get("model").and_then(|v| v.as_str())
    {
        cli.model = v.to_string();
    }
    if !given("preview_model")
        && let Some(v) = get("preview-model").or_else(|| get("preview_model")).and_then(|v| v.as_str())
    {
        cli.preview_model = v.to_string();
    }
    if !given("notes")
        && let Some(v) = get("notes").and_then(|v| v.as_str())
    {
        cli.notes = Some(PathBuf::from(v));
    }
    if !given("language")
        && let Some(v) = get("language").and_then(|v| v.as_str())
    {
        cli.language = v.to_string();
    }
    if !given("prompt")
        && let Some(v) = get("prompt").and_then(|v| v.as_str())
    {
        cli.prompt = Some(v.to_string());
    }
    if !given("with_agent")
        && let Some(v) = get("with-agent").or_else(|| get("with_agent")).and_then(|v| v.as_str())
    {
        cli.with_agent = Some(v.to_string());
    }
    if !given("knowledgebase")
        && let Some(v) = get("knowledgebase").or_else(|| get("knowledge_base")).and_then(|v| v.as_str())
    {
        cli.knowledgebase = Some(PathBuf::from(v));
    }
    if !given("source")
        && let Some(v) = get("source").and_then(|v| v.as_str())
        && let Ok(mode) = SourceArg::from_str(v, true)
    {
        cli.source = Some(mode);
    }
    if !given("device")
        && let Some(v) = get("device").and_then(|v| v.as_str())
    {
        cli.device = Some(v.to_string());
    }
    if !given("device_me")
        && let Some(v) = get("device_me").and_then(|v| v.as_str())
    {
        cli.device_me = Some(v.to_string());
    }
    if !given("device_them")
        && let Some(v) = get("device_them").and_then(|v| v.as_str())
    {
        cli.device_them = Some(v.to_string());
    }
    if !given("host")
        && let Some(v) = get("host").and_then(|v| v.as_str())
    {
        cli.host = Some(v.to_string());
    }
    if !given("webhook")
        && let Some(v) = get("webhook").and_then(|v| v.as_str())
    {
        cli.webhook = Some(v.to_string());
    }
    if !given("session")
        && let Some(v) = get("session").and_then(|v| v.as_str())
    {
        cli.session = Some(v.to_string());
    }
    if !given("server")
        && let Some(v) = get("server").and_then(|v| v.as_str())
    {
        cli.server = Some(v.to_string());
    }
    if !given("session_token")
        && let Some(v) = get("session-token").or_else(|| get("session_token")).and_then(|v| v.as_str())
    {
        cli.session_token = Some(v.to_string());
    }
    if !given("rate")
        && let Some(v) = get("rate").and_then(|v| v.as_integer())
        && let Ok(v) = u32::try_from(v)
    {
        cli.rate = v;
    }
    if !given("threshold-db")
        && let Some(v) = get("threshold-db").or_else(|| get("threshold_db")).and_then(|v| v.as_float())
    {
        cli.threshold_db = v as f32;
    }
    if !given("min-silence")
        && let Some(v) = get("min-silence").or_else(|| get("min_silence")).and_then(|v| v.as_float())
    {
        cli.min_silence = v as f32;
    }
    if !given("min-segment")
        && let Some(v) = get("min-segment").or_else(|| get("min_segment")).and_then(|v| v.as_float())
    {
        cli.min_segment = v as f32;
    }
    if !given("max-segment")
        && let Some(v) = get("max-segment").or_else(|| get("max_segment")).and_then(|v| v.as_float())
    {
        cli.max_segment = v as f32;
    }
    if !given("threads")
        && let Some(v) = get("threads").and_then(|v| v.as_integer())
        && let Ok(v) = i32::try_from(v)
    {
        cli.threads = v;
    }
    if !given("partial")
        && let Some(v) = get("partial").and_then(|v| v.as_bool())
    {
        cli.partial = v;
    }
    if !given("all_monitors")
        && let Some(v) = get("all_monitors").and_then(|v| v.as_bool())
    {
        cli.all_monitors = v;
    }
    if !given("no_vad")
        && let Some(v) = get("no_vad").and_then(|v| v.as_bool())
    {
        cli.no_vad = v;
    }
}

/// Reference-lyrics follower for known-song mode (`--lyrics`).
///
/// Holds a cursor into the lyric word list. Each segment is decoded with the
/// words just *before* the cursor as the initial prompt — whisper treats that
/// as "previously said" text and continues from there, so it expects the
/// upcoming lyric line and picks its wording instead of guessing at slurred
/// syllables. After each segment the cursor moves to wherever the hypothesis
/// landed in the lyrics (fuzzy, so a repeated chorus finds its earlier spot).
struct LyricFollower {
    words: Vec<String>,
    pos: usize,
    /// Furthest word of the file already emitted by --lyric-sync: the cursor
    /// may revisit earlier lines (repeated choruses), the output may not.
    emitted: usize,
    /// Words of the last line output, to catch a segment that re-sings (or,
    /// under the prompt's bias, re-reads) what was just transcribed.
    last: Vec<String>,
}

/// Words of lyric context handed to the decoder (whisper's prompt budget is
/// 224 tokens; 30 words is plenty and keeps the bias local).
const LYRIC_PROMPT_WORDS: usize = 30;
/// Words of the hypothesis to look for when moving the cursor.
const LYRIC_ANCHOR_WORDS: usize = 6;

fn lyric_norm(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect::<String>()
        .to_lowercase()
}

impl LyricFollower {
    fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading lyrics {}", path.display()))?;
        let words: Vec<String> = text.split_whitespace().map(|w| w.to_string()).collect();
        if words.is_empty() {
            anyhow::bail!("lyrics file {} is empty", path.display());
        }
        Ok(Self { words, pos: 0, emitted: 0, last: Vec::new() })
    }

    fn len(&self) -> usize {
        self.words.len()
    }

    /// Context ending at the cursor: the decoder continues into the segment.
    fn prompt(&self) -> String {
        let end = self.pos.min(self.words.len());
        let start = end.saturating_sub(LYRIC_PROMPT_WORDS);
        self.words[start..end].join(" ")
    }

    /// Record what was actually printed, so the next segment can be checked
    /// for re-singing the same passage (a snapped line and an ASR line of the
    /// same audio are near-identical).
    fn remember(&mut self, text: &str) {
        self.last = text.split_whitespace().map(lyric_norm).filter(|w| !w.is_empty()).collect();
    }

    /// Move the cursor past the lyrics the hypothesis just covered.
    /// Returns `(Some(reference words), false)` when the match is solid and
    /// forward (emit the authoritative wording), `(None, true)` when the audio
    /// re-visited a span that was already emitted (drop it: the transcript is
    /// linear), and `(None, false)` when nothing matched (keep ASR text).
    fn advance(&mut self, text: &str) -> (Option<String>, bool) {
        let anchor: Vec<String> = {
            let mut v: Vec<String> = text
                .split_whitespace()
                .map(lyric_norm)
                .filter(|w| !w.is_empty())
                .collect();
            let skip = v.len().saturating_sub(LYRIC_ANCHOR_WORDS);
            v.drain(..skip);
            v
        };
        // a segment that mostly repeats the previous line is a re-rendition
        // or a prompt-induced re-read: emit it once (sync mode drops it)
        let hyp: Vec<String> = text.split_whitespace().map(lyric_norm).filter(|w| !w.is_empty()).collect();
        if hyp.len() >= 4 && !self.last.is_empty() {
            let shared = hyp.iter().filter(|w| self.last.contains(w)).count();
            if shared * 10 >= hyp.len() * 6 {
                return (None, true);
            }
        }
        if anchor.len() < 2 || self.pos >= self.words.len() {
            return (None, false);
        }
        let need = anchor.len().div_ceil(2); // at least half the anchor words
        let mut best: Option<(usize, usize, usize)> = None; // (hits, dist, end)
        let last = self.words.len().saturating_sub(anchor.len());
        for start in 0..=last {
            let hits = anchor
                .iter()
                .enumerate()
                .filter(|(k, w)| &lyric_norm(&self.words[start + k]) == *w)
                .count();
            if hits < need {
                continue;
            }
            // prefer matches at/after the cursor; a backwards jump has to be
            // a much better match to win (repeated choruses do go backwards)
            let dist = if start >= self.pos {
                start - self.pos
            } else {
                4 * (self.pos - start)
            };
            if best.is_none_or(|(h, d, _)| hits > h || (hits == h && dist < d)) {
                best = Some((hits, dist, start + anchor.len()));
            }
        }
        let Some((hits, _, end)) = best else {
            return (None, false);
        };
        let start = self.pos.min(self.words.len());
        self.pos = end.min(self.words.len());
        let solid = anchor.len() >= 3 && hits * 2 >= anchor.len() && end > start;
        if !solid {
            return (None, false);
        }
        if start < self.emitted {
            // the song re-visited a passage we already transcribed: the linear
            // transcript must not repeat it
            return (None, true);
        }
        self.emitted = self.pos;
        let lyric = self.words[start..self.pos].join(" ");
        self.last = lyric.split_whitespace().map(lyric_norm).filter(|w| !w.is_empty()).collect();
        (Some(lyric), false)
    }
}

fn first_words(s: &str, n: usize) -> String {
    s.split_whitespace().take(n).collect::<Vec<_>>().join(" ")
}

fn combine_prompt(domain: Option<&str>, context: Option<&str>) -> Option<String> {
    let domain = domain.map(|p| first_words(p, 40)).filter(|p| !p.is_empty());
    match (domain, context.filter(|p| !p.trim().is_empty())) {
        (Some(domain), Some(context)) => Some(format!("{domain}. {}", last_words(context, 12))),
        (Some(domain), None) => Some(domain),
        (None, Some(context)) => Some(context.to_string()),
        (None, None) => None,
    }
}

/// Last `n` words of `s`, for whisper's initial prompt.
fn last_words(s: &str, n: usize) -> String {
    s.split_whitespace()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" ")
}

// --- channel: one audio source through VAD to segments -----------------------

struct Channel {
    label: Option<&'static str>,
    segmenter: Segmenter,
    framer: Framer,
    resampler: Resampler,
    scratch: Vec<f32>,
    /// Tail of the last transcribed segment (per-channel prompt continuity)
    tail: Option<String>,
    partial: bool,
    last_partial: std::time::Instant,
}

// Live-caption cadence. whisper.cpp pads every inference to a 30 s window,
// so each preview costs a full-window pass. The 0.8 s floor is below the
// inference time itself: the sync_channel(1) + try_send pair drops previews
// while the worker is busy, so captions effectively self-pace back-to-back —
// a fresh line lands as soon as the previous inference finishes.
const PARTIAL_INTERVAL: Duration = Duration::from_millis(800);
const PARTIAL_MIN: usize = TARGET_RATE as usize / 2; // preview after 0.5 s of speech
const PARTIAL_WINDOW: usize = TARGET_RATE as usize * 4; // preview last 4 s

impl Channel {
    /// `in_rate`: sample rate of the incoming mono chunks.
    fn new(label: Option<&'static str>, in_rate: u32, cli: &Cli) -> Self {
        let segmenter = Segmenter::new(
            TARGET_RATE,
            cli.threshold_db,
            cli.min_silence,
            cli.min_segment,
            cli.max_segment,
        );
        let framer = Framer::new(segmenter.frame_len());
        let resampler = Resampler::new(in_rate, TARGET_RATE);
        Self { label, segmenter, framer, resampler, scratch: Vec::new(), tail: None, partial: cli.partial, last_partial: std::time::Instant::now() }
    }

    /// Push a chunk of mono samples at the channel's input rate.
    fn push(&mut self, chunk: &[f32], engine: &mut Engine) {
        engine.drain_agent_annotations();
        self.scratch.clear();
        self.resampler.push_slice(chunk, &mut self.scratch);
        self.framer.push(&self.scratch);
        self.scratch.clear();
        while let Some(frame) = self.framer.next_frame() {
            if let Some(segment) = self.segmenter.feed(&frame) {
                engine.handle(&segment, self.label, &mut self.tail);
            }
        }
        // rolling live caption of the utterance in progress
        if self.partial
            && self.last_partial.elapsed() >= PARTIAL_INTERVAL
            && self.segmenter.pending().len() >= PARTIAL_MIN
        {
            let pending = self.segmenter.pending();
            let start = pending.len().saturating_sub(PARTIAL_WINDOW);
            let window = pending[start..].to_vec();
            self.last_partial = std::time::Instant::now();
            engine.partial(&window, self.label, &self.tail);
        }
    }

    /// Final flush (EOF or Ctrl-C): pad the partial frame, close the segment.
    fn finish(&mut self, engine: &mut Engine) {
        if let Some(frame) = self.framer.finish()
            && let Some(segment) = self.segmenter.feed(&frame)
        {
            engine.handle(&segment, self.label, &mut self.tail);
        }
        if let Some(segment) = self.segmenter.flush() {
            engine.handle(&segment, self.label, &mut self.tail);
        }
    }
}

/// Append an LLM (Ollama) summary of the whole session to the notepad.
fn summarize_session(engine: &mut Engine) -> Result<()> {
    let Some(llm) = &engine.llm else { return Ok(()) };
    let transcript = engine.notepad.read_content().unwrap_or_default();
    if transcript.trim().is_empty() {
        return Ok(());
    }
    eprintln!("wheezper: summarizing session with {}…", llm.describe());
    let summary = match llm.summarize(&transcript) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("wheezper: summary failed (transcript kept as-is): {e:#}");
            return Ok(());
        }
    };
    if summary.is_empty() {
        return Ok(());
    }
    engine.notepad.append_block("Summary", &summary)?;
    println!("\n{summary}");
    eprintln!("wheezper: summary appended to notepad.");
    Ok(())
}

// --- stdin source -----------------------------------------------------------

fn run_stdin(cli: &Cli, engine: Engine) -> Result<Engine> {
    eprintln!("wheezper: source=stdin ({} Hz, mono f32le)", cli.rate);
    let (mut engine, mut channel) = (engine, Channel::new(None, cli.rate, cli));
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut byte_buf = vec![0u8; 16384];
    let mut samples = Vec::new();

    loop {
        let n = reader.read(&mut byte_buf)?;
        if n == 0 {
            break;
        }
        samples.clear();
        for chunk in byte_buf[..n].as_chunks::<4>().0.iter() {
            samples.push(f32::from_le_bytes(*chunk));
        }
        channel.push(&samples, &mut engine);
    }
    channel.finish(&mut engine);
    eprintln!("wheezper: done.");
    Ok(engine)
}

// --- live capture (single source) ---------------------------------------------

fn run_capture(cli: &Cli, mode: SourceMode, label: Option<&'static str>, engine: Engine) -> Result<Engine> {
    let host = capture::resolve_host(cli.host.as_deref())?;
    let caps: Vec<Capture> = if mode == SourceMode::Loopback && cli.all_monitors {
        capture::open_all_monitors(&host)?
    } else {
        vec![capture::open(mode, cli.device.as_deref(), &host)?]
    };
    for cap in &caps {
        eprintln!(
            "wheezper: source={} device=\"{}\" ({} Hz, {} ch){} — Ctrl-C to stop",
            match mode {
                SourceMode::Mic => "mic",
                SourceMode::Loopback => "loopback",
            },
            cap.device_name,
            cap.sample_rate,
            cap.channels,
            if caps.len() > 1 { " [+monitor]" } else { "" },
        );
    }
    if !cli.no_stream {
        let chans = caps.into_iter().map(|c| (c, label)).collect();
        return run_stream(cli, chans, engine);
    }

    let (mut engine, mut channels): (_, Vec<Channel>) =
        (engine, caps.iter().map(|c| Channel::new(label, c.sample_rate, cli)).collect());
    let running = capture::ctrl_c_flag()?;
    // 25 ms per empty channel: condvar pops return immediately when data is
    // queued, so this only bounds how long a silent channel can hold the
    // loop — every channel gets its chunks within ~25 ms of arrival.
    let step = Duration::from_millis(25);

    let result: Result<()> = loop {
        for (cap, channel) in caps.iter().zip(channels.iter_mut()) {
            // drain the queue, not one chunk per step (see run_stream)
            let mut pop = cap.receiver.pop_timeout(step);
            while let Pop::Data(chunk) = pop {
                channel.push(&chunk, &mut engine);
                pop = cap.receiver.try_pop();
            }
        }
        if !running.load(std::sync::atomic::Ordering::SeqCst) {
            break Ok(());
        }
    };

    for cap in &caps {
        warn_dropped(&cap.receiver);
    }
    result?;
    for channel in channels.iter_mut() {
        channel.finish(&mut engine);
    }
    eprintln!("wheezper: done.");
    Ok(engine)
}

// --- dual source: mic ("me") + loopback ("them") -------------------------------

fn run_dual(cli: &Cli, engine: Engine) -> Result<Engine> {
    let host = capture::resolve_host(cli.host.as_deref())?;
    // open loopback first: the far side of the call is the thing you least
    // want to miss while streams negotiate
    let them_caps: Vec<Capture> = if cli.all_monitors && cli.device_them.is_none() {
        capture::open_all_monitors(&host)?
    } else {
        vec![capture::open(SourceMode::Loopback, cli.device_them.as_deref(), &host)?]
    };
    let mic: Capture = capture::open(SourceMode::Mic, cli.device_me.as_deref(), &host)?;
    eprintln!(
        "wheezper: source=dual  me=\"{}\" ({}, {} ch) them=\"{}\"{} — Ctrl-C to stop",
        mic.device_name, mic.sample_rate, mic.channels,
        them_caps.iter().map(|c| c.device_name.as_str()).collect::<Vec<_>>().join("\", \""),
        if them_caps.len() > 1 { format!(" ({} monitors)", them_caps.len()) } else { String::new() },
    );
    if !cli.no_stream {
        let mut chans: Vec<(Capture, Option<&'static str>)> =
            them_caps.into_iter().map(|c| (c, Some("them"))).collect();
        chans.push((mic, Some("me")));
        return run_stream(cli, chans, engine);
    }

    let (mut engine, mut me) = (engine, Channel::new(Some("me"), mic.sample_rate, cli));
    let mut others: Vec<Channel> =
        them_caps.iter().map(|c| Channel::new(Some("them"), c.sample_rate, cli)).collect();
    let running = capture::ctrl_c_flag()?;
    let step = Duration::from_millis(25); // see run_capture

    let result: Result<()> = loop {
        let mut pop = mic.receiver.pop_timeout(step);
        while let Pop::Data(chunk) = pop {
            me.push(&chunk, &mut engine);
            pop = mic.receiver.try_pop();
        }
        for (cap, channel) in them_caps.iter().zip(others.iter_mut()) {
            let mut pop = cap.receiver.pop_timeout(step);
            while let Pop::Data(chunk) = pop {
                channel.push(&chunk, &mut engine);
                pop = cap.receiver.try_pop();
            }
        }
        if !running.load(std::sync::atomic::Ordering::SeqCst) {
            break Ok(());
        }
    };

    warn_dropped(&mic.receiver);
    for cap in &them_caps {
        warn_dropped(&cap.receiver);
    }
    result?;
    me.finish(&mut engine);
    for channel in others.iter_mut() {
        channel.finish(&mut engine);
    }
    eprintln!("wheezper: done.");
    Ok(engine)
}

// --- incremental streaming: two tiers, words committed once ----------------

/// Conservative preview width: works in an 80-column terminal without
/// querying the terminal size. The preview keeps the *tail* of the text.
const PREVIEW_COLS: usize = 72;

/// Bottom-line live preview of the provisional tier. Rewrites itself in
/// place with CR + clear-line — deliberately no cursor-up tricks, which
/// break the moment a row wraps. `hide` before any other output, `show`
/// after; the one cosmetic artifact is a blank row whenever a caption line
/// completes while the preview is up (it acts as a block separator).
struct LiveView {
    enabled: bool,
    shown: bool,
}

impl LiveView {
    fn new(enabled: bool) -> Self {
        Self { enabled, shown: false }
    }

    /// Erase the preview row so other output can print cleanly.
    fn hide(&mut self) {
        if self.enabled && self.shown {
            print!("\r\x1b[2K");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            self.shown = false;
        }
    }

    /// (Re)draw the preview on its own row at the bottom.
    fn show(&mut self, label: Option<&str>, text: &str) {
        if !self.enabled || text.is_empty() {
            return;
        }
        if self.shown {
            print!("\r\x1b[2K");
        } else {
            println!();
        }
        let mut line = String::new();
        if let Some(l) = label {
            line.push_str(l);
            line.push_str(": ");
        }
        line.push_str(text);
        if line.chars().count() > PREVIEW_COLS {
            // keep the tail: what is being said right now matters most
            let skip = line.chars().count() - PREVIEW_COLS;
            line = format!("…{}", line.chars().skip(skip).collect::<String>());
        }
        print!("▍{line}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        self.shown = true;
    }
}

/// Pick the model for the live preview tier.
///
/// Preview text is display-only, so a smaller model than the notes model is not
/// a quality regression: it puts complete words on screen sooner and frees CPU
/// for the notes tier, which is the one that falls behind when the notes model
/// is `small`/`medium`/`turbo` (measured on the reference box: one provisional
/// pass is 0.10 s on base vs 0.20-0.40 s on small).
///
/// `auto` keeps `--model` when it is already cheap (tiny/base, or an explicit
/// path the user chose), otherwise it uses base. `same` opts out.
fn resolve_preview_model(choice: &str, notes_arg: &str, notes_model: &Path) -> Result<PathBuf> {
    let choice = choice.trim();
    match choice.to_ascii_lowercase().as_str() {
        "" | "same" => Ok(notes_model.to_path_buf()),
        "auto" => {
            let name = notes_arg.trim().to_ascii_lowercase();
            if name == "tiny" || name == "base" || Path::new(notes_arg).exists() {
                Ok(notes_model.to_path_buf())
            } else {
                models::resolve(Some("base")).context("resolving the preview model 'base'")
            }
        }
        _ => models::resolve(Some(choice))
            .with_context(|| format!("resolving preview model '{choice}'")),
    }
}

/// Unified live loop, two tiers per channel. The final tier is the Phase-1
/// engine — generous windows, words committed exactly once by timestamp,
/// append-only, the only thing that reaches the notes. The provisional tier
/// decodes the live edge to put words on screen in ~0.5 s; its text is
/// display-only and superseded word-by-word as the final tier confirms.
fn run_stream(
    cli: &Cli,
    chans: Vec<(Capture, Option<&'static str>)>,
    mut engine: Engine,
) -> Result<Engine> {
    // Split the core budget between the tiers: the provisional tier is the
    // latency-critical one (the 0.5 s budget); the final tier's lag is
    // invisible behind the preview, so it runs on the leftovers, with one
    // core always reserved for the capture/main loop — oversubscribing the
    // box makes the ggml spin barriers convoy and doubles every pass time
    // (measured: 2+4 misses the p90 budget, 2+3 clears it with margin).
    // Piped output spawns no preview tier, so the final tier is alone and
    // keeps the Phase-1 thread share.
    let cores = std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4);
    let live = std::io::stdout().is_terminal();
    let (threads, prov_threads) = match cli.threads {
        n if n > 0 => (n, (n / 2).max(1)),
        _ if live => stream::split_threads(cores, 0),
        _ => (cores.min(4), 0),
    };
    let model_path = models::resolve(Some(&cli.model))
        .with_context(|| format!("resolving model '{}'", cli.model))?;
    let (tx, rx) = stream::spawn_worker(
        &model_path,
        &cli.language,
        threads,
        !cli.no_vad,
        transcribe::Profile::Final,
    )?;
    // the provisional tier only pays off when someone is watching stdout
    // live: piped output skips the extra worker (and its CPU) entirely and
    // behaves exactly like Phase 1
    let prov = if live {
        let preview_path = resolve_preview_model(&cli.preview_model, &cli.model, &model_path)?;
        if preview_path != model_path {
            eprintln!(
                "wheezper: stream: preview model={} (notes stay on {})",
                preview_path.display(),
                model_path.display()
            );
        }
        let (ptx, prx) = stream::spawn_worker(
            &preview_path,
            &cli.language,
            prov_threads,
            !cli.no_vad,
            transcribe::Profile::Provisional,
        )?;
        eprintln!("wheezper: stream: live preview tier on — words appear in ~0.5 s and firm up as the final pass confirms");
        Some((ptx, prx))
    } else {
        None
    };
    eprintln!("wheezper: stream: two-tier incremental transcription (append-only notes)");

    let mut caps: Vec<Capture> = Vec::with_capacity(chans.len());
    let mut feeds: Vec<stream::ChannelFeed> = Vec::with_capacity(chans.len());
    let mut streams: Vec<stream::StreamChannel> = Vec::with_capacity(chans.len());
    let mut provs: Vec<stream::ProvisionalTier> = Vec::with_capacity(chans.len());
    for (cap, label) in chans {
        feeds.push(stream::ChannelFeed::new(cap.sample_rate));
        streams.push(stream::StreamChannel::new(label, stream::Tuning::final_default()));
        if live {
            provs.push(stream::ProvisionalTier::new());
        }
        caps.push(cap);
    }
    let running = capture::ctrl_c_flag()?;
    let step = Duration::from_millis(25);
    let mut view = LiveView::new(live);
    let preview_prompt = engine.domain_prompt.clone().unwrap_or_default();

    let print_open = |sc: &stream::StreamChannel, first: bool| {
        if first {
            let ts = Local::now().format("%H:%M:%S");
            match sc.label {
                Some(ch) => print!("\n[{ts}] {ch}: "),
                None => print!("\n[{ts}] "),
            }
        }
    };

    'live: loop {
        engine.drain_agent_annotations();
        let mut all_closed = true;
        for (i, (cap, feed)) in caps.iter().zip(feeds.iter_mut()).enumerate() {
            // drain everything the device handed us, not one chunk per
            // iteration: a 16 kHz monitor delivers ~10 ms chunks, so a single
            // pop per 25 ms step falls behind real time and the pipeline
            // silently decodes a growing backlog
            let mut pop = cap.receiver.pop_timeout(step);
            loop {
                match pop {
                    Pop::Data(chunk) => {
                        let mono = feed.resample(&chunk);
                        streams[i].push_16k(mono);
                        if live {
                            provs[i].push_16k(mono);
                        }
                        pop = cap.receiver.try_pop();
                    }
                    Pop::Timeout => break,
                    Pop::Closed => {
                        streams[i].mark_closed();
                        if live {
                            provs[i].mark_closed();
                        }
                        break;
                    }
                }
            }
            all_closed &= streams[i].closed();
        }
        if all_closed {
            break;
        }

        // final tier: append-only stream + notes (words committed exactly
        // once, by timestamp — Phase-1 behaviour, just slower than the
        // preview nobody waits on)
        while let Ok(res) = rx.try_recv() {
            if let Some(sc) = streams.get_mut(res.chan) {
                let was_open = sc.line_open();
                let d = sc.on_result(res.start_s, res.dur_s, &res.segs);
                if live {
                    provs[res.chan].advance_final_boundary(sc.last_commit_s());
                }
                if !d.text.trim().is_empty() {
                    view.hide();
                    engine.lyric_advance(&d.text);
                    engine.lyric_remember(&d.text);
                    print_open(sc, !was_open);
                    print!("{} ", d.text.trim());
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
            }
        }

        // provisional tier: live preview line (display-only, revisable)
        if let Some((_, prx)) = &prov {
            let mut updated: Option<usize> = None;
            while let Ok(res) = prx.try_recv() {
                if let Some(p) = provs.get_mut(res.chan)
                    && !p.on_result(res.start_s, res.dur_s, &res.segs).is_empty()
                {
                    updated = Some(res.chan);
                }
            }
            if let Some(idx) = updated {
                let text = provs[idx].preview();
                let label = streams[idx].label;
                if text.is_empty() {
                    view.hide();
                } else {
                    view.show(label, &text);
                }
            }
        }

        // dispatch new windows; flush completed lines
        let lyric_prompt = engine.lyric_prompt();
        for (i, sc) in streams.iter_mut().enumerate() {
            let context = lyric_prompt.clone().unwrap_or_else(|| sc.prompt_tail());
            let prompt = engine.whisper_prompt(Some(&context));
            sc.maybe_send(i, &tx, prompt.as_deref().unwrap_or(""));
            if let Some((ptx, _)) = &prov {
                provs[i].maybe_send(i, ptx, &preview_prompt);
            }
            if sc.line_ready()
                && let Some(line) = sc.take_line()
            {
                view.hide();
                commit_line(&mut engine, sc.label, &line);
            }
        }

        if !running.load(std::sync::atomic::Ordering::SeqCst) {
            break 'live;
        }
    }

    // shutdown: give every channel one last pass per tier (the tail of the
    // utterance), then commit what is left. A tier that fell behind during
    // capture needs more than one window to reach the end of its buffer;
    // dropping the rest would silently lose the tail of the notes.
    for (i, sc) in streams.iter_mut().enumerate() {
        sc.flush();
        let context = engine.lyric_prompt().unwrap_or_else(|| sc.prompt_tail());
        let prompt = engine.whisper_prompt(Some(&context));
        sc.maybe_send(i, &tx, prompt.as_deref().unwrap_or(""));
    }
    for (i, p) in provs.iter_mut().enumerate() {
        p.flush();
        if let Some((ptx, _)) = &prov {
            p.maybe_send(i, ptx, &preview_prompt);
        }
    }
    // a final pass can take seconds (whisper pads to 30 s); wait it out
    // instead of abandoning the last line the moment the channel goes
    // momentarily quiet
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        // re-dispatch while any channel still holds sendable audio: each
        // arriving pass frees the next window. `maybe_send` returning false
        // on an idle channel means its buffer is drained, so the deadline
        // is only reached when the box cannot keep up at all.
        let mut pending = false;
        for (i, sc) in streams.iter_mut().enumerate() {
            pending |= if sc.busy() {
                true
            } else {
                let context = engine.lyric_prompt().unwrap_or_else(|| sc.prompt_tail());
                let prompt = engine.whisper_prompt(Some(&context));
                sc.maybe_send(i, &tx, prompt.as_deref().unwrap_or(""))
            };
        }
        if let Some((ptx, _)) = &prov {
            for (i, p) in provs.iter_mut().enumerate() {
                pending |= if p.busy() { true } else { p.maybe_send(i, ptx, &preview_prompt) };
            }
        }
        if !pending || Instant::now() >= deadline {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(res) => {
                if let Some(sc) = streams.get_mut(res.chan) {
                    let was_open = sc.line_open();
                    let d = sc.on_result(res.start_s, res.dur_s, &res.segs);
                    if live {
                        provs[res.chan].advance_final_boundary(sc.last_commit_s());
                    }
                    if !d.text.trim().is_empty() {
                        view.hide();
                        engine.lyric_advance(&d.text);
                        engine.lyric_remember(&d.text);
                        print_open(sc, !was_open);
                        print!("{} ", d.text.trim());
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if let Some((_, prx)) = &prov {
            match prx.recv_timeout(Duration::from_millis(500)) {
                Ok(res) => {
                    if let Some(p) = provs.get_mut(res.chan) {
                        p.on_result(res.start_s, res.dur_s, &res.segs);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
            }
        }
    }
    for sc in streams.iter_mut() {
        sc.finish();
        if sc.line_open()
            && let Some(line) = sc.take_line()
        {
            view.hide();
            commit_line(&mut engine, sc.label, &line);
        }
    }
    for p in provs.iter_mut() {
        p.finish();
    }
    for cap in &caps {
        warn_dropped(&cap.receiver);
    }
    eprintln!("\nwheezper: done.");
    Ok(engine)
}

/// Write one completed streaming line to the notepad + webhook and close
/// the terminal line.
fn commit_line(engine: &mut Engine, label: Option<&str>, line: &str) {
    println!();
    if let Err(e) = engine.append_line(label, line) {
        eprintln!("wheezper: notepad write failed: {e:#}");
    }
    if let Some(hook) = &engine.webhook {
        hook.post(label.unwrap_or(""), line);
    }
}

fn warn_dropped(rx: &capture::AudioReceiver) {
    let dropped = rx.dropped_chunks();
    if dropped > 0 {
        eprintln!("wheezper: warning: {dropped} audio chunks dropped (transcription fell behind)");
    }
}


#[cfg(test)]
mod phase3_tests {
    use super::{combine_prompt, first_words, resolve_preview_model};
    use std::path::Path;

    #[test]
    fn preview_model_auto_keeps_a_cheap_notes_model() {
        let notes = Path::new("/models/ggml-base-q5_1.bin");
        // tiny/base are cheap enough for the preview: no second model
        for name in ["base", "tiny", "Cargo.toml"] {
            assert_eq!(
                resolve_preview_model("auto", name, notes).unwrap(),
                notes,
                "auto should keep the notes model for {name}"
            );
        }
        // explicit opt-outs / choices never resolve the notes model name
        for choice in ["same", ""] {
            assert_eq!(resolve_preview_model(choice, "turbo", notes).unwrap(), notes);
        }
    }

    #[test]
    fn domain_prompt_is_bounded_and_keeps_local_context() {
        let domain = (0..45).map(|n| format!("d{n}")).collect::<Vec<_>>().join(" ");
        let local = "one two three four five six seven eight nine ten eleven twelve thirteen";
        let combined = combine_prompt(Some(&domain), Some(local)).unwrap();
        let words: Vec<_> = combined.split_whitespace().collect();
        assert_eq!(words.len(), 52); // 40 domain words + 12 local words
        assert_eq!(words[0], "d0");
        assert!(combined.ends_with("two three four five six seven eight nine ten eleven twelve thirteen"));
        assert_eq!(first_words(&domain, 40).split_whitespace().count(), 40);
    }

    #[test]
    fn local_prompt_is_unchanged_without_domain_context() {
        assert_eq!(combine_prompt(None, Some("the previous twelve words")), Some("the previous twelve words".to_string()));
    }
}
