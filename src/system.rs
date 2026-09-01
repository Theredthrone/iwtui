//! Hostname get/set via raw libc, plus a sudo-based escalation path so
//! non-root users get a **password prompt** — exactly the nmtui experience —
//! instead of a "Permission denied" error.
//!
//! `sudo -S` reads the password from stdin (never a TTY), so the prompt can
//! live inside the TUI. The hostname is passed as an argv element (never
//! interpolated into a shell string), and every change is validated first.

use std::ffi::{CStr, CString};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::{err, AppResult};

/// Why a hostname change failed. `AuthFailed` re-opens the password dialog
/// ("wrong password"); everything else is shown as a plain error.
#[derive(Debug)]
pub enum HostnameSetError {
    AuthFailed,
    Other(String),
}

impl std::fmt::Display for HostnameSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostnameSetError::AuthFailed => write!(f, "authentication failed"),
            HostnameSetError::Other(e) => write!(f, "{e}"),
        }
    }
}

/// The one and only hostname validator. Returns a user-facing message on
/// error, so the UI can show it inline while the user types.
pub fn validate_hostname(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("The hostname cannot be empty".into());
    }
    if name.len() > 63 {
        return Err("Too long — a hostname can be at most 63 characters".into());
    }
    if name.starts_with('-') || name.starts_with('.') {
        return Err("The hostname cannot start with '-' or '.'".into());
    }
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '-' && ch != '.' {
            return Err(format!(
                "'{ch}' is not allowed — use letters, digits, '-' and '.'"
            ));
        }
    }
    Ok(())
}

pub fn get_hostname() -> AppResult<String> {
    let mut buf = [0u8; 256];
    let res = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if res != 0 {
        return Err(err("gethostname(2) failed"));
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *mut libc::c_char) };
    Ok(cstr.to_string_lossy().into_owned())
}

/// Direct kernel set (works when already root / has CAP_SYS_ADMIN).
/// Persisting to `/etc/hostname` is best-effort.
pub fn set_hostname(name: &str) -> Result<(), HostnameSetError> {
    let name = name.trim();
    if let Err(msg) = validate_hostname(name) {
        return Err(HostnameSetError::Other(msg));
    }

    // libc::sethostname expects a valid C-string (null-terminated).
    let c_name = CString::new(name).map_err(|_| {
        HostnameSetError::Other(
            "That hostname contains a character this system cannot store".into(),
        )
    })?;
    let res = unsafe { libc::sethostname(c_name.as_ptr(), c_name.as_bytes().len()) };
    if res != 0 {
        let os_err = std::io::Error::last_os_error();
        if os_err.raw_os_error() == Some(libc::EPERM) {
            // The caller turns this into a root-password prompt.
            return Err(HostnameSetError::AuthFailed);
        }
        return Err(HostnameSetError::Other(format!(
            "The system refused the hostname change: {os_err}"
        )));
    }

    persist_hostname(name);
    Ok(())
}

/// Write `/etc/hostname` (best effort — the kernel set already happened).
fn persist_hostname(name: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open("/etc/hostname")
    {
        let _ = file.write_all(name.as_bytes());
        let _ = file.write_all(b"\n");
    }
}

/// Locate `sudo` on PATH and return its absolute path, so we never execute
/// anything off PATH while handling a password.
fn find_sudo() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("sudo");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Set the hostname by escalating through `sudo -S` with the password fed
/// on stdin (blocking — run it from `spawn_blocking`). The live kernel
/// hostname is set via `/proc/sys/kernel/hostname` and `/etc/hostname` is
/// updated in the same privileged command, so a single password entry
/// covers both.
pub fn set_hostname_elevated(name: &str, password: &str) -> Result<(), HostnameSetError> {
    let name = name.trim();
    if let Err(msg) = validate_hostname(name) {
        return Err(HostnameSetError::Other(msg));
    }
    let Some(sudo) = find_sudo() else {
        return Err(HostnameSetError::Other(
            "sudo was not found — install sudo, or run iwtui as root".into(),
        ));
    };

    // The hostname travels as a positional argument ($1), never inside the
    // shell string, so no escaping/injection is possible. `sudo -k` forces
    // a fresh authentication so the entered password is always the one
    // used; `-p ''` suppresses sudo's own prompt text.
    let script = r#"echo "$1" > /proc/sys/kernel/hostname && printf '%s\n' "$1" > /etc/hostname"#;
    let mut child = Command::new(&sudo)
        .args(["-S", "-p", "", "-k", "--", "sh", "-c", script, "sh", name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| HostnameSetError::Other(format!("Could not run sudo: {e}")))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| HostnameSetError::Other("Could not reach sudo's input".into()))?;
        let _ = stdin.write_all(password.as_bytes());
        let _ = stdin.write_all(b"\n");
        // Closing stdin signals sudo that the password is complete.
    }

    let output = child
        .wait_with_output()
        .map_err(|e| HostnameSetError::Other(format!("sudo failed: {e}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let lower = stderr.to_lowercase();
    if lower.contains("incorrect password")
        || lower.contains("authentication failure")
        || lower.contains("no password was provided")
        || lower.contains("a password is required")
    {
        return Err(HostnameSetError::AuthFailed);
    }
    let detail = stderr.lines().last().unwrap_or("unknown sudo error");
    Err(HostnameSetError::Other(format!("sudo: {detail}")))
}
