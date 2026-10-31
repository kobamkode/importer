//! USB runtime power management.
//!
//! Linux parks idle USB devices by default, which can drop a card reader off
//! the bus in the middle of an Import. The app offers the documented, temporary
//! root-only workaround behind a Settings toggle: check the unprivileged
//! kernel controls, and if the machine is not already safe, apply the fix once
//! before the first file.
//!
//! The decision is a pure function over the control values ([`fix_needed`]) so
//! it is testable without root, sudo or a real sysfs.

use std::process::Command;

/// Where the kernel-wide autosuspend timeout lives. `-1` disables it.
const AUTOSUSPEND: &str = "/sys/module/usbcore/parameters/autosuspend";
/// One directory per attached USB device; each has `power/control`.
const DEVICES: &str = "/sys/bus/usb/devices";
const SAFE_AUTOSUSPEND: &str = "-1";
const SAFE_CONTROL: &str = "on";
/// sudo's prompt, naming the app so the pause is recognisable. `%u` is sudo's
/// own placeholder for the invoking user.
const SUDO_PROMPT: &str = "[kanistr] password for %u: ";

/// Is the fix needed? `None` means the autosuspend parameter can't be read
/// (non-Linux), and the feature is inert. Otherwise it is needed when the
/// parameter is anything but `-1`, or any device still allows autosuspend.
fn fix_needed(autosuspend: Option<&str>, device_controls: &[String]) -> bool {
    let Some(autosuspend) = autosuspend else {
        return false;
    };
    autosuspend.trim() != SAFE_AUTOSUSPEND
        || device_controls
            .iter()
            .any(|control| control.trim().eq_ignore_ascii_case("auto"))
}

/// Read the live controls and apply [`fix_needed`]. Unreadable files count as
/// absent, which keeps a non-Linux machine on the "nothing to do" path.
/// A few sysfs reads with no sudo, cheap enough to call on the UI thread
/// before an Import so the privileged step is skipped when it isn't needed.
pub fn is_fix_needed() -> bool {
    let autosuspend = std::fs::read_to_string(AUTOSUSPEND).ok();
    fix_needed(autosuspend.as_deref(), &device_controls())
}

fn device_controls() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(DEVICES) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("power/control")).ok())
        .collect()
}

/// Make sure the kernel can't park a USB device: the documented two-line
/// workaround, as one privileged shell. No-op when the machine is already
/// safe, so a second Import in the same boot never asks for a password.
///
/// sudo prompts on the controlling terminal itself (hidden input, its usual
/// credential cache); the password never passes through this process. The
/// child inherits stderr so sudo's own diagnostics ("Sorry, try again.") reach
/// the terminal; the header only carries a generic reason, and the io error
/// text when sudo cannot even be spawned.
pub fn apply() -> Result<(), String> {
    if !is_fix_needed() {
        return Ok(());
    }
    // Per-device writes tolerate failure: a device can vanish mid-loop, and
    // one stubborn control must not block the rest. The parameter write is
    // the one that must succeed.
    let script = format!(
        "echo {SAFE_AUTOSUSPEND} > {AUTOSUSPEND} && \
         for c in {DEVICES}/*/power/control; do echo {SAFE_CONTROL} > \"$c\" 2>/dev/null || true; done"
    );
    let status = Command::new("sudo")
        .arg("-p")
        .arg(SUDO_PROMPT)
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .status()
        .map_err(|err| format!("could not run sudo: {err}"))?;
    if !status.success() {
        return Err("sudo was cancelled or failed".to_string());
    }
    // Success means the controls now read safe — not merely that sudo exited 0.
    if is_fix_needed() {
        return Err("the kernel controls still read unsafe".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controls(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn already_safe_state_needs_no_fix() {
        // sysfs files end in a newline; that must not look unsafe.
        assert!(!fix_needed(Some("-1\n"), &controls(&["on", "on"])));
    }

    #[test]
    fn enabled_autosuspend_needs_a_fix() {
        assert!(fix_needed(Some("2\n"), &controls(&["on"])));
    }

    #[test]
    fn device_left_on_auto_needs_a_fix() {
        assert!(fix_needed(Some("-1\n"), &controls(&["on", "auto"])));
    }

    #[test]
    fn missing_sysfs_needs_no_fix() {
        assert!(!fix_needed(None, &controls(&["auto"])));
    }
}
