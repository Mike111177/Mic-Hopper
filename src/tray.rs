//! System tray icon. Runs on its own thread with a Win32 message pump.
//! Shows a coloured badge with "D" (desktop), "H" (headset) or "-" (nothing),
//! a tooltip with live levels, and a menu to force a mic or quit.

use anyhow::{Context, Result};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::selector::Mic;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Force(Mic),
}

#[derive(Debug, Clone)]
pub struct Status {
    pub active: Option<Mic>,
    pub mode: Mode,
    pub desktop_db: f32,
    pub headset_db: f32,
    pub diff_db: f32,
    pub desktop_ok: bool,
    pub headset_ok: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum UiCommand {
    SetMode(Mode),
    Quit,
}

const SIZE: u32 = 32;

const GLYPH_D: [&str; 7] = ["####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."];
const GLYPH_H: [&str; 7] = ["#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"];
const GLYPH_DASH: [&str; 7] = [".....", ".....", ".....", "#####", ".....", ".....", "....."];

fn make_icon(active: Option<Mic>, dim: bool) -> Icon {
    let (glyph, rgb) = match active {
        Some(Mic::Desktop) => (&GLYPH_D, [46u8, 204, 113]),
        Some(Mic::Headset) => (&GLYPH_H, [52u8, 152, 219]),
        None => (&GLYPH_DASH, [140u8, 140, 140]),
    };
    let rgb = if dim { rgb.map(|c| c / 2) } else { rgb };

    let mut px = vec![0u8; (SIZE * SIZE * 4) as usize];
    let c = (SIZE as f32 - 1.0) / 2.0;
    let r = SIZE as f32 / 2.0 - 0.5;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            let d = (dx * dx + dy * dy).sqrt();
            // 1px anti-aliased edge.
            let a = ((r - d + 0.5).clamp(0.0, 1.0) * 255.0) as u8;
            let i = ((y * SIZE + x) * 4) as usize;
            px[i..i + 3].copy_from_slice(&rgb);
            px[i + 3] = a;
        }
    }

    // Letter: 5x7 glyph scaled 3x, centred.
    let scale = 3u32;
    let gw = 5 * scale;
    let gh = 7 * scale;
    let ox = (SIZE - gw) / 2;
    let oy = (SIZE - gh) / 2;
    for (gy, row) in glyph.iter().enumerate() {
        for (gx, ch) in row.bytes().enumerate() {
            if ch != b'#' {
                continue;
            }
            for sy in 0..scale {
                for sx in 0..scale {
                    let x = ox + gx as u32 * scale + sx;
                    let y = oy + gy as u32 * scale + sy;
                    let i = ((y * SIZE + x) * 4) as usize;
                    px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
    }
    Icon::from_rgba(px, SIZE, SIZE).expect("valid icon buffer")
}

fn tooltip(s: &Status) -> String {
    let active = match s.active {
        Some(m) => m.label(),
        None => "none",
    };
    let mode = match s.mode {
        Mode::Auto => "auto".to_string(),
        Mode::Force(m) => format!("forced {}", m.label().to_lowercase()),
    };
    let flag = |ok: bool| if ok { "" } else { " (offline)" };
    format!(
        "Mike Hopper: {active} [{mode}]\nDesktop {:.0} dB{}  Headset {:.0} dB{}\ndiff {:+.1} dB",
        s.desktop_db,
        flag(s.desktop_ok),
        s.headset_db,
        flag(s.headset_ok),
        s.diff_db
    )
}

/// Spawn the tray thread. Status updates go in via `status_rx`; user actions
/// come back on the returned receiver.
pub fn spawn(status_rx: Receiver<Status>) -> Result<Receiver<UiCommand>> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<UiCommand>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();

    thread::Builder::new()
        .name("tray".into())
        .spawn(move || run(status_rx, cmd_tx, ready_tx))
        .context("spawning tray thread")?;

    ready_rx.recv().context("tray thread died during startup")??;
    Ok(cmd_rx)
}

fn run(status_rx: Receiver<Status>, cmd_tx: Sender<UiCommand>, ready_tx: Sender<Result<()>>) {
    let status_item = MenuItem::new("Starting...", false, None);
    let auto_item = CheckMenuItem::new("Automatic", true, true, None);
    let desk_item = CheckMenuItem::new("Force desktop mic", true, false, None);
    let head_item = CheckMenuItem::new("Force headset mic", true, false, None);
    let quit_item = MenuItem::new("Quit", true, None);

    let menu = Menu::new();
    let built = menu
        .append_items(&[
            &status_item,
            &PredefinedMenuItem::separator(),
            &auto_item,
            &desk_item,
            &head_item,
            &PredefinedMenuItem::separator(),
            &quit_item,
        ])
        .context("building tray menu")
        .and_then(|_| {
            TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("Mike Hopper: starting")
                .with_icon(make_icon(None, false))
                .build()
                .context("creating tray icon")
        });

    let tray = match built {
        Ok(t) => {
            let _ = ready_tx.send(Ok(()));
            t
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    let menu_rx = MenuEvent::receiver();
    let mut last_active: Option<Option<Mic>> = None;
    let mut last_dim = false;

    loop {
        pump_messages();

        // Coalesce status updates; only the latest matters.
        let mut latest = None;
        loop {
            match status_rx.try_recv() {
                Ok(s) => latest = Some(s),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if let Some(s) = latest {
            let dim = match s.active {
                Some(Mic::Desktop) => !s.desktop_ok,
                Some(Mic::Headset) => !s.headset_ok,
                None => false,
            };
            if last_active != Some(s.active) || last_dim != dim {
                let _ = tray.set_icon(Some(make_icon(s.active, dim)));
                last_active = Some(s.active);
                last_dim = dim;
            }
            let _ = tray.set_tooltip(Some(tooltip(&s)));
            status_item.set_text(format!(
                "Active: {}",
                s.active.map(|m| m.label()).unwrap_or("none")
            ));
            auto_item.set_checked(s.mode == Mode::Auto);
            desk_item.set_checked(s.mode == Mode::Force(Mic::Desktop));
            head_item.set_checked(s.mode == Mode::Force(Mic::Headset));
        }

        while let Ok(ev) = menu_rx.try_recv() {
            let id = ev.id();
            let cmd = if id == quit_item.id() {
                Some(UiCommand::Quit)
            } else if id == auto_item.id() {
                Some(UiCommand::SetMode(Mode::Auto))
            } else if id == desk_item.id() {
                Some(UiCommand::SetMode(Mode::Force(Mic::Desktop)))
            } else if id == head_item.id() {
                Some(UiCommand::SetMode(Mode::Force(Mic::Headset)))
            } else {
                None
            };
            if let Some(cmd) = cmd {
                if cmd_tx.send(cmd).is_err() {
                    return;
                }
            }
        }

        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(windows)]
fn pump_messages() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(not(windows))]
fn pump_messages() {}
