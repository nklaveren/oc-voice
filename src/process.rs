use std::io;
use std::process::{Child, Output, Stdio};

pub trait CommandRunner: Send + Sync {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<Output>;
    fn spawn_piped(&self, program: &str, args: &[&str]) -> io::Result<Child>;

    /// Whether effects must be described rather than carried out.
    ///
    /// Everything reaching the world through `output` is already guarded, but
    /// not everything goes through it: the page module speaks WebSocket
    /// directly, and a socket is not a subprocess. Without this the probe
    /// would click things while explaining what it would click — the same
    /// trap `curl .../json/activate` fell into.
    fn dry_run(&self) -> bool {
        false
    }
}

/// Reads through to the real system, refuses anything that would change it.
///
/// The probe needs live windows and monitors to resolve targets honestly,
/// but must never actually focus a monitor or switch a workspace while
/// someone is exploring what an utterance would do. Reads (`-j` queries)
/// pass through; `dispatch` and text injection are swallowed and recorded.
pub struct DryRunRunner {
    inner: SystemRunner,
    blocked: std::sync::Mutex<Vec<(String, Vec<String>)>>,
}

impl Default for DryRunRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl DryRunRunner {
    pub fn new() -> Self {
        DryRunRunner {
            inner: SystemRunner,
            blocked: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Everything this runner refused to execute, in order.
    pub fn blocked(&self) -> Vec<(String, Vec<String>)> {
        self.blocked.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn is_mutating(program: &str, args: &[&str]) -> bool {
        // Anything that types, and any hyprctl call that is not a query.
        matches!(program, "wtype" | "xdotool" | "osascript")
            || (program == "hyprctl" && !args.contains(&"-j"))
            // A browser tab is state too. `curl .../json/activate/<id>` is a
            // plain GET, which reads like a query and is not one: it switches
            // the tab in front of the user. The probe changed a window title
            // this way and then measured the world it had just altered,
            // reporting a resolution that the tab path had not performed.
            // `/json/list` stays allowed — it is the read this is built on.
            || (program == "curl" && args.iter().any(|a| a.contains("/json/activate/")))
    }
}

impl CommandRunner for DryRunRunner {
    fn dry_run(&self) -> bool {
        true
    }

    fn output(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        if Self::is_mutating(program, args) {
            if let Ok(mut g) = self.blocked.lock() {
                g.push((
                    program.to_string(),
                    args.iter().map(|s| s.to_string()).collect(),
                ));
            }
            return Ok(Output {
                status: Default::default(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        }
        self.inner.output(program, args)
    }

    fn spawn_piped(&self, program: &str, args: &[&str]) -> io::Result<Child> {
        self.inner.spawn_piped(program, args)
    }
}

pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        std::process::Command::new(program).args(args).output()
    }

    fn spawn_piped(&self, program: &str, args: &[&str]) -> io::Result<Child> {
        std::process::Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    }
}

#[cfg(test)]
pub struct FakeRunner {
    pub calls: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    pub stdout: Vec<u8>,
}

#[cfg(test)]
impl FakeRunner {
    pub fn new(stdout: Vec<u8>) -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            stdout,
        }
    }

    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl CommandRunner for FakeRunner {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<Output> {
        self.calls.lock().unwrap().push((
            program.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        ));
        use std::os::unix::process::ExitStatusExt;
        Ok(Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: self.stdout.clone(),
            stderr: Vec::new(),
        })
    }

    fn spawn_piped(&self, program: &str, args: &[&str]) -> io::Result<Child> {
        self.calls.lock().unwrap().push((
            program.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        ));
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "FakeRunner does not spawn long-running processes",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_blocks_everything_that_changes_state() {
        let dry = DryRunRunner::new();
        // These must never reach the system from a diagnostic tool.
        for (prog, args) in [
            ("hyprctl", vec!["dispatch", "workspace", "4"]),
            ("hyprctl", vec!["dispatch", "killactive"]),
            ("hyprctl", vec!["dispatch", "focusmonitor", "DP-1"]),
            ("wtype", vec!["texto qualquer"]),
            ("xdotool", vec!["type", "texto"]),
            (
                "osascript",
                vec!["-e", "tell application \"System Events\""],
            ),
            // A GET that is not a query: it switches the tab in front of the
            // user. The probe used to run this for real, change a window
            // title, and then measure the world it had just altered.
            ("curl", vec!["-s", "http://127.0.0.1:9222/json/activate/D4"]),
        ] {
            assert!(
                DryRunRunner::is_mutating(prog, &args),
                "{prog} {args:?} deveria ser bloqueado"
            );
            let out = dry.output(prog, &args).unwrap();
            assert!(out.stdout.is_empty());
        }
        assert_eq!(dry.blocked().len(), 7, "todas as chamadas registradas");
    }

    #[test]
    fn dry_run_lets_queries_through() {
        // Resolution has to see the real session or the probe lies.
        for (prog, args) in [
            ("hyprctl", vec!["clients", "-j"]),
            ("hyprctl", vec!["monitors", "-j"]),
            // Listing tabs is the read the whole tab path is built on, and
            // blocking it would make the probe report "no tab matched" for
            // every tab there is.
            ("curl", vec!["-s", "http://127.0.0.1:9222/json/list"]),
        ] {
            assert!(!DryRunRunner::is_mutating(prog, &args));
        }
    }
}
