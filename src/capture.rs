//! Native audio capture via CPAL: microphone and system-loopback sources.
//!
//! Host policy: on Linux we prefer the PulseAudio host (works against
//! PipeWire's pulse layer and exposes monitor sources for loopback);
//! on Windows the default WASAPI host does loopback by opening an output
//! device as an input stream.

use crate::dsp;
use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, Stream, StreamConfig, SupportedStreamConfigRange};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub const TARGET_RATE: u32 = 16000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMode {
    Mic,
    Loopback,
}

/// Select the audio host: explicit `--host`, else PulseAudio on Linux when
/// available (PipeWire-compatible, exposes monitors), else CPAL's default.
pub fn resolve_host(prefer: Option<&str>) -> Result<Host> {
    let hosts = cpal::available_hosts();
    if let Some(name) = prefer {
        let wanted = name.to_ascii_lowercase();
        let id = hosts
            .into_iter()
            .find(|h| format!("{h:?}").to_ascii_lowercase().contains(&wanted))
            .ok_or_else(|| anyhow!("audio host '{name}' is not available"))?;
        return cpal::host_from_id(id).context("initialising audio host");
    }
    #[cfg(target_os = "linux")]
    if let Some(id) = hosts.iter().find(|h| format!("{h:?}").contains("PulseAudio"))
        && let Ok(host) = cpal::host_from_id(*id)
    {
        return Ok(host);
    }
    Ok(cpal::default_host())
}

fn dev_name(device: &Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "<unnamed>".into())
}

/// Raw host device identifier (e.g. pulse source name `alsa_output...monitor`).
fn dev_id(device: &Device) -> String {
    device.id().map(|i| i.id().to_string()).unwrap_or_default()
}

fn host_id_name(host: &Host) -> String {
    format!("{:?}", host.id())
}

pub fn list_devices(host: &Host) -> Result<()> {
    println!("host: {}", host_id_name(host));
    let default_in = host.default_input_device().map(|d| dev_name(&d));
    let default_out = host.default_output_device().map(|d| dev_name(&d));
    for device in host.devices().context("listing devices")? {
        let name = dev_name(&device);
        let mut tags = Vec::new();
        if let Ok(cfg) = device.default_input_config() {
            tags.push(format!("in: {} Hz, {} ch", cfg.sample_rate(), cfg.channels()));
        }
        if let Ok(cfg) = device.default_output_config() {
            tags.push(format!("out: {} Hz, {} ch", cfg.sample_rate(), cfg.channels()));
        }
        if Some(&name) == default_in.as_ref() {
            tags.push("DEFAULT INPUT".into());
        }
        if Some(&name) == default_out.as_ref() {
            tags.push("DEFAULT OUTPUT".into());
        }
        if dev_id(&device).to_ascii_lowercase().ends_with(".monitor") {
            tags.push("LOOPBACK".into());
        }
        println!("  {name}  [{}]", tags.join(", "));
    }
    Ok(())
}

/// A running capture stream plus everything needed to consume it.
/// `stream` must stay alive for capture to continue (the callback owns the
/// queue sender and drops it when the stream is dropped).
pub struct Capture {
    /// Keep-alive: dropping this stops capture.
    #[allow(dead_code)]
    pub stream: Stream,
    pub receiver: AudioReceiver,
    pub sample_rate: u32,
    pub channels: u16,
    pub device_name: String,
}

/// Build and start an input stream on `device`, delivering mono chunks.
pub fn open(mode: SourceMode, device_substr: Option<&str>, host: &Host) -> Result<Capture> {
    let device = pick_device(host, mode, device_substr)?;
    let device_name = dev_name(&device);

    let mut ranges = device
        .supported_input_configs()
        .with_context(|| format!("device '{device_name}' cannot capture audio input"))?;
    let range = ranges
        .find(|r| r.sample_format() == SampleFormat::F32)
        .ok_or_else(|| anyhow!("device '{device_name}' has no f32 input configuration"))?;

    let rate = pick_rate(&range);
    let channels = range.channels();
    let config: StreamConfig = range.with_sample_rate(rate).into();

    // ~3 minutes of buffered audio; overflow drops the oldest chunks.
    let (sender, receiver) = audio_queue(rate as usize * 180);
    let ch = channels as usize;
    let data_cb = move |data: &[f32], _info: &cpal::InputCallbackInfo| {
        let mono = dsp::mix_to_mono(data, ch);
        sender.push(mono);
    };
    let err_cb = |err| eprintln!("wheezper: audio stream error: {err}");
    let stream = device
        .build_input_stream::<f32, _, _>(config, data_cb, err_cb, None)
        .context("building input stream")?;
    stream.play().context("starting input stream")?;

    Ok(Capture { stream, receiver, sample_rate: rate, channels, device_name })
}

fn pick_device(host: &Host, mode: SourceMode, substr: Option<&str>) -> Result<Device> {
    match mode {
        SourceMode::Mic => match substr {
            Some(s) => find_device(host, s),
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow!("no default input device found")),
        },
        SourceMode::Loopback => loopback_device(host, substr),
    }
}

/// Case-insensitive substring match, preferring devices that can capture.
fn find_device(host: &Host, s: &str) -> Result<Device> {
    let want = s.to_ascii_lowercase();
    let matched: Vec<Device> = host
        .devices()
        .context("listing devices")?
        .filter(|d| {
            dev_name(d).to_ascii_lowercase().contains(&want)
                || dev_id(d).to_ascii_lowercase().contains(&want)
        })
        .collect();
    if matched.is_empty() {
        bail!("no device matching '{s}'");
    }
    matched
        .into_iter()
        .find(|d| d.supported_input_configs().is_ok())
        .ok_or_else(|| anyhow!("device matching '{s}' cannot capture audio input"))
}

fn loopback_device(host: &Host, substr: Option<&str>) -> Result<Device> {
    let host_name = host_id_name(host).to_ascii_lowercase();
    if host_name.contains("pulseaudio") {
        if let Some(s) = substr {
            return find_device(host, s);
        }
        let sink = host
            .default_output_device()
            .ok_or_else(|| anyhow!("no default output device; cannot find its monitor"))?;
        let sink_raw = dev_id(&sink);
        let monitor = format!("{sink_raw}.monitor").to_ascii_lowercase();
        let mut any_monitor = None;
        for d in host.devices().context("listing devices")? {
            let raw = dev_id(&d).to_ascii_lowercase();
            if d.supported_input_configs().is_err() {
                continue;
            }
            if raw == monitor {
                return Ok(d); // monitor of the default sink
            }
            if raw.ends_with(".monitor") && any_monitor.is_none() {
                any_monitor = Some(d);
            }
        }
        return any_monitor.ok_or_else(|| {
            anyhow!("no loopback monitor source found (PulseAudio/PipeWire server has no sinks?)")
        });
    }
    if host_name.contains("wasapi") {
        // WASAPI: opening an output device as input = transparent loopback.
        return match substr {
            Some(s) => find_device(host, s),
            None => host
                .default_output_device()
                .ok_or_else(|| anyhow!("no default output device for loopback")),
        };
    }
    bail!(
        "loopback is not supported on host '{host_name}'. \
         Use a pipe instead: ffmpeg -f pulse -i <monitor> -ar 16000 -ac 1 -f f32le - | wheezper --source stdin"
    )
}

/// Prefer 16 kHz directly; else the nearest rate the device supports.
fn pick_rate(range: &SupportedStreamConfigRange) -> u32 {
    const TARGET: u32 = TARGET_RATE;
    if range.min_sample_rate() <= TARGET && TARGET <= range.max_sample_rate() {
        return TARGET;
    }
    if range.min_sample_rate() > TARGET {
        range.min_sample_rate()
    } else {
        range.max_sample_rate()
    }
}

// ---------------------------------------------------------------------------
// AudioQueue: audio thread -> pipeline thread, bounded, drop-oldest.
// ---------------------------------------------------------------------------

struct QueueState {
    chunks: VecDeque<Vec<f32>>,
    samples: usize,
    closed: bool,
    dropped_chunks: u64,
}

pub fn audio_queue(cap_samples: usize) -> (AudioSender, AudioReceiver) {
    let state = Arc::new((
        Mutex::new(QueueState { chunks: VecDeque::new(), samples: 0, closed: false, dropped_chunks: 0 }),
        Condvar::new(),
    ));
    (
        AudioSender { state: state.clone(), cap: cap_samples },
        AudioReceiver { state },
    )
}

pub struct AudioSender {
    state: Arc<(Mutex<QueueState>, Condvar)>,
    cap: usize,
}

pub struct AudioReceiver {
    state: Arc<(Mutex<QueueState>, Condvar)>,
}

pub enum Pop {
    Data(Vec<f32>),
    Timeout,
    Closed,
}

impl AudioSender {
    /// Push a mono chunk. If the queue is full, the oldest chunks are dropped
    /// (transcription fell behind; better to lose old audio than block the
    /// audio thread or grow memory without bound).
    pub fn push(&self, chunk: Vec<f32>) {
        if chunk.is_empty() {
            return;
        }
        let (lock, cv) = &*self.state;
        let mut st = lock.lock().unwrap();
        while st.samples + chunk.len() > self.cap {
            match st.chunks.pop_front() {
                Some(old) => {
                    st.samples -= old.len();
                    st.dropped_chunks += 1;
                }
                None => break,
            }
        }
        st.samples += chunk.len();
        st.chunks.push_back(chunk);
        cv.notify_one();
    }
}

impl AudioReceiver {
    /// Wait up to `timeout` for a chunk.
    pub fn pop_timeout(&self, timeout: Duration) -> Pop {
        let (lock, cv) = &*self.state;
        let mut st = lock.lock().unwrap();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(chunk) = st.chunks.pop_front() {
                st.samples -= chunk.len();
                return Pop::Data(chunk);
            }
            if st.closed {
                return Pop::Closed;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Pop::Timeout;
            }
            let (guard, _res) = cv.wait_timeout(st, deadline - now).unwrap();
            st = guard;
        }
    }

    pub fn dropped_chunks(&self) -> u64 {
        self.state.0.lock().unwrap().dropped_chunks
    }
}

// shared Ctrl-C flag helper
pub fn ctrl_c_flag() -> Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(true));
    let f = flag.clone();
    ctrlc::set_handler(move || f.store(false, Ordering::SeqCst))
        .context("installing Ctrl-C handler")?;
    Ok(flag)
}
