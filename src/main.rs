// No console window is created for the tray app; CLI modes attach to the
// launching terminal or open their own window (see `console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod audio;
mod config;
mod console;
mod selector;
mod setup;
mod single_instance;
mod tray;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use audio::{now_ms, Engine};
use config::{Config, StartWith};
use selector::{Mic, Selector};
use tray::{Mode, Status, UiCommand};

/// Routes one of two microphones to a virtual mic based on a volume heuristic.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to the TOML config (default: mike-hopper.toml next to the exe, then cwd).
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// List audio devices and exit.
    #[arg(short, long)]
    list_devices: bool,

    /// Print live levels to the console (for calibration). Implies --console.
    #[arg(short, long)]
    monitor: bool,

    /// Print logs to the terminal this was launched from.
    #[arg(long)]
    console: bool,

    /// Run without a tray icon.
    #[arg(long)]
    no_tray: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Interactively choose devices and write the config file.
    Setup,
}

const TICK: Duration = Duration::from_millis(10);
const STALE_MS: u64 = 2500;
const RETRY_MS: u64 = 3000;
const TRAY_REFRESH_MS: u64 = 500;
const MONITOR_MS: u64 = 250;

struct InputSlot {
    mic: Mic,
    pattern: String,
    stream: Option<cpal::Stream>,
    next_retry_ms: u64,
    was_online: bool,
}

impl InputSlot {
    fn new(mic: Mic, pattern: String) -> Self {
        Self { mic, pattern, stream: None, next_retry_ms: 0, was_online: false }
    }

    /// Drop a dead stream, (re)build when due. Returns whether the input is live.
    fn service(&mut self, engine: &Engine, now: u64) -> bool {
        let shared = &engine.inputs[self.mic.index()];
        if self.stream.is_some() {
            let failed = shared.failed.load(Ordering::Relaxed);
            let stale = now.saturating_sub(shared.last_callback_ms.load(Ordering::Relaxed)) > STALE_MS;
            if failed || stale {
                log::warn!(
                    "{} mic went offline ({})",
                    self.mic.label(),
                    if failed { "stream error" } else { "no data" }
                );
                self.stream = None;
                self.next_retry_ms = now + RETRY_MS;
            }
        }
        if self.stream.is_none() && now >= self.next_retry_ms {
            match engine.start_input(self.mic, &self.pattern) {
                Ok(s) => {
                    self.stream = Some(s);
                    log::info!("{} mic online", self.mic.label());
                }
                Err(e) => {
                    if self.was_online || self.next_retry_ms == 0 {
                        log::warn!("{} mic unavailable: {e:#}", self.mic.label());
                    }
                    self.next_retry_ms = now + RETRY_MS;
                }
            }
        }
        let online = self.stream.is_some();
        self.was_online = online;
        online
    }
}

/// Candidate config locations in priority order: `--config`, else next to the
/// exe, then the current directory.
fn config_candidates(cli: &Cli) -> Vec<PathBuf> {
    if let Some(p) = &cli.config {
        return vec![p.clone()];
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    exe_dir
        .iter()
        .map(|d| d.join("mike-hopper.toml"))
        .chain(std::iter::once(PathBuf::from("mike-hopper.toml")))
        .collect()
}

fn resolve_config_path(cli: &Cli) -> Result<PathBuf> {
    let candidates = config_candidates(cli);
    if let Some(found) = candidates.iter().find(|p| p.exists()) {
        return Ok(found.clone());
    }
    bail!(
        "no config found at {}.\nRun `mike-hopper setup` to create one interactively.",
        candidates[0].display()
    );
}

fn print_devices(host: &cpal::Host) -> Result<()> {
    let (ins, outs) = audio::list_devices(host)?;
    println!("Input devices (use for [devices].desktop / headset):");
    for d in ins {
        println!("  {:<60} {} Hz, {} ch, {:?}", d.name, d.sample_rate, d.channels, d.sample_format);
    }
    println!("\nOutput devices (use for [devices].output — pick the virtual cable's playback side):");
    for d in outs {
        println!("  {:<60} {} Hz, {} ch, {:?}", d.name, d.sample_rate, d.channels, d.sample_format);
    }
    Ok(())
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // --help, --version, or a usage error: make sure it lands somewhere.
            if !console::attach() {
                console::own();
            }
            let _ = e.print();
            console::pause_if_owned();
            std::process::exit(e.exit_code());
        }
    };

    if matches!(cli.command, Some(Command::Setup)) {
        // Interactive: a dedicated window, so stdin isn't shared with a shell prompt.
        console::own();
    } else if cli.list_devices || cli.monitor || cli.console {
        if !console::attach() {
            console::own();
        }
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let result = run(cli);
    if let Err(e) = &result {
        if console::available() {
            eprintln!("error: {e:#}");
        } else {
            single_instance::error_dialog("Mike Hopper", &format!("{e:#}"));
        }
    }
    console::pause_if_owned();
    if result.is_err() {
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let host = cpal::default_host();

    if cli.list_devices {
        return print_devices(&host);
    }

    if let Some(Command::Setup) = cli.command {
        // Prefer an existing config location; otherwise create next to the exe.
        let candidates = config_candidates(&cli);
        let path = candidates
            .iter()
            .find(|p| p.exists())
            .unwrap_or(&candidates[0])
            .clone();
        return setup::run(&host, &path);
    }

    // One running instance system-wide; two would both write to the cable.
    let _instance_lock = match single_instance::acquire()? {
        Some(lock) => lock,
        None => bail!("Mike Hopper is already running. Quit it from the tray icon first."),
    };

    let cfg_path = resolve_config_path(&cli)?;
    let cfg = Config::load(&cfg_path)?;
    log::info!("config: {}", cfg_path.display());

    let start = match cfg.switching.start_with {
        StartWith::Desktop => Mic::Desktop,
        StartWith::Headset => Mic::Headset,
    };

    let engine = Engine::new(
        host,
        &cfg.devices.output,
        cfg.audio.buffer_ms,
        cfg.audio.crossfade_ms,
        start,
    )
    .context("starting audio output (is the virtual cable installed and its name correct?)")?;

    let mut slots = [
        InputSlot::new(Mic::Desktop, cfg.devices.desktop.clone()),
        InputSlot::new(Mic::Headset, cfg.devices.headset.clone()),
    ];

    let (status_tx, status_rx) = std::sync::mpsc::channel::<Status>();
    let cmd_rx = if cli.no_tray {
        drop(status_rx);
        None
    } else {
        Some(tray::spawn(status_rx).context("starting tray icon")?)
    };

    let mut selector = Selector::new(cfg.switching.clone(), start);
    let mut mode = Mode::Auto;
    let mut active: Option<Mic> = None;
    let mut last_tick = Instant::now();
    let mut last_tray_ms = 0u64;
    let mut last_monitor_ms = 0u64;
    let mut last_sent_active: Option<Option<Mic>> = None;
    let mut last_sent_mode = mode;

    log::info!("running; starting with {} mic", start.label());

    loop {
        let now = now_ms();
        let dt_ms = last_tick.elapsed().as_secs_f32() * 1000.0;
        last_tick = Instant::now();

        if engine.output.failed.load(Ordering::Relaxed) {
            bail!("output stream failed (virtual cable removed?); exiting");
        }

        if let Some(rx) = &cmd_rx {
            match rx.try_recv() {
                Ok(UiCommand::Quit) => break,
                Ok(UiCommand::SetMode(m)) => {
                    mode = m;
                    log::info!("mode: {m:?}");
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => bail!("tray thread exited"),
            }
        }

        let online = [slots[0].service(&engine, now), slots[1].service(&engine, now)];

        let desk_db = engine.inputs[0].level_db();
        let head_db = engine.inputs[1].level_db();
        let desk_db = if online[0] { desk_db } else { -120.0 };
        let head_db = if online[1] { head_db } else { -120.0 };

        if let Some(m) = selector.update(desk_db, head_db, dt_ms) {
            log::info!("auto-switch -> {} mic", m.label());
        }

        let wanted = match mode {
            Mode::Auto => selector.current(),
            Mode::Force(m) => {
                selector.force(m);
                m
            }
        };
        let new_active = if online[wanted.index()] {
            Some(wanted)
        } else if online[wanted.other().index()] {
            selector.force(wanted.other());
            Some(wanted.other())
        } else {
            None
        };
        if new_active != active {
            if let Some(m) = new_active {
                engine.set_selected(m);
            }
            log::info!(
                "active mic: {}",
                new_active.map(|m| m.label()).unwrap_or("none (no inputs online)")
            );
            active = new_active;
        }

        let changed = last_sent_active != Some(active) || last_sent_mode != mode;
        if changed || now.saturating_sub(last_tray_ms) >= TRAY_REFRESH_MS {
            let lv = selector.levels();
            let _ = status_tx.send(Status {
                active,
                mode,
                desktop_db: lv.desktop_db,
                headset_db: lv.headset_db,
                diff_db: lv.diff_db,
                desktop_ok: online[0],
                headset_ok: online[1],
            });
            last_tray_ms = now;
            last_sent_active = Some(active);
            last_sent_mode = mode;
        }

        if cli.monitor && now.saturating_sub(last_monitor_ms) >= MONITOR_MS {
            last_monitor_ms = now;
            let lv = selector.levels();
            let cushion = |i: usize| -> String {
                match engine.inputs[i].cushion_frames() {
                    Some(f) => format!("{:.1}", f as f32 * 1000.0 / engine.sample_rate as f32),
                    None => "-".into(),
                }
            };
            println!(
                "desk {:>6.1} dB  head {:>6.1} dB  diff {:>+6.1} dB  {}  active: {:<7}  cushion ms D:{} H:{}  underruns D:{} H:{}  overruns D:{} H:{}",
                lv.desktop_db,
                lv.headset_db,
                lv.diff_db,
                if lv.speaking { "SPEAKING" } else { "silent  " },
                active.map(|m| m.label()).unwrap_or("none"),
                cushion(0),
                cushion(1),
                engine.output.underruns[0].load(Ordering::Relaxed),
                engine.output.underruns[1].load(Ordering::Relaxed),
                engine.inputs[0].overruns.load(Ordering::Relaxed),
                engine.inputs[1].overruns.load(Ordering::Relaxed),
            );
        }

        std::thread::sleep(TICK);
    }

    log::info!("quitting");
    Ok(())
}
