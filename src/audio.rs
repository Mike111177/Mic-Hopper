//! Audio I/O. Two capture streams each feed a lock-free ring buffer; one
//! playback stream (the virtual cable) drains both every callback and mixes
//! them with a short crossfade according to the `selected` atomic.
//!
//! Each input is resampled to the output rate with a linear interpolator whose
//! ratio is nudged by a tiny proportional controller. The output callback
//! publishes the smallest amount of audio left in each ring buffer after it
//! drains; the input callback steers its resample ratio so that minimum sits
//! at `buffer_ms`. That keeps a fixed, small cushion regardless of clock drift
//! between devices and regardless of callback phase, so latency stays low
//! *and* there are no periodic underrun glitches.

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use crate::selector::Mic;

/// Ring buffer capacity in seconds of audio at the output rate. Generous so
/// the drift controller has room; the live fill level stays near `buffer_ms`.
const RING_SECONDS: f32 = 0.5;
/// Max relative pitch correction applied by the drift controller (0.2%).
const MAX_CORRECTION: f64 = 0.002;
/// Proportional gain: correction per frame of cushion error. Saturates at
/// `MAX_CORRECTION` for errors beyond ~100 frames.
const CORRECTION_GAIN: f64 = 2.0e-5;
/// Input callbacks per controller update window.
const CONTROL_WINDOW: u32 = 32;

static START: OnceLock<Instant> = OnceLock::new();

pub fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Lock-free stats published by an input callback.
pub struct InputShared {
    level_db_bits: AtomicU32,
    pub last_callback_ms: AtomicU64,
    pub failed: AtomicBool,
    pub overruns: AtomicU64,
    /// Smallest ring-buffer fill observed by the output after draining, since
    /// the input last reset it. `u32::MAX` means "no observation yet".
    min_leftover: AtomicU32,
}

impl Default for InputShared {
    fn default() -> Self {
        Self {
            level_db_bits: AtomicU32::new((-120.0f32).to_bits()),
            last_callback_ms: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            overruns: AtomicU64::new(0),
            min_leftover: AtomicU32::new(u32::MAX),
        }
    }
}

impl InputShared {
    pub fn level_db(&self) -> f32 {
        f32::from_bits(self.level_db_bits.load(Ordering::Relaxed))
    }
    fn set_level_db(&self, v: f32) {
        self.level_db_bits.store(v.to_bits(), Ordering::Relaxed);
    }
    /// Smallest post-drain fill (frames) seen so far in the current control
    /// window, for diagnostics.
    pub fn cushion_frames(&self) -> Option<u32> {
        match self.min_leftover.load(Ordering::Relaxed) {
            u32::MAX => None,
            v => Some(v),
        }
    }
}

#[derive(Default)]
pub struct OutputShared {
    pub underruns: [AtomicU64; 2],
    pub failed: AtomicBool,
}

pub struct DeviceInfo {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: SampleFormat,
}

pub fn list_devices(host: &cpal::Host) -> Result<(Vec<DeviceInfo>, Vec<DeviceInfo>)> {
    let mut ins = vec![];
    for d in host.input_devices()? {
        let name = d.name().unwrap_or_else(|_| "<unknown>".into());
        if let Ok(c) = d.default_input_config() {
            ins.push(DeviceInfo {
                name,
                sample_rate: c.sample_rate().0,
                channels: c.channels(),
                sample_format: c.sample_format(),
            });
        }
    }
    let mut outs = vec![];
    for d in host.output_devices()? {
        let name = d.name().unwrap_or_else(|_| "<unknown>".into());
        if let Ok(c) = d.default_output_config() {
            outs.push(DeviceInfo {
                name,
                sample_rate: c.sample_rate().0,
                channels: c.channels(),
                sample_format: c.sample_format(),
            });
        }
    }
    Ok((ins, outs))
}

fn name_matches(name: &str, pattern: &str) -> bool {
    name.to_lowercase().contains(&pattern.trim().to_lowercase())
}

pub fn find_input_device(host: &cpal::Host, pattern: &str) -> Result<cpal::Device> {
    host.input_devices()?
        .find(|d| d.name().map(|n| name_matches(&n, pattern)).unwrap_or(false))
        .ok_or_else(|| anyhow!("no input device matching \"{pattern}\""))
}

pub fn find_output_device(host: &cpal::Host, pattern: &str) -> Result<cpal::Device> {
    host.output_devices()?
        .find(|d| d.name().map(|n| name_matches(&n, pattern)).unwrap_or(false))
        .ok_or_else(|| anyhow!("no output device matching \"{pattern}\""))
}

/// Linear-interpolation resampler with an adjustable step.
struct Resampler {
    step: f64,
    frac: f64,
    prev: f32,
}

impl Resampler {
    fn new(step: f64) -> Self {
        // frac starts at 1.0 so the first input sample becomes `prev`
        // before any output is produced.
        Self { step, frac: 1.0, prev: 0.0 }
    }

    fn process(&mut self, input: &[f32], mut out: impl FnMut(f32)) {
        let mut idx = 0usize;
        loop {
            while self.frac >= 1.0 {
                if idx >= input.len() {
                    return;
                }
                self.prev = input[idx];
                idx += 1;
                self.frac -= 1.0;
            }
            if idx >= input.len() {
                return;
            }
            out(self.prev + (input[idx] - self.prev) * self.frac as f32);
            self.frac += self.step;
        }
    }
}

/// Owns the output stream and the producer ends of both ring buffers so input
/// streams can be (re)built at any time without touching the output.
pub struct Engine {
    host: cpal::Host,
    pub sample_rate: u32,
    target_fill: usize,
    producers: [Arc<Mutex<Producer<f32>>>; 2],
    pub inputs: [Arc<InputShared>; 2],
    pub output: Arc<OutputShared>,
    pub selected: Arc<AtomicU8>,
    _output_stream: cpal::Stream,
}

impl Engine {
    pub fn new(
        host: cpal::Host,
        output_pattern: &str,
        buffer_ms: f32,
        crossfade_ms: f32,
        initial: Mic,
    ) -> Result<Self> {
        let out_dev = find_output_device(&host, output_pattern)?;
        let out_name = out_dev.name().unwrap_or_default();
        let out_cfg = out_dev
            .default_output_config()
            .with_context(|| format!("querying output config for \"{out_name}\""))?;
        let sample_rate = out_cfg.sample_rate().0;
        let out_channels = out_cfg.channels();
        log::info!(
            "output: \"{out_name}\" {} Hz, {} ch, {:?}",
            sample_rate,
            out_channels,
            out_cfg.sample_format()
        );

        let capacity = (sample_rate as f32 * RING_SECONDS) as usize;
        let target_fill = (sample_rate as f32 * buffer_ms / 1000.0).round() as usize;
        let fade_frames = ((sample_rate as f32 * crossfade_ms / 1000.0).round() as usize).max(1);

        let (p0, c0) = RingBuffer::<f32>::new(capacity);
        let (p1, c1) = RingBuffer::<f32>::new(capacity);

        let inputs = [Arc::new(InputShared::default()), Arc::new(InputShared::default())];
        let output = Arc::new(OutputShared::default());
        let selected = Arc::new(AtomicU8::new(initial.index() as u8));

        let stream_cfg = StreamConfig {
            channels: out_channels,
            sample_rate: out_cfg.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        let mixer = Mixer {
            lanes: [
                Lane { consumer: c0, primed: false },
                Lane { consumer: c1, primed: false },
            ],
            gains: match initial {
                Mic::Desktop => [1.0, 0.0],
                Mic::Headset => [0.0, 1.0],
            },
            fade_step: 1.0 / fade_frames as f32,
            target_fill,
            capacity,
            selected: selected.clone(),
            shared: output.clone(),
            inputs: [inputs[0].clone(), inputs[1].clone()],
        };

        let output_stream = match out_cfg.sample_format() {
            SampleFormat::F32 => build_output::<f32>(&out_dev, &stream_cfg, mixer, output.clone()),
            SampleFormat::I16 => build_output::<i16>(&out_dev, &stream_cfg, mixer, output.clone()),
            SampleFormat::U16 => build_output::<u16>(&out_dev, &stream_cfg, mixer, output.clone()),
            SampleFormat::I32 => build_output::<i32>(&out_dev, &stream_cfg, mixer, output.clone()),
            other => bail!("unsupported output sample format {other:?}"),
        }
        .with_context(|| format!("building output stream for \"{out_name}\""))?;
        output_stream.play().context("starting output stream")?;

        Ok(Self {
            host,
            sample_rate,
            target_fill,
            producers: [Arc::new(Mutex::new(p0)), Arc::new(Mutex::new(p1))],
            inputs,
            output,
            selected,
            _output_stream: output_stream,
        })
    }

    pub fn set_selected(&self, mic: Mic) {
        self.selected.store(mic.index() as u8, Ordering::Relaxed);
    }

    /// Build and start a capture stream for `mic`. The returned stream must be
    /// kept alive by the caller; dropping it stops capture. Safe to call again
    /// after a failure (the ring buffer is reused).
    pub fn start_input(&self, mic: Mic, pattern: &str) -> Result<cpal::Stream> {
        let dev = find_input_device(&self.host, pattern)?;
        let name = dev.name().unwrap_or_default();
        let cfg = dev
            .default_input_config()
            .with_context(|| format!("querying input config for \"{name}\""))?;
        let in_rate = cfg.sample_rate().0;
        let channels = cfg.channels();
        log::info!(
            "{} input: \"{name}\" {} Hz, {} ch, {:?}",
            mic.label(),
            in_rate,
            channels,
            cfg.sample_format()
        );
        if in_rate != self.sample_rate {
            log::warn!(
                "{} input runs at {} Hz but output is {} Hz; resampling (set both to the same rate for best quality)",
                mic.label(),
                in_rate,
                self.sample_rate
            );
        }

        let stream_cfg = StreamConfig {
            channels,
            sample_rate: cfg.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        let shared = self.inputs[mic.index()].clone();
        shared.failed.store(false, Ordering::Relaxed);
        shared.last_callback_ms.store(now_ms(), Ordering::Relaxed);
        shared.min_leftover.store(u32::MAX, Ordering::Relaxed);

        let capture = Capture {
            producer: self.producers[mic.index()].clone(),
            shared: shared.clone(),
            channels: channels as usize,
            base_step: in_rate as f64 / self.sample_rate as f64,
            resampler: Resampler::new(in_rate as f64 / self.sample_rate as f64),
            target_fill: self.target_fill as f64,
            correction: 0.0,
            window_count: 0,
            mono: Vec::with_capacity(16384),
        };

        let stream = match cfg.sample_format() {
            SampleFormat::F32 => build_input::<f32>(&dev, &stream_cfg, capture, shared),
            SampleFormat::I16 => build_input::<i16>(&dev, &stream_cfg, capture, shared),
            SampleFormat::U16 => build_input::<u16>(&dev, &stream_cfg, capture, shared),
            SampleFormat::I32 => build_input::<i32>(&dev, &stream_cfg, capture, shared),
            other => bail!("unsupported input sample format {other:?}"),
        }
        .with_context(|| format!("building input stream for \"{name}\""))?;
        stream.play().with_context(|| format!("starting input stream for \"{name}\""))?;
        Ok(stream)
    }
}

struct Capture {
    producer: Arc<Mutex<Producer<f32>>>,
    shared: Arc<InputShared>,
    channels: usize,
    base_step: f64,
    resampler: Resampler,
    target_fill: f64,
    correction: f64,
    window_count: u32,
    mono: Vec<f32>,
}

impl Capture {
    fn process<T>(&mut self, data: &[T])
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        self.shared.last_callback_ms.store(now_ms(), Ordering::Relaxed);
        let ch = self.channels.max(1);
        let frames = data.len() / ch;
        if frames == 0 {
            return;
        }

        // Downmix to mono and measure RMS.
        self.mono.clear();
        let mut sum_sq = 0.0f32;
        let inv_ch = 1.0 / ch as f32;
        for frame in data.chunks_exact(ch) {
            let mut s = 0.0f32;
            for &x in frame {
                s += f32::from_sample(x);
            }
            s *= inv_ch;
            sum_sq += s * s;
            self.mono.push(s);
        }
        let rms_db = 10.0 * (sum_sq / frames as f32 + 1e-12).log10();
        self.shared.set_level_db(rms_db);

        // Drift control: once per window, look at the smallest post-drain fill
        // the output saw and steer the resample ratio so it converges on
        // `target_fill`. Positive error (too much buffered) -> consume input
        // faster -> fewer output samples -> fill shrinks.
        self.window_count += 1;
        if self.window_count >= CONTROL_WINDOW {
            self.window_count = 0;
            let min = self.shared.min_leftover.swap(u32::MAX, Ordering::Relaxed);
            if min != u32::MAX {
                let error = min as f64 - self.target_fill;
                self.correction = (error * CORRECTION_GAIN).clamp(-MAX_CORRECTION, MAX_CORRECTION);
            }
        }
        self.resampler.step = self.base_step * (1.0 + self.correction);

        // The mutex is never contended: the main thread only clones the Arc.
        let Ok(mut prod) = self.producer.try_lock() else {
            return;
        };
        let mut overruns = 0u64;
        self.resampler.process(&self.mono, |x| {
            if prod.push(x).is_err() {
                overruns += 1;
            }
        });
        if overruns > 0 {
            self.shared.overruns.fetch_add(overruns, Ordering::Relaxed);
        }
    }
}

fn build_input<T>(
    dev: &cpal::Device,
    cfg: &StreamConfig,
    mut capture: Capture,
    shared: Arc<InputShared>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    dev.build_input_stream(
        cfg,
        move |data: &[T], _: &cpal::InputCallbackInfo| capture.process(data),
        move |err| {
            log::error!("input stream error: {err}");
            shared.failed.store(true, Ordering::Relaxed);
        },
        None,
    )
}

struct Lane {
    consumer: Consumer<f32>,
    primed: bool,
}

struct Mixer {
    lanes: [Lane; 2],
    gains: [f32; 2],
    fade_step: f32,
    target_fill: usize,
    capacity: usize,
    selected: Arc<AtomicU8>,
    shared: Arc<OutputShared>,
    inputs: [Arc<InputShared>; 2],
}

impl Mixer {
    fn render<T>(&mut self, out: &mut [T], channels: usize)
    where
        T: SizedSample + FromSample<f32>,
    {
        let frames = out.len() / channels.max(1);
        if frames == 0 {
            return;
        }
        let target = self.selected.load(Ordering::Relaxed) as usize;

        for (i, lane) in self.lanes.iter_mut().enumerate() {
            let avail = lane.consumer.slots();
            // Pathological backlog (e.g. output was stalled): snap back to target.
            if avail > self.capacity / 2 {
                for _ in 0..(avail - self.target_fill) {
                    let _ = lane.consumer.pop();
                }
            }
            // Wait for a cushion before starting (or restarting after underrun)
            // so the first chunks don't immediately starve.
            if !lane.primed && lane.consumer.slots() >= frames + self.target_fill {
                lane.primed = true;
            }
            // Only count underruns on the audible lane.
            if lane.primed && lane.consumer.slots() < frames && i == target {
                self.shared.underruns[i].fetch_add(1, Ordering::Relaxed);
            }
        }

        for frame in out.chunks_exact_mut(channels.max(1)) {
            let mut mix = 0.0f32;
            for (i, lane) in self.lanes.iter_mut().enumerate() {
                let goal = if i == target { 1.0 } else { 0.0 };
                let g = &mut self.gains[i];
                if *g < goal {
                    *g = (*g + self.fade_step).min(1.0);
                } else if *g > goal {
                    *g = (*g - self.fade_step).max(0.0);
                }

                let s = if lane.primed {
                    match lane.consumer.pop() {
                        Ok(s) => s,
                        Err(_) => {
                            lane.primed = false;
                            0.0
                        }
                    }
                } else {
                    0.0
                };
                if *g > 0.0 {
                    mix += s * *g;
                }
            }
            let v = T::from_sample(mix.clamp(-1.0, 1.0));
            for x in frame.iter_mut() {
                *x = v;
            }
        }

        // Publish what is left after draining; the input side steers its
        // resample ratio to keep the minimum of this at `target_fill`.
        for (i, lane) in self.lanes.iter().enumerate() {
            if lane.primed {
                let left = lane.consumer.slots().min(u32::MAX as usize) as u32;
                self.inputs[i].min_leftover.fetch_min(left, Ordering::Relaxed);
            }
        }
    }
}

fn build_output<T>(
    dev: &cpal::Device,
    cfg: &StreamConfig,
    mut mixer: Mixer,
    shared: Arc<OutputShared>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = cfg.channels as usize;
    dev.build_output_stream(
        cfg,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| mixer.render(data, channels),
        move |err| {
            log::error!("output stream error: {err}");
            shared.failed.store(true, Ordering::Relaxed);
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_identity_passes_samples_through() {
        let mut r = Resampler::new(1.0);
        let mut out = vec![];
        r.process(&[1.0, 2.0, 3.0, 4.0], |x| out.push(x));
        r.process(&[5.0, 6.0], |x| out.push(x));
        // Linear interpolation needs one sample of lookahead, so the final
        // sample of the latest block is held until the next block arrives.
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        r.process(&[7.0], |x| out.push(x));
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn resampler_upsamples_by_three() {
        // 16 kHz -> 48 kHz: step = 1/3, three outputs per input.
        let mut r = Resampler::new(1.0 / 3.0);
        let mut out = vec![];
        r.process(&[0.0, 3.0], |x| out.push(x));
        r.process(&[6.0], |x| out.push(x));
        assert_eq!(out.len(), 6);
        for (a, b) in out.iter().zip([0.0, 1.0, 2.0, 3.0, 4.0, 5.0]) {
            assert!((a - b).abs() < 1e-5, "{out:?}");
        }
    }

    #[test]
    fn resampler_downsamples_consistently_across_blocks() {
        // 48k -> 44.1k roughly; output count over many samples should match ratio.
        let step = 48000.0 / 44100.0;
        let mut r = Resampler::new(step);
        let mut n = 0usize;
        let block = vec![0.5f32; 480];
        for _ in 0..100 {
            r.process(&block, |_| n += 1);
        }
        let expected = (48000.0 / step) as usize;
        assert!((n as i64 - expected as i64).abs() <= 2, "got {n}, expected ~{expected}");
    }
}
