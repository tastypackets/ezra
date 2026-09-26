use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use super::environment::EnvironmentOverride;

const COMMAND_NOT_FOUND: u8 = 127;
const COMMAND_NOT_EXECUTABLE: u8 = 126;

/// Only returns if the program could not be executed.
pub fn replace_process(
    program: &OsStr,
    arguments: &[OsString],
    environment_overrides: &[EnvironmentOverride],
) -> ExitCode {
    let mut command = Command::new(program);
    command.args(arguments);
    for variable in environment_overrides {
        command.env(variable.name, &variable.value);
    }

    let error = command.exec();
    tracing::error!("could not run {}: {error}", program.to_string_lossy());
    ExitCode::from(exit_code_for(&error))
}

pub fn exit_code_for(error: &io::Error) -> u8 {
    match error.kind() {
        io::ErrorKind::NotFound => COMMAND_NOT_FOUND,
        _ => COMMAND_NOT_EXECUTABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_program_exits_like_a_shell() {
        assert_eq!(
            exit_code_for(&io::Error::from(io::ErrorKind::NotFound)),
            127
        );
    }

    #[test]
    fn unexecutable_program_exits_like_a_shell() {
        assert_eq!(
            exit_code_for(&io::Error::from(io::ErrorKind::PermissionDenied)),
            126
        );
        assert_eq!(
            exit_code_for(&io::Error::from_raw_os_error(nix::libc::ENOEXEC)),
            126
        );
    }
}
