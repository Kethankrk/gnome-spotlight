//! Opening a file with the desktop's default application.
//!
//! The daemon links no GLib (contract §15), so there is no `Gio.AppInfo` on this side of the bus:
//! the desktop's own launcher, `xdg-open`, is what hands a path to the application registered for
//! it. The child is started and left to itself — an application can run for hours, and the D-Bus
//! thread must not wait for it — and reaped by a thread of its own so the daemon collects no
//! zombies.
//!
//! `LUMEN_OPEN_BIN` names another program to run instead, and the integration harness points it at
//! `/bin/true`: a test suite must never launch the developer's applications (contract §19).

use std::process::{Child, Command, Stdio};

/// The environment variable naming the program that opens a uri.
pub const OPEN_BIN_ENV: &str = "LUMEN_OPEN_BIN";
/// The desktop launcher used when nothing overrides it.
pub const DEFAULT_OPEN_BIN: &str = "xdg-open";

/// The program that opens a uri, resolved once when the daemon starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opener {
    program: String,
}

impl Default for Opener {
    fn default() -> Self {
        Self::from_environment()
    }
}

impl Opener {
    /// The program the daemon's environment asks for, or [`DEFAULT_OPEN_BIN`].
    pub fn from_environment() -> Self {
        Self::with_program(program_from(
            std::env::var(OPEN_BIN_ENV).ok(),
        ))
    }

    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// The program this opener runs. Diagnostics.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Hand `uri` to the default application, reporting only whether the launcher could be
    /// started: what the application does with the path afterwards is not the daemon's business.
    pub fn open(&self, uri: &str) -> Result<(), String> {
        let child = Command::new(&self.program)
            .arg(uri)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;

        reap(child);
        Ok(())
    }
}

/// An empty or blank override is not a program: it is the default, so a stray `LUMEN_OPEN_BIN=`
/// cannot make every open fail.
pub fn program_from(configured: Option<String>) -> String {
    match configured {
        Some(program) if !program.trim().is_empty() => program,
        _ => DEFAULT_OPEN_BIN.to_string(),
    }
}

fn reap(mut child: Child) {
    let _ = std::thread::Builder::new()
        .name("lumen-opener-reaper".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_the_configured_one_or_the_desktop_launcher() {
        assert_eq!(program_from(None), "xdg-open");
        assert_eq!(program_from(Some(String::new())), "xdg-open");
        assert_eq!(program_from(Some("   ".to_string())), "xdg-open");
        assert_eq!(
            program_from(Some("/usr/bin/xdg-open".to_string())),
            "/usr/bin/xdg-open"
        );
    }

    #[test]
    fn opening_hands_the_uri_to_the_program() {
        let opener = Opener::with_program("/bin/true");
        assert_eq!(opener.program(), "/bin/true");
        assert!(
            opener.open("/home/me/report.pdf").is_ok(),
            "the launcher runs, so the daemon performed the action"
        );
    }

    #[test]
    fn a_program_that_cannot_be_started_is_reported_rather_than_silently_dropped() {
        let opener = Opener::with_program("/nonexistent/lumen-default-application");
        let reason = opener.open("/home/me/report.pdf").expect_err("it cannot run");
        assert!(!reason.is_empty(), "and says why: {reason}");
    }
}
