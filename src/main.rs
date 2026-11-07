mod capture;
mod config;
mod dsp;
mod llm;
mod models;
mod notepad;
mod segmenter;
mod sink;
mod stream;
mod transcribe;
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
use std::path::PathBuf;
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

    /// Notepad output file [default: notes/YYYY-MM-DD.md]
    #[arg(short, long)]
    notes: Option<PathBuf>,

    /// Whisper language code, or "auto" to detect per segment
    #[arg(short, long, default_value = "auto")]
    language: String,

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

    /// LLM model for summary/polish. Server: anything it hosts.
    /// Built-in: gemma3-1b [default, Google's, ~800 MB GGUF] or a .gguf path
    /// Cloud: names from https://ollama.com/api/tags (e.g. gemma4:31b)
    #[arg(long, default_value = "gemma3:1b")]
    ollama_model: String,

    /// Clean up each utterance with the Ollama model before writing it
    /// (needs --ollama; adds latency per line)
    #[arg(long)]
    polish: bool,

    /// Config file (TOML). Without this flag, ./wheezper.toml then
    /// ~/.config/wheezper/config.toml (or %APPDATA%\wheezper) are used if present.
    #[arg(long)]
    config: Option<PathBuf>,

    /// List available audio devices and exit
    #[arg(long)]
    list_devices: bool,

    /// stdin input sample rate (Hz); stdin must be mono f32le
    #[arg(long, default_value_t = 16000)]
    rate: u32,

    /// RMS level (dBFS) below which audio counts as silence
    #[arg(long, default_value_t = -40.0)]
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

    /// CPU threads for whisper (0 = auto, capped at 4)
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
    summarize_session(&mut engine)?;
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
    webhook: Option<sink::Webhook>,
    partial: bool,
    /// Live-caption channel: Some when --partial. try_send only — if the
    /// worker is still inferring, this preview is dropped (whisper pads
    /// every call to 30 s, so each one costs ~3-5 s; previews must never
    /// block finals or audio).
    partial_tx: Option<std::sync::mpsc::SyncSender<PartialMsg>>,
    /// Local LLM post-processing: session summary on stop, optional polish.
    llm: Option<llm::Llm>,
    polish: bool,
}

impl Engine {
    fn new(cli: &Cli, notes_path: &std::path::Path, model: &std::path::Path, threads: i32) -> Result<Self> {
        let notepad = Notepad::open(notes_path)?;
        let transcriber = Transcriber::new(model, &cli.language, threads)?;
        eprintln!("wheezper: notes -> {}", notepad.path().display());
        let webhook = cli.webhook.as_deref().map(sink::Webhook::new);

        let llm: Option<llm::Llm> = if let Some(url) = cli.ollama.as_deref() {
            let server = llm::Ollama::new(url, &cli.ollama_model);
            match server.check() {
                Ok(()) => {
                    eprintln!("wheezper: ollama server ok — model {} @ {}", server.model, server.url);
                    Some(llm::Llm::Server(server))
                }
                Err(e) => {
                    // no server: hardwired llama.cpp, model auto-downloads
                    eprintln!("wheezper: no Ollama server ({e}); using built-in llama.cpp");
                    match llm::Builtin::new(&cli.ollama_model, threads) {
                        Ok(b) => Some(llm::Llm::Builtin(b)),
                        Err(e2) => {
                            eprintln!("wheezper: warning: built-in LLM unavailable: {e2:#}");
                            None
                        }
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

        Ok(Self { transcriber, vad, last: Default::default(), notepad, webhook, partial: cli.partial, partial_tx, llm, polish: cli.polish && cli.ollama.is_some() })
    }

    /// Queue a live-caption preview (never blocks; drops when busy).
    fn partial(&self, samples: &[f32], label: Option<&'static str>, tail: &Option<String>) {
        let Some(tx) = &self.partial_tx else { return };
        let msg = PartialMsg { samples: samples.to_vec(), label, prompt: tail.clone() };
        let _ = tx.try_send(msg);
    }

    /// Clear any partial-caption line before a final line is printed.
    fn clear_partial(&self) {
        if self.partial {
            print!("\r{}\r", " ".repeat(90));
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
    }

    /// Transcribe one segment and write it to every sink.
    /// `tail` carries per-channel prompt continuity between calls.
    fn handle(&mut self, samples: &[f32], label: Option<&str>, tail: &mut Option<String>) {
        // neural VAD gate: call comfort-noise / loopback dither that passed
        // the RMS gate is dropped here, before it can hallucinate a line
        if let Some(v) = &mut self.vad
            && !v.has_speech(samples, 0.1)
        {
            return;
        }
        let prompt = tail.clone();
        match self.transcriber.transcribe(samples, prompt.as_deref()) {
            Ok(raw) => {
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
                match self.notepad.append(label, &text) {
                    Ok(true) => {
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
            match cap.receiver.pop_timeout(step) {
                Pop::Data(chunk) => channel.push(&chunk, &mut engine),
                Pop::Timeout => {}
                Pop::Closed => break,
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
        match mic.receiver.pop_timeout(step) {
            Pop::Data(chunk) => me.push(&chunk, &mut engine),
            Pop::Timeout => {}
            Pop::Closed => break Ok(()),
        }
        for (cap, channel) in them_caps.iter().zip(others.iter_mut()) {
            match cap.receiver.pop_timeout(step) {
                Pop::Data(chunk) => channel.push(&chunk, &mut engine),
                Pop::Timeout => {}
                Pop::Closed => break,
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

// --- incremental streaming: words committed once, as soon as stable -------

/// Unified live loop: sliding-window inference, two-pass word commit,
/// append-only output. One shared worker thread does all inference.
fn run_stream(
    cli: &Cli,
    chans: Vec<(Capture, Option<&'static str>)>,
    mut engine: Engine,
) -> Result<Engine> {
    let threads = if cli.threads == 0 {
        std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4).min(4)
    } else {
        cli.threads
    };
    let model_path = models::resolve(Some(&cli.model))
        .with_context(|| format!("resolving model '{}'", cli.model))?;
    let (tx, rx) = stream::spawn_worker(&model_path, &cli.language, threads, !cli.no_vad)?;
    eprintln!("wheezper: stream: sliding-window incremental transcription (words committed once)");

    let mut caps: Vec<Capture> = Vec::with_capacity(chans.len());
    let mut streams: Vec<stream::StreamChannel> = Vec::with_capacity(chans.len());
    for (cap, label) in chans {
        streams.push(stream::StreamChannel::new(label, cap.sample_rate));
        caps.push(cap);
    }
    let running = capture::ctrl_c_flag()?;
    let step = Duration::from_millis(25);

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
        let mut all_closed = true;
        for (_i, (cap, sc)) in caps.iter().zip(streams.iter_mut()).enumerate() {
            match cap.receiver.pop_timeout(step) {
                Pop::Data(chunk) => sc.push(&chunk),
                Pop::Timeout => {}
                Pop::Closed => sc.mark_closed(),
            }
            all_closed &= sc.closed();
        }
        if all_closed {
            break;
        }

        // collect worker hypotheses
        while let Ok(res) = rx.try_recv() {
            if let Some(sc) = streams.get_mut(res.chan) {
                let was_open = sc.line_open();
                let delta = sc.on_result(&res.text);
                if !delta.is_empty() {
                    print_open(sc, !was_open);
                    print!("{delta} ");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
            }
        }

        // dispatch new windows; flush completed lines
        for (i, sc) in streams.iter_mut().enumerate() {
            let prompt = sc.prompt_tail();
            sc.maybe_send(i, &tx, &prompt);
            if sc.line_ready()
                && let Some(line) = sc.take_line()
            {
                commit_line(&mut engine, sc.label, &line);
            }
        }

        if !running.load(std::sync::atomic::Ordering::SeqCst) {
            break 'live;
        }
    }

    // shutdown: drain in-flight windows, commit remaining hypotheses
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(res) => {
                if let Some(sc) = streams.get_mut(res.chan) {
                    let was_open = sc.line_open();
                    let delta = sc.on_result(&res.text);
                    if !delta.is_empty() {
                        print_open(sc, !was_open);
                        print!("{delta} ");
                    }
                }
            }
            Err(_) => break,
        }
    }
    for sc in streams.iter_mut() {
        let was_open = sc.line_open();
        let delta = sc.finish();
        if !delta.is_empty() {
            print_open(sc, !was_open);
            print!("{delta} ");
        }
        if sc.line_open()
            && let Some(line) = sc.take_line()
        {
            commit_line(&mut engine, sc.label, &line);
        }
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
    if let Err(e) = engine.notepad.append(label, line) {
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
