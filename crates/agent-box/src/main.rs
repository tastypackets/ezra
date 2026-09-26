mod init;
mod manager;
mod path_ext;
mod process_ext;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Container entrypoint: switch to the agent user and run PROGRAM
    Init {
        program: OsString,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },
    /// Serve the manager API over HTTPS
    Manager,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_target(false)
        .init();

    match Cli::parse().command {
        Command::Init { program, arguments } => init::run(&program, &arguments),
        Command::Manager => manager::run(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_takes_the_program_and_its_arguments_after_a_double_dash() {
        let cli = Cli::try_parse_from(["agent-box", "init", "--", "bash", "-c", "exit 42"])
            .expect("valid command line");

        let Command::Init { program, arguments } = cli.command else {
            panic!("parsed as another command");
        };
        assert_eq!(program, "bash");
        assert_eq!(arguments, ["-c", "exit 42"]);
    }

    #[test]
    fn init_requires_a_program() {
        assert!(Cli::try_parse_from(["agent-box", "init"]).is_err());
    }
}
