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

use std::io::IsTerminal;
use std::process::{Command, Stdio};

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
///
/// A desktop launch has no terminal for sudo's prompt, so it falls back to
/// polkit (`pkexec`), which asks through the desktop's authentication agent.
/// Cached or passwordless sudo still runs silently first.
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

    // Cached or passwordless sudo needs no prompt at all.
    if run_sudo(&script, true).is_ok() {
        return verify();
    }
    if std::io::stdin().is_terminal() {
        return run_sudo(&script, false).and_then(|()| verify());
    }
    if pkexec_available() {
        return run_pkexec(&script).and_then(|()| verify());
    }
    // Last resort: sudo with SUDO_ASKPASS, if the user configured one.
    run_sudo(&script, false).map_err(|_| NO_TERMINAL.to_string())?;
    verify()
}

/// Shown when no terminal, no polkit agent and no askpass were available.
const NO_TERMINAL: &str = "sudo needs a terminal for its password — run Kanistr from a terminal, or install polkit (pkexec)";

fn run_sudo(script: &str, non_interactive: bool) -> Result<(), String> {
    let mut command = Command::new("sudo");
    if non_interactive {
        command.arg("-n").stderr(Stdio::null());
    } else {
        command.arg("-p").arg(SUDO_PROMPT);
    }
    let status = command
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .status()
        .map_err(|err| format!("could not run sudo: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("sudo was cancelled or failed".to_string())
    }
}

fn run_pkexec(script: &str) -> Result<(), String> {
    let status = Command::new("pkexec")
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .status()
        .map_err(|err| format!("could not run pkexec: {err}"))?;
    if status.success() {
        return Ok(());
    }
    Err(match status.code() {
        // pkexec: 126 = the authentication dialog was dismissed.
        Some(126) => "authentication was cancelled".to_string(),
        // 127 = not authorized, or no authentication agent could be reached.
        Some(127) => "polkit could not authenticate (is a polkit agent running?)".to_string(),
        _ => "authentication failed".to_string(),
    })
}

fn pkexec_available() -> bool {
    Command::new("pkexec")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn verify() -> Result<(), String> {
    // Success means the controls now read safe — not merely that the helper
    // exited 0.
    if is_fix_needed() {
        Err("the kernel controls still read unsafe".to_string())
    } else {
        Ok(())
    }
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
