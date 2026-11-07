mod notepad;
mod segmenter;
mod transcribe;

use anyhow::Result;
use chrono::Local;
use clap::Parser;
use notepad::Notepad;
use segmenter::Segmenter;
use std::io::Read;
use std::path::PathBuf;
use transcribe::Transcriber;

/// wheezper — live audio transcription into a notepad.
///
/// Reads raw mono f32le PCM from stdin and appends transcribed utterances to
/// a markdown notepad. Feed it from anything:
///
///   ffmpeg -f pulse -i default -ar 16000 -ac 1 -f f32le - | wheezper -m model.bin
///   pw-cat --format f32 ...  | wheezper -m model.bin
///   cat recording.f32        | wheezper -m model.bin
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to the ggml whisper model (.bin)
    #[arg(short, long, env = "WHEEZPER_MODEL")]
    model: PathBuf,

    /// Notepad output file [default: notes/YYYY-MM-DD.md]
    #[arg(short, long)]
    notes: Option<PathBuf>,

    /// Whisper language code, or "auto" to detect per segment
    #[arg(short, long, default_value = "auto")]
    language: String,

    /// Input sample rate (Hz); input must be mono f32le
    #[arg(long, default_value_t = 16000)]
    rate: u32,

    /// Frames of silence below this RMS level (dBFS) count as silence
    #[arg(long, default_value_t = -40.0)]
    threshold_db: f32,

    /// Seconds of silence that close a segment
    #[arg(long, default_value_t = 0.4)]
    min_silence: f32,

    /// Minimum segment length in seconds
    #[arg(long, default_value_t = 0.5)]
    min_segment: f32,

    /// Maximum segment length in seconds
    #[arg(long, default_value_t = 12.0)]
    max_segment: f32,

    /// CPU threads for whisper (0 = all)
    #[arg(long, default_value_t = 0)]
    threads: i32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // whisper.cpp is extremely chatty on stderr; silence it (we own the output)
    whisper_rs::install_logging_hooks();

    let threads = if cli.threads == 0 {
        std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4)
    } else {
        cli.threads
    };

    let notes_path = cli.notes.clone().unwrap_or_else(|| {
        PathBuf::from("notes").join(format!("{}.md", Local::now().format("%Y-%m-%d")))
    });

    let mut notepad = Notepad::open(&notes_path)?;
    let mut transcriber = Transcriber::new(&cli.model, &cli.language, threads)?;
    let mut segmenter = Segmenter::new(cli.rate, cli.threshold_db, cli.min_silence, cli.min_segment, cli.max_segment);

    eprintln!(
        "wheezper: model={}, lang={}, rate={} Hz -> {}",
        cli.model.display(),
        cli.language,
        cli.rate,
        notepad.path().display()
    );

    let frame_bytes = segmenter.frame_len() * 4; // f32le
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut byte_buf = vec![0u8; frame_bytes];
    let mut sample_buf = vec![0f32; frame_bytes / 4];

    loop {
        match read_exact_or_eof(&mut reader, &mut byte_buf) {
            Ok(false) => break, // EOF
            Ok(true) => {
                for (i, chunk) in byte_buf.chunks_exact(4).enumerate() {
                    sample_buf[i] = f32::from_le_bytes(chunk.try_into().unwrap());
                }
                if let Some(segment) = segmenter.feed(&sample_buf) {
                    handle_segment(&mut transcriber, &mut notepad, &segment);
                }
            }
            Err(e) => return Err(e.context("reading stdin")),
        }
    }

    if let Some(segment) = segmenter.flush() {
        handle_segment(&mut transcriber, &mut notepad, &segment);
    }
    eprintln!("wheezper: done.");
    Ok(())
}

fn handle_segment(transcriber: &mut Transcriber, notepad: &mut Notepad, samples: &[f32]) {
    match transcriber.transcribe(samples) {
        Ok(text) => match notepad.append(&text) {
            Ok(true) => {
                let ts = Local::now().format("%H:%M:%S");
                println!("[{ts}] {text}");
            }
            Ok(false) => {} // dropped: empty or duplicate
            Err(e) => eprintln!("wheezper: notepad write failed: {e:#}"),
        },
        Err(e) => eprintln!("wheezper: transcription failed: {e:#}"),
    }
}

/// Read exactly `buf.len()` bytes; Ok(false) means clean EOF before any byte.
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = reader.read(&mut buf[filled..])?;
        if n == 0 {
            // EOF; drop any partial trailing frame (< 30 ms)
            return Ok(false);
        }
        filled += n;
    }
    Ok(true)
}
