mod capture;
mod dsp;
mod models;
mod notepad;
mod segmenter;
mod sink;
mod transcribe;

use anyhow::{Context, Result};
use capture::{Capture, Pop, SourceMode, TARGET_RATE};
use chrono::Local;
use clap::{Parser, ValueEnum};
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
    let cli = Cli::parse();

    // whisper.cpp is extremely chatty on stderr; silence it (we own the output)
    whisper_rs::install_logging_hooks();

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

    match source {
        SourceArg::Stdin => run_stdin(&cli, engine),
        SourceArg::Mic | SourceArg::Loopback => {
            let mode = if source == SourceArg::Loopback { SourceMode::Loopback } else { SourceMode::Mic };
            let label = (source == SourceArg::Loopback).then_some("them");
            run_capture(&cli, mode, label, engine)
        }
        SourceArg::Dual => run_dual(&cli, engine),
    }
}

// --- engine: transcription + sinks (shared across all channels) -----------

struct Engine {
    transcriber: Transcriber,
    notepad: Notepad,
    webhook: Option<sink::Webhook>,
}

impl Engine {
    fn new(cli: &Cli, notes_path: &std::path::Path, model: &std::path::Path, threads: i32) -> Result<Self> {
        let notepad = Notepad::open(notes_path)?;
        let transcriber = Transcriber::new(model, &cli.language, threads)?;
        eprintln!("wheezper: notes -> {}", notepad.path().display());
        let webhook = cli.webhook.as_deref().map(sink::Webhook::new);
        Ok(Self { transcriber, notepad, webhook })
    }

    /// Transcribe one segment and write it to every sink.
    /// `tail` carries per-channel prompt continuity between calls.
    fn handle(&mut self, samples: &[f32], label: Option<&str>, tail: &mut Option<String>) {
        let prompt = tail.clone();
        match self.transcriber.transcribe(samples, prompt.as_deref()) {
            Ok(text) => match self.notepad.append(label, &text) {
                Ok(true) => {
                    *tail = Some(last_words(&text, 8));
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
            },
            Err(e) => eprintln!("wheezper: transcription failed: {e:#}"),
        }
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
}

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
        Self { label, segmenter, framer, resampler, scratch: Vec::new(), tail: None }
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

// --- stdin source -----------------------------------------------------------

fn run_stdin(cli: &Cli, engine: Engine) -> Result<()> {
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
        for chunk in byte_buf.as_chunks::<4>().0.iter() {
            samples.push(f32::from_le_bytes(*chunk));
        }
        channel.push(&samples, &mut engine);
    }
    channel.finish(&mut engine);
    eprintln!("wheezper: done.");
    Ok(())
}

// --- live capture (single source) ---------------------------------------------

fn run_capture(cli: &Cli, mode: SourceMode, label: Option<&'static str>, engine: Engine) -> Result<()> {
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
    Ok(())
}

// --- dual source: mic ("me") + loopback ("them") -------------------------------

fn run_dual(cli: &Cli, engine: Engine) -> Result<()> {
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
    Ok(())
}

fn warn_dropped(rx: &capture::AudioReceiver) {
    let dropped = rx.dropped_chunks();
    if dropped > 0 {
        eprintln!("wheezper: warning: {dropped} audio chunks dropped (transcription fell behind)");
    }
}
