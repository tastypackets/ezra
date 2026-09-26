use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use super::environment::EnvironmentOverride;

const COMMAND_NOT_FOUND: u8 = 127;
const COMMAND_NOT_EXECUTABLE: u8 = 126;

/// The program init hands the container over to.
pub struct Program<'a> {
    pub name: &'a OsStr,
    pub arguments: &'a [OsString],
}

impl Program<'_> {
    /// Only returns if the program could not be executed.
    pub fn replace_current_process(
        &self,
        environment_overrides: &[EnvironmentOverride],
    ) -> ExitCode {
        let mut command = Command::new(self.name);
        command.args(self.arguments);
        for variable in environment_overrides {
            command.env(variable.name, &variable.value);
        }
        let error = command.exec();
        tracing::error!("could not run {}: {error}", self.name.to_string_lossy());
        ExitCode::from(error.shell_exit_code())
    }
}

trait ExecErrorExt {
    /// The code a shell exits with when it cannot run a program: 127 when missing, 126 otherwise.
    fn shell_exit_code(&self) -> u8;
}

impl ExecErrorExt for io::Error {
    fn shell_exit_code(&self) -> u8 {
        match self.kind() {
            io::ErrorKind::NotFound => COMMAND_NOT_FOUND,
            _ => COMMAND_NOT_EXECUTABLE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_program_exits_like_a_shell() {
        assert_eq!(
            io::Error::from(io::ErrorKind::NotFound).shell_exit_code(),
            127
        );
    }

    #[test]
    fn unexecutable_program_exits_like_a_shell() {
        assert_eq!(
            io::Error::from(io::ErrorKind::PermissionDenied).shell_exit_code(),
            126
        );
        assert_eq!(
            io::Error::from_raw_os_error(nix::libc::ENOEXEC).shell_exit_code(),
            126
        );
    }
}
