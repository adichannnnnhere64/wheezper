mod capture;
mod config;
mod dsp;
mod llm;
mod models;
mod notepad;
mod segmenter;
mod sink;
mod transcribe;

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
use std::time::Duration;
use transcribe::Transcriber;

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
#[derive(Parser)]
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

    /// Use a local Ollama server (https://ollama.com) to post-process the
    /// transcript: a session summary is appended to the notepad on stop.
    /// Optionally pass a server URL [default: http://localhost:11434]
    #[arg(long, num_args = 0..=1, default_missing_value = "http://localhost:11434")]
    ollama: Option<String>,

    /// Ollama model for summary/polish — anything the server hosts
    /// [default: gemma3:1b, Google's smallest]
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

    /// Seconds of silence that close a segment
    #[arg(long, default_value_t = 0.6)]
    min_silence: f32,

    /// Minimum segment length in seconds
    #[arg(long, default_value_t = 0.5)]
    min_segment: f32,

    /// Maximum segment length in seconds (whisper natively handles 30 s)
    #[arg(long, default_value_t = 25.0)]
    max_segment: f32,

    /// CPU threads for whisper (0 = all)
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
        std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4)
    } else {
        cli.threads
    };

    let model_path = models::resolve(Some(&cli.model))
        .with_context(|| format!("resolving model '{}'", cli.model))?;
    eprintln!("wheezper: model={}", model_path.display());

    let engine = Engine::new(&cli, &notes_path, &model_path, threads)?;
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
    notepad: Notepad,
    webhook: Option<sink::Webhook>,
    partial: bool,
    /// Live-caption channel: Some when --partial. try_send only — if the
    /// worker is still inferring, this preview is dropped (whisper pads
    /// every call to 30 s, so each one costs ~3-5 s; previews must never
    /// block finals or audio).
    partial_tx: Option<std::sync::mpsc::SyncSender<PartialMsg>>,
    /// Local LLM post-processing (Ollama): summary on stop, optional polish.
    ollama: Option<llm::Ollama>,
    polish: bool,
}

impl Engine {
    fn new(cli: &Cli, notes_path: &std::path::Path, model: &std::path::Path, threads: i32) -> Result<Self> {
        let notepad = Notepad::open(notes_path)?;
        let transcriber = Transcriber::new(model, &cli.language, threads)?;
        eprintln!("wheezper: notes -> {}", notepad.path().display());
        let webhook = cli.webhook.as_deref().map(sink::Webhook::new);

        let ollama = cli.ollama.as_deref().map(|url| {
            let o = llm::Ollama::new(url, &cli.ollama_model);
            if let Err(e) = o.check() {
                eprintln!("wheezper: warning: {e:#}");
            } else {
                eprintln!("wheezper: ollama ok — model {} @ {}", o.model, o.url);
            }
            o
        });

        let partial_tx = if cli.partial {
            let (tx, rx) = std::sync::mpsc::sync_channel::<PartialMsg>(1);
            let model = model.to_path_buf();
            let lang = cli.language.clone();
            std::thread::Builder::new()
                .name("caption-worker".into())
                .spawn(move || {
                    // its own model instance: whisper.cpp states sharing one
                    // context are NOT safely concurrent (empirically corrupt
                    // each other); the extra weights cost ~30-60 MB
                    let mut worker = match Transcriber::new(&model, &lang, threads) {
                        Ok(w) => w,
                        Err(e) => {
                            eprintln!("wheezper: caption worker failed to start: {e:#}");
                            return;
                        }
                    };
                    let mut last_text = String::new();
                    while let Ok(msg) = rx.recv() {
                        let Ok(text) = worker.transcribe(&msg.samples, msg.prompt.as_deref())
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

        Ok(Self { transcriber, notepad, webhook, partial: cli.partial, partial_tx, ollama, polish: cli.polish && cli.ollama.is_some() })
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
        let prompt = tail.clone();
        match self.transcriber.transcribe(samples, prompt.as_deref()) {
            Ok(raw) => {
                // optional LLM cleanup per utterance (keeps raw text on failure)
                let text = match (&self.ollama, self.polish) {
                    (Some(o), true) => o.polish(&raw).unwrap_or(raw),
                    _ => raw,
                };
                match self.notepad.append(label, &text) {
                    Ok(true) => {
                        *tail = Some(last_words(&text, 8));
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

const PARTIAL_INTERVAL: Duration = Duration::from_millis(3000);
const PARTIAL_MIN: usize = TARGET_RATE as usize * 2; // need 2 s before previewing
const PARTIAL_WINDOW: usize = TARGET_RATE as usize * 6; // preview last 6 s

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
    let Some(ollama) = &engine.ollama else { return Ok(()) };
    let transcript = engine.notepad.read_content().unwrap_or_default();
    if transcript.trim().is_empty() {
        return Ok(());
    }
    eprintln!("wheezper: summarizing session with Ollama ({})…", ollama.model);
    let summary = match ollama.summarize(&transcript) {
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
    let cap: Capture = capture::open(mode, cli.device.as_deref(), &host)?;
    eprintln!(
        "wheezper: source={} device=\"{}\" ({} Hz, {} ch) — Ctrl-C to stop",
        match mode {
            SourceMode::Mic => "mic",
            SourceMode::Loopback => "loopback",
        },
        cap.device_name,
        cap.sample_rate,
        cap.channels
    );

    let (mut engine, mut channel) = (engine, Channel::new(label, cap.sample_rate, cli));
    let running = capture::ctrl_c_flag()?;

    let result: Result<()> = loop {
        match cap.receiver.pop_timeout(Duration::from_millis(250)) {
            Pop::Data(chunk) => channel.push(&chunk, &mut engine),
            Pop::Timeout => {}
            Pop::Closed => break Ok(()),
        }
        if !running.load(std::sync::atomic::Ordering::SeqCst) {
            break Ok(());
        }
    };

    warn_dropped(&cap.receiver);
    result?;
    channel.finish(&mut engine);
    eprintln!("wheezper: done.");
    Ok(engine)
}

// --- dual source: mic ("me") + loopback ("them") -------------------------------

fn run_dual(cli: &Cli, engine: Engine) -> Result<Engine> {
    let host = capture::resolve_host(cli.host.as_deref())?;
    // open loopback first: the far side of the call is the thing you least
    // want to miss while streams negotiate
    let them: Capture = capture::open(SourceMode::Loopback, None, &host)?;
    let mic: Capture = capture::open(SourceMode::Mic, None, &host)?;
    eprintln!(
        "wheezper: source=dual  me=\"{}\" ({}, {} ch) them=\"{}\" ({}, {} ch) — Ctrl-C to stop",
        mic.device_name, mic.sample_rate, mic.channels,
        them.device_name, them.sample_rate, them.channels
    );

    let (mut engine, mut me) = (engine, Channel::new(Some("me"), mic.sample_rate, cli));
    let mut others = Channel::new(Some("them"), them.sample_rate, cli);
    let running = capture::ctrl_c_flag()?;
    let step = Duration::from_millis(125);

    let result: Result<()> = loop {
        match mic.receiver.pop_timeout(step) {
            Pop::Data(chunk) => me.push(&chunk, &mut engine),
            Pop::Timeout => {}
            Pop::Closed => break Ok(()),
        }
        match them.receiver.pop_timeout(step) {
            Pop::Data(chunk) => others.push(&chunk, &mut engine),
            Pop::Timeout => {}
            Pop::Closed => break Ok(()),
        }
        if !running.load(std::sync::atomic::Ordering::SeqCst) {
            break Ok(());
        }
    };

    warn_dropped(&mic.receiver);
    warn_dropped(&them.receiver);
    result?;
    me.finish(&mut engine);
    others.finish(&mut engine);
    eprintln!("wheezper: done.");
    Ok(engine)
}

fn warn_dropped(rx: &capture::AudioReceiver) {
    let dropped = rx.dropped_chunks();
    if dropped > 0 {
        eprintln!("wheezper: warning: {dropped} audio chunks dropped (transcription fell behind)");
    }
}
