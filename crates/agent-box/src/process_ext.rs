use std::io;
use std::process::{Command, Output, Stdio};

pub trait OutputExt {
    /// Standard output as text, invalid UTF-8 replaced.
    fn stdout_text(&self) -> String;

    /// Standard error as trimmed text, invalid UTF-8 replaced.
    fn stderr_text(&self) -> String;
}

impl OutputExt for Output {
    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_owned()
    }
}

pub trait CommandStatusExt {
    /// Runs with stdin closed and output inherited. A non-zero exit is an error.
    fn run_checked(&mut self) -> io::Result<()>;
}

impl CommandStatusExt for Command {
    fn run_checked(&mut self) -> io::Result<()> {
        let status = self.stdin(Stdio::null()).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{} {status}",
                self.get_program().to_string_lossy()
            )))
        }
    }
}
