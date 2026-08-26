use std::ffi::CStr;
use std::io::Write;
use crate::{AppResult, err};

pub fn get_hostname() -> AppResult<String> {
    let mut buf = [0u8; 256];
    let res = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if res != 0 {
        return Err(err("gethostname(2) failed"));
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr() as *const libc::c_char) };
    Ok(cstr.to_string_lossy().into_owned())
}

pub fn set_hostname(name: &str) -> AppResult<()> {
    if name.is_empty() {
        return Err(err("Hostname cannot be empty"));
    }
    if name.len() > 63 {
        return Err(err("Hostname too long (max 63 bytes)"));
    }
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '-' && ch != '.' {
            return Err(err(format!(
                "Invalid character {ch:?} in hostname (only a-z A-Z 0-9 - . allowed)"
            )));
        }
    }
    
    let res = unsafe { libc::sethostname(name.as_ptr() as *const libc::c_char, name.len()) };
    if res != 0 {
        return Err(err(format!("sethostname({name:?}) failed (are you root?)")));
    }

    // Persist to /etc/hostname
    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).truncate(true).open("/etc/hostname") {
        let _ = file.write_all(name.as_bytes());
        let _ = file.write_all(b"\n");
    }

    Ok(())
}
