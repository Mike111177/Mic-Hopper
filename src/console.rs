//! Console management for a GUI-subsystem binary.
//!
//! The exe is built with `windows_subsystem = "windows"` so the tray app never
//! opens a console window. CLI modes call [`attach`] or [`own`] to get one:
//! attach to the launching terminal when there is one, otherwise create a
//! fresh window.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set when we created our own console window; it closes when the process
/// exits, so interactive modes pause before quitting.
static OWNED: AtomicBool = AtomicBool::new(false);
/// Set when stdout is connected to a console (attached or owned).
static HAVE: AtomicBool = AtomicBool::new(false);

/// Attach to the parent process's console (e.g. the PowerShell window that
/// launched us). Returns whether a console is now available.
#[cfg(windows)]
pub fn attach() -> bool {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    if HAVE.load(Ordering::Relaxed) {
        return true;
    }
    // SAFETY: plain FFI call; failure (no parent console) is reported via the
    // return value and is expected when launched from Explorer or a shortcut.
    let ok = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0;
    HAVE.store(ok, Ordering::Relaxed);
    ok
}

/// Create a dedicated console window for interactive use. Used for `setup`:
/// sharing stdin with a parent shell's prompt would make both fight over
/// keystrokes, so interactive modes always get their own window.
#[cfg(windows)]
pub fn own() -> bool {
    use windows_sys::Win32::System::Console::{AllocConsole, FreeConsole};
    if OWNED.load(Ordering::Relaxed) {
        return true;
    }
    // SAFETY: plain FFI calls. FreeConsole detaches any inherited console so
    // AllocConsole can succeed; both are harmless if there is none.
    unsafe { FreeConsole() };
    let ok = unsafe { AllocConsole() } != 0;
    OWNED.store(ok, Ordering::Relaxed);
    HAVE.store(ok, Ordering::Relaxed);
    ok
}

#[cfg(not(windows))]
pub fn attach() -> bool {
    HAVE.store(true, Ordering::Relaxed);
    true
}

#[cfg(not(windows))]
pub fn own() -> bool {
    HAVE.store(true, Ordering::Relaxed);
    true
}

/// Whether stdout/stderr currently reach a console.
pub fn available() -> bool {
    HAVE.load(Ordering::Relaxed)
}

/// If we opened our own console window, wait for Enter so the user can read
/// the output before the window disappears.
pub fn pause_if_owned() {
    if !OWNED.load(Ordering::Relaxed) {
        return;
    }
    use std::io::{BufRead, Write};
    print!("\nPress Enter to close this window...");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}
