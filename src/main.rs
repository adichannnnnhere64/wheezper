mod capture;
mod dsp;
mod models;
mod notepad;
mod segmenter;
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
///   wheezper -m models/ggml-base-q5_1.bin                 # default microphone
///   wheezper --source loopback -m model.bin               # system audio (calls)
///   ffmpeg -i call.wav -ar 16000 -ac 1 -f f32le - | wheezper --source stdin -m model.bin
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

    /// Audio source: mic, loopback (system audio), or stdin (f32le mono PCM).
    /// Default: stdin if stdin is piped, else mic.
    #[arg(long)]
    source: Option<SourceArg>,

    /// Pick device by case-insensitive substring (see --list-devices)
    #[arg(long)]
    device: Option<String>,

    /// Audio host override, e.g. pulseaudio, wasapi, alsa
    #[arg(long)]
    host: Option<String>,

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

    let pipeline = Pipeline::new(&cli, &notes_path, &model_path, threads)?;
    eprintln!("wheezper: lang={}", cli.language);

    match source {
        SourceArg::Stdin => run_stdin(&cli, pipeline),
        SourceArg::Mic | SourceArg::Loopback => {
            let mode = if source == SourceArg::Loopback { SourceMode::Loopback } else { SourceMode::Mic };
            run_capture(&cli, mode, pipeline)
        }
    }
}

// --- pipeline --------------------------------------------------------------

struct Pipeline {
    segmenter: Segmenter,
    framer: Framer,
    transcriber: Transcriber,
    notepad: Notepad,
    /// Tail of the last transcribed segment, fed as whisper's initial prompt
    /// so force-cut segments continue mid-sentence instead of hallucinating.
    last_tail: Option<String>,
}

impl Pipeline {
    fn new(cli: &Cli, notes_path: &std::path::Path, model: &std::path::Path, threads: i32) -> Result<Self> {
        let notepad = Notepad::open(notes_path)?;
        let transcriber = Transcriber::new(model, &cli.language, threads)?;
        let segmenter = Segmenter::new(
            TARGET_RATE,
            cli.threshold_db,
            cli.min_silence,
            cli.min_segment,
            cli.max_segment,
        );
        let framer = Framer::new(segmenter.frame_len());
        eprintln!("wheezper: notes -> {}", notepad.path().display());
        Ok(Self { segmenter, framer, transcriber, notepad, last_tail: None })
    }

    /// Push a chunk of 16 kHz mono samples.
    fn push(&mut self, samples: &[f32]) {
        self.framer.push(samples);
        while let Some(frame) = self.framer.next_frame() {
            if let Some(segment) = self.segmenter.feed(&frame) {
                self.handle(&segment);
            }
        }
    }

    /// Final flush (EOF or Ctrl-C): pad the partial frame, close the segment.
    fn finish(&mut self) {
        if let Some(frame) = self.framer.finish()
            && let Some(segment) = self.segmenter.feed(&frame)
        {
            self.handle(&segment);
        }
        if let Some(segment) = self.segmenter.flush() {
            self.handle(&segment);
        }
    }

    fn handle(&mut self, samples: &[f32]) {
        let prompt = self.last_tail.clone();
        match self.transcriber.transcribe(samples, prompt.as_deref()) {
            Ok(text) => match self.notepad.append(&text) {
                Ok(true) => {
                    self.last_tail = Some(last_words(&text, 8));
                    let ts = Local::now().format("%H:%M:%S");
                    println!("[{ts}] {text}");
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

// --- stdin source -----------------------------------------------------------

fn run_stdin(cli: &Cli, mut pipeline: Pipeline) -> Result<()> {
    eprintln!("wheezper: source=stdin ({} Hz, mono f32le)", cli.rate);
    let mut resampler = Resampler::new(cli.rate, TARGET_RATE);
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut byte_buf = vec![0u8; 16384];
    let mut samples = Vec::new();
    let mut out = Vec::new();

    loop {
        let n = reader.read(&mut byte_buf)?;
        if n == 0 {
            break;
        }
        samples.clear();
        for chunk in byte_buf.as_chunks::<4>().0.iter() {
            samples.push(f32::from_le_bytes(*chunk));
        }
        out.clear();
        resampler.push_slice(&samples, &mut out);
        pipeline.push(&out);
    }
    pipeline.finish();
    eprintln!("wheezper: done.");
    Ok(())
}

// --- live capture source ------------------------------------------------------

fn run_capture(cli: &Cli, mode: SourceMode, mut pipeline: Pipeline) -> Result<()> {
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

    let running = capture::ctrl_c_flag()?;
    let mut resampler = Resampler::new(cap.sample_rate, TARGET_RATE);
    let mut out = Vec::new();

    let result: Result<()> = loop {
        match cap.receiver.pop_timeout(Duration::from_millis(250)) {
            Pop::Data(chunk) => {
                out.clear();
                resampler.push_slice(&chunk, &mut out);
                pipeline.push(&out);
            }
            Pop::Timeout => {}
            Pop::Closed => break Ok(()),
        }
        if !running.load(std::sync::atomic::Ordering::SeqCst) {
            break Ok(());
        }
    };

    let dropped = cap.receiver.dropped_chunks();
    if dropped > 0 {
        eprintln!("wheezper: warning: {dropped} audio chunks dropped (transcription fell behind)");
    }
    result?;
    pipeline.finish();
    eprintln!("wheezper: done.");
    Ok(())
}
