//! `lumen-daemon`: own `org.gnome.Lumen`, serve `org.gnome.Lumen.Engine`, exit cleanly on SIGTERM.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicI32, Ordering};

use lumen_daemon::store::ClipboardMode;
use lumen_daemon::{dbus, Indexer};

const USAGE: &str = "\
Usage: lumen-daemon [OPTIONS]

Options:
      --dbus-name <NAME>    well-known session bus name to own [default: org.gnome.Lumen]
      --object-path <PATH>  object path to export the engine at [default: /org/gnome/Lumen]
      --indexer <SPEC>      none | fake[:<seed-file>] | localsearch  [default: none]
      --clipboard <SPEC>    persist | memory  [default: persist]
                            `persist` keeps the history encrypted under
                            $XDG_DATA_HOME/lumen/clipboard.bin, with the key in the login keyring;
                            it falls back to memory, with a reason, when either is unavailable.
  -h, --help                print this help
  -V, --version             print the version

Environment:
  DBUS_SESSION_BUS_ADDRESS  the bus to serve on
  GSETTINGS_SCHEMA_DIR      directory holding the compiled Lumen schema; the web-search row's
                            `search-engine` key is read from it (no schema means DuckDuckGo)
  LUMEN_FAKE_INDEX          seed file for `--indexer fake` when the flag is not given
  LUMEN_TINYSPARQL_LIB      library path for `--indexer localsearch` (testing seam)
  LUMEN_SECRET_KEY_FILE     read the clipboard encryption key from this file instead of the login
                            keyring (testing seam)

The installer passes `--indexer localsearch` so the daemon searches the system's file index;
without it the daemon serves every other provider and reports no `files` capability.
";

/// The test harness and the packaging unit both need to know when the name is ours; the readiness
/// line on stdout is that signal.
const READY_MARKER: &str = "ready";

enum Outcome {
    Run(Box<dbus::Config>),
    Help,
    Version,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match parse_args(&args) {
        Ok(Outcome::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Outcome::Version) => {
            println!("lumen-daemon {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Outcome::Run(config)) => run(*config),
        Err(message) => {
            eprintln!("lumen-daemon: {message}");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run(config: dbus::Config) -> ExitCode {
    let name = config.dbus_name.clone();
    let path = config.object_path.clone();

    let shutdown = match install_terminate_handler() {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("lumen-daemon: cannot install the SIGTERM handler: {e}");
            return ExitCode::FAILURE;
        }
    };

    let serving = match zbus::block_on(dbus::serve(config)) {
        Ok(serving) => serving,
        Err(e) => {
            eprintln!("lumen-daemon: cannot serve {name} at {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("lumen-daemon {READY_MARKER}: name={name} path={path}");
    let _ = io::stdout().flush();

    // The other readiness protocol: the shipped user unit is `Type=notify` (packaging/), so tell
    // systemd the same thing. A session without systemd has no `NOTIFY_SOCKET`, and a failure to
    // send is logged rather than fatal — serving on the bus is what actually matters.
    if let Err(e) = lumen_daemon::notify::ready() {
        eprintln!("lumen-daemon: could not notify systemd of readiness: {e}");
    }

    wait_for_terminate(shutdown);

    let _ = zbus::block_on(serving.connection.release_name(serving.name.as_str()));
    drop(serving);

    ExitCode::SUCCESS
}

fn parse_args(args: &[String]) -> Result<Outcome, String> {
    let mut config = dbus::Config::default();
    let mut saw_indexer = false;
    let mut rest = args.iter();

    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_string())),
            None => (arg.as_str(), None),
        };

        let take_value = |rest: &mut std::slice::Iter<'_, String>| match inline.clone() {
            Some(value) => Ok(value),
            None => rest
                .next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value")),
        };

        match flag {
            "-h" | "--help" => return Ok(Outcome::Help),
            "-V" | "--version" => return Ok(Outcome::Version),
            "--dbus-name" => config.dbus_name = take_value(&mut rest)?,
            "--object-path" => config.object_path = take_value(&mut rest)?,
            "--indexer" => {
                config.indexer = parse_indexer(&take_value(&mut rest)?)?;
                saw_indexer = true;
            }
            "--clipboard" => config.clipboard = parse_clipboard(&take_value(&mut rest)?)?,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }

    if !saw_indexer {
        if let Ok(seed) = std::env::var("LUMEN_FAKE_INDEX") {
            config.indexer = Indexer::Fake {
                seed: Some(PathBuf::from(seed)),
            };
        }
    }

    if let Indexer::Fake { seed: Some(seed) } = &config.indexer {
        if !seed.is_file() {
            return Err(format!(
                "the fake index seed {} is not a readable file",
                seed.display()
            ));
        }
    }

    if !config.object_path.starts_with('/') {
        return Err(format!("`{}` is not an object path", config.object_path));
    }

    Ok(Outcome::Run(Box::new(config)))
}

fn parse_indexer(spec: &str) -> Result<Indexer, String> {
    match spec.split_once(':') {
        None if spec == "none" => Ok(Indexer::None),
        None if spec == "fake" => Ok(Indexer::Fake { seed: None }),
        None if spec == "localsearch" || spec == "tinysparql" => Ok(Indexer::LocalSearch),
        None => Err(format!("unknown indexer `{spec}`")),
        Some((kind, seed)) if kind == "fake" => Ok(Indexer::Fake {
            seed: Some(PathBuf::from(seed)),
        }),
        Some((kind, _)) => Err(format!("unknown indexer `{kind}`")),
    }
}

/// `persist` (the default) writes the encrypted history to the XDG data directory; `memory` never
/// touches the disk, which is what the test harness and a locked-down session ask for.
fn parse_clipboard(spec: &str) -> Result<ClipboardMode, String> {
    match spec {
        "persist" | "persistent" | "disk" => Ok(ClipboardMode::Persistent),
        "memory" | "none" => Ok(ClipboardMode::Memory),
        other => Err(format!("unknown clipboard mode `{other}`")),
    }
}

static TERMINATE_FD: AtomicI32 = AtomicI32::new(-1);

fn install_terminate_handler() -> io::Result<libc::c_int> {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    TERMINATE_FD.store(fds[1], Ordering::SeqCst);

    // SA_RESTART keeps the runtime's own reads and writes whole; the byte the handler writes is
    // what wakes the blocking read below.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_terminate as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) != 0
            || libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(fds[0])
}

extern "C" fn on_terminate(_signal: libc::c_int) {
    let fd = TERMINATE_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte = 1u8;
        unsafe { libc::write(fd, &byte as *const u8 as *const libc::c_void, 1) };
    }
}

fn wait_for_terminate(fd: libc::c_int) {
    let mut byte = 1u8;
    loop {
        let read = unsafe { libc::read(fd, &mut byte as *mut u8 as *mut libc::c_void, 1) };
        if read >= 0 {
            return;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    fn run_config(list: &[&str]) -> dbus::Config {
        match parse_args(&args(list)).expect("arguments parse") {
            Outcome::Run(config) => *config,
            _ => panic!("expected a runnable configuration"),
        }
    }

    #[test]
    fn no_arguments_means_the_contract_defaults() {
        let config = run_config(&[]);
        assert_eq!(config.dbus_name, "org.gnome.Lumen");
        assert_eq!(config.object_path, "/org/gnome/Lumen");
        assert_eq!(config.indexer, Indexer::None);
        assert_eq!(config.clipboard, ClipboardMode::Persistent);
    }

    #[test]
    fn the_clipboard_spec_selects_the_store() {
        assert_eq!(
            run_config(&["--clipboard", "memory"]).clipboard,
            ClipboardMode::Memory
        );
        assert_eq!(
            run_config(&["--clipboard=persist"]).clipboard,
            ClipboardMode::Persistent
        );
        assert!(parse_args(&args(&["--clipboard", "somewhere-else"])).is_err());
        assert!(parse_args(&args(&["--clipboard"])).is_err());
    }

    #[test]
    fn a_test_harness_can_pick_its_own_name_and_path() {
        let config = run_config(&[
            "--dbus-name",
            "org.gnome.Lumen.Test",
            "--object-path=/tmp/lumen",
        ]);
        assert_eq!(config.dbus_name, "org.gnome.Lumen.Test");
        assert_eq!(config.object_path, "/tmp/lumen");
    }

    #[test]
    fn the_indexer_spec_selects_the_seam() {
        let seed = std::env::temp_dir().join(format!("lumen-main-seed-{}.json", std::process::id()));
        std::fs::write(&seed, r#"{"files": []}"#).expect("write a seed");

        assert_eq!(run_config(&["--indexer", "none"]).indexer, Indexer::None);
        assert_eq!(
            run_config(&["--indexer=fake"]).indexer,
            Indexer::Fake { seed: None }
        );
        assert_eq!(
            run_config(&["--indexer", &format!("fake:{}", seed.display())]).indexer,
            Indexer::Fake {
                seed: Some(seed.clone())
            }
        );
        assert_eq!(
            run_config(&["--indexer", "localsearch"]).indexer,
            Indexer::LocalSearch,
            "the installer selects the real index"
        );
        assert_eq!(
            run_config(&["--indexer", "tinysparql"]).indexer,
            Indexer::LocalSearch
        );

        assert!(parse_args(&args(&["--indexer", "woof"])).is_err());
        assert!(
            parse_args(&args(&["--indexer", "fake:/nonexistent/seed.json"])).is_err(),
            "a seed that cannot be read must fail at startup, not silently search nothing"
        );

        std::fs::remove_file(&seed).ok();
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(matches!(parse_args(&args(&["--help"])), Ok(Outcome::Help)));
        assert!(matches!(parse_args(&args(&["-V"])), Ok(Outcome::Version)));
    }

    #[test]
    fn bad_arguments_are_reported() {
        assert!(parse_args(&args(&["--nonsense"])).is_err());
        assert!(parse_args(&args(&["--object-path"])).is_err());
        assert!(parse_args(&args(&["--object-path", "not/absolute"])).is_err());
    }
}
