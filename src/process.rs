use std::io;
use std::process::{Child, Output, Stdio};

pub trait CommandRunner: Send + Sync {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<Output>;
    fn spawn_piped(&self, program: &str, args: &[&str]) -> io::Result<Child>;
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
