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
    open_on(&device)
}

/// Pick a capture configuration, preferring formats that need no conversion.
/// Integer formats are converted in the callback (see `dsp::to_mono`), because
/// plenty of devices — and every null/monitor sink — offer only those.
fn choose_input_range(
    ranges: impl Iterator<Item = SupportedStreamConfigRange>,
) -> std::result::Result<SupportedStreamConfigRange, String> {
    let mut best: Option<(u8, SupportedStreamConfigRange)> = None;
    let mut offered: Vec<String> = Vec::new();
    for range in ranges {
        let rank = match range.sample_format() {
            SampleFormat::F32 => 0u8,
            SampleFormat::I16 => 1,
            SampleFormat::I32 => 2,
            SampleFormat::U16 => 3,
            other => {
                offered.push(format!("{other:?}"));
                continue;
            }
        };
        offered.push(format!("{:?}", range.sample_format()));
        if best.is_none_or(|(rank_best, _)| rank < rank_best) {
            best = Some((rank, range));
        }
    }
    best.map(|(_, range)| range)
        .ok_or_else(|| format!("has no supported input sample format (offers: {})", offered.join(", ")))
}

/// Build and start an input stream on an already-chosen `device`.
///
/// Devices do not always advertise usable input configurations: a PulseAudio
/// monitor of a PipeWire null sink reports none at all, and some ALSA devices
/// advertise only integer formats. So: use the advertised ranges when there
/// are any, else the device default, else a few safe stream configs — whichever
/// the backend accepts first wins.
fn open_on(device: &Device) -> Result<Capture> {
    let device_name = dev_name(device);
    let err_cb = |err| eprintln!("wheezper: audio stream error: {err}");

    let mut candidates: Vec<(SampleFormat, StreamConfig)> = Vec::new();
    let ranges: Vec<SupportedStreamConfigRange> = device
        .supported_input_configs()
        .map(|ranges| ranges.collect())
        .unwrap_or_default();
    match choose_input_range(ranges.into_iter()) {
        Ok(range) => {
            let rate = pick_rate(&range);
            candidates.push((range.sample_format(), range.with_sample_rate(rate).into()));
        }
        Err(e) if device.default_input_config().is_err() => {
            eprintln!("wheezper: {device_name} {e}, trying default stream configs");
        }
        Err(_) => {}
    }
    match device.default_input_config() {
        Ok(default) => candidates.push((default.sample_format(), default.config())),
        Err(_) => {
            // WASAPI loopback: the "input" is an output device, and the shared
            // audio client only accepts the device's mix format — which is what
            // default_output_config() reports.
            if let Ok(mix) = device.default_output_config() {
                candidates.insert(0, (mix.sample_format(), mix.config()));
            }
        }
    }
    // last resort: formats every backend can resample to
    for (format, channels, rate) in [
        (SampleFormat::F32, 2u16, 48_000u32),
        (SampleFormat::I16, 2, 48_000),
        (SampleFormat::F32, 1, 48_000),
        (SampleFormat::F32, 2, 44_100),
        (SampleFormat::F32, 1, TARGET_RATE),
        (SampleFormat::I16, 1, TARGET_RATE),
    ] {
        candidates.push((
            format,
            StreamConfig {
                channels,
                sample_rate: rate,
                buffer_size: cpal::BufferSize::Default,
            },
        ));
    }

    let mut last_err: Option<anyhow::Error> = None;
    for (format, config) in candidates {
        let rate = config.sample_rate;
        let channels = config.channels;
        // ~3 minutes of buffered audio; overflow drops the oldest chunks.
        let (sender, receiver) = audio_queue(rate as usize * 180);
        let built = match format {
            SampleFormat::F32 => build_input::<f32>(device, config, channels as usize, sender, err_cb),
            SampleFormat::I16 => build_input::<i16>(device, config, channels as usize, sender, err_cb),
            SampleFormat::I32 => build_input::<i32>(device, config, channels as usize, sender, err_cb),
            SampleFormat::U16 => build_input::<u16>(device, config, channels as usize, sender, err_cb),
            other => {
                last_err = Some(anyhow!("unsupported sample format {other:?}"));
                continue;
            }
        };
        match built.and_then(|stream| {
            stream.play().context("starting input stream")?;
            Ok(Capture { stream, receiver, sample_rate: rate, channels, device_name: device_name.clone() })
        }) {
            Ok(capture) => return Ok(capture),
            Err(e) => {
                eprintln!("wheezper: {device_name}: {format:?} {channels}ch {rate}Hz rejected: {e}");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("device '{device_name}' has no usable input configuration")))
}

/// Build one input stream, converting whatever the device delivers to mono f32.
fn build_input<T: dsp::SampleConv + cpal::SizedSample + 'static>(
    device: &Device,
    config: StreamConfig,
    channels: usize,
    sender: AudioSender,
    err_cb: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<Stream> {
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| sender.push(dsp::to_mono(data, channels)),
            err_cb,
            // bounded: a misbehaving device must not hang startup forever
            Some(Duration::from_secs(10)),
        )
        .context("building input stream")
}

/// Open an input stream on EVERY loopback monitor source in the system.
///
/// Silent sinks deliver silence (their monitors still run), so nothing
/// spurious is transcribed — but no output device can escape capture,
/// whichever sink an application happens to play to.
pub fn open_all_monitors(host: &Host) -> Result<Vec<Capture>> {
    let mut monitors = Vec::new();
    for d in host.devices().context("listing devices")? {
        if dev_id(&d).to_ascii_lowercase().ends_with(".monitor")
            && d.supported_input_configs().is_ok()
        {
            monitors.push(d);
        }
    }
    if monitors.is_empty() {
        if host_id_name(host).to_ascii_lowercase().contains("wasapi") {
            bail!(
                "--all-monitors is a PulseAudio/PipeWire feature (every sink monitor); \
                 on Windows just use --source loopback or --source dual, which capture \
                 the default output device Teams plays through"
            );
        }
        bail!("no loopback monitor sources found (PulseAudio/PipeWire server has no sinks?)");
    }
    monitors.iter().map(open_on).collect()
}

fn pick_device(host: &Host, mode: SourceMode, substr: Option<&str>) -> Result<Device> {
    match mode {
        SourceMode::Mic => match substr {
            Some(s) => find_device(host, s, SourceMode::Mic),
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow!("no default input device found")),
        },
        SourceMode::Loopback => loopback_device(host, substr),
    }
}

/// Case-insensitive substring match, mode-aware.
///
/// Ambiguity is the norm: a PulseAudio sink and its monitor share a name, and
/// on Windows a *loopback* target is an **output** device that advertises no
/// input configs at all. So the ranking depends on what is being captured:
///
/// - `Mic`: only real capture devices (input configs) qualify.
/// - `Loopback`: a monitor source, or — on WASAPI — an output device (cpal
///   enables loopback when an output device is opened as input). Plain capture
///   sources are a last resort for explicit `--device` requests.
fn find_device(host: &Host, s: &str, mode: SourceMode) -> Result<Device> {
    let want = s.to_ascii_lowercase();
    let wasapi = host_id_name(host).to_ascii_lowercase().contains("wasapi");
    let mut ranked: Vec<(u8, Device)> = Vec::new();
    let mut names: Vec<String> = Vec::new();

    for device in host.devices().context("listing devices")? {
        let name = dev_name(&device);
        let id = dev_id(&device);
        if !name.to_ascii_lowercase().contains(&want) && !id.to_ascii_lowercase().contains(&want) {
            continue;
        }
        names.push(name);
        let capture = device
            .supported_input_configs()
            .map(|mut ranges| ranges.next().is_some())
            .unwrap_or(false);
        let monitor = id.to_ascii_lowercase().ends_with(".monitor");
        let rank = match mode {
            SourceMode::Mic => match (capture, monitor) {
                (true, _) => Some(0u8), // a real microphone / capture source
                _ => None,              // a monitor or an output device is not a mic
            },
            SourceMode::Loopback => match (monitor, capture, wasapi) {
                (true, _, _) => Some(0),           // PulseAudio/PipeWire monitor
                (false, false, true) => Some(0),   // WASAPI output device -> loopback
                (false, true, _) => Some(1),       // explicit capture source
                (false, false, false) => None,     // nothing capturable here
            },
        };
        if let Some(rank) = rank {
            ranked.push((rank, device));
        }
    }
    if ranked.is_empty() {
        let what = match mode {
            SourceMode::Mic => "an input device",
            SourceMode::Loopback => "a loopback/monitor or output device",
        };
        bail!(
            "no device matching '{s}' can provide {what} (matched: {})",
            if names.is_empty() { "nothing".to_string() } else { names.join(", ") }
        );
    }
    ranked.sort_by_key(|(rank, _)| *rank);
    Ok(ranked.remove(0).1)
}

fn loopback_device(host: &Host, substr: Option<&str>) -> Result<Device> {
    let host_name = host_id_name(host).to_ascii_lowercase();
    if host_name.contains("pulseaudio") {
        if let Some(s) = substr {
            return find_device(host, s, SourceMode::Loopback);
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
            Some(s) => find_device(host, s, SourceMode::Mic),
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

#[derive(Clone)]
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
