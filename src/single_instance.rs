//! System-wide single-instance guard using a named Win32 mutex.

use anyhow::Result;

pub struct InstanceLock {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

/// Try to become the one running instance. Returns `None` if another process
/// already holds the lock. The lock is released when the returned guard drops
/// or the process exits.
#[cfg(windows)]
pub fn acquire() -> Result<Option<InstanceLock>> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    // `Global\` spans all logon sessions on the machine.
    let name: Vec<u16> = "Global\\MikeHopper.SingleInstance"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: valid null-terminated wide string; null security attributes.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        anyhow::bail!("CreateMutexW failed: {}", std::io::Error::last_os_error());
    }
    // SAFETY: plain FFI call with no arguments.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: handle came from CreateMutexW above.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return Ok(None);
    }
    Ok(Some(InstanceLock { handle }))
}

#[cfg(not(windows))]
pub fn acquire() -> Result<Option<InstanceLock>> {
    Ok(Some(InstanceLock {}))
}

#[cfg(windows)]
impl Drop for InstanceLock {
    fn drop(&mut self) {
        // SAFETY: handle is owned by this guard and closed exactly once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

/// Show a modal error dialog (Windows only); used when there is no console
/// to print to, e.g. when launched from the Startup shortcut.
pub fn error_dialog(title: &str, text: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let (t, m) = (wide(title), wide(text));
        // SAFETY: null owner window; valid null-terminated wide strings.
        unsafe { MessageBoxW(std::ptr::null_mut(), m.as_ptr(), t.as_ptr(), MB_OK | MB_ICONERROR) };
    }
    #[cfg(not(windows))]
    {
        let _ = (title, text);
    }
}
