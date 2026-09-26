use std::fs;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nix::unistd::{Uid, fchown};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioTarget {
    Pipe,
    Terminal,
    Other,
}

pub fn classify(link_target: &Path) -> StdioTarget {
    let target = link_target.as_os_str().as_bytes();
    if target.starts_with(b"pipe:") {
        StdioTarget::Pipe
    } else if target.starts_with(b"/dev/pts/") {
        StdioTarget::Terminal
    } else {
        StdioTarget::Other
    }
}

/// Docker creates the container's stdio pipes and tty owned by root (moby#31243);
/// without this the agent cannot reopen them, e.g. `$(tty)` or `> /dev/stderr`.
pub fn hand_over_to(owner: Uid) {
    let (stdin, stdout, stderr) = (io::stdin(), io::stdout(), io::stderr());
    for (descriptor_number, descriptor) in
        [(0, stdin.as_fd()), (1, stdout.as_fd()), (2, stderr.as_fd())]
    {
        let Ok(link_target) = fs::read_link(format!("/proc/self/fd/{descriptor_number}")) else {
            continue;
        };
        if classify(&link_target) == StdioTarget::Other {
            continue;
        }
        if let Err(error) = fchown(descriptor, Some(owner), None) {
            tracing::warn!("could not hand fd {descriptor_number} to uid {owner}: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipes_and_terminals_are_handed_over() {
        assert_eq!(classify(Path::new("pipe:[123456]")), StdioTarget::Pipe);
        assert_eq!(classify(Path::new("/dev/pts/0")), StdioTarget::Terminal);
    }

    #[test]
    fn everything_else_is_left_alone() {
        assert_eq!(classify(Path::new("/dev/null")), StdioTarget::Other);
        assert_eq!(classify(Path::new("socket:[42]")), StdioTarget::Other);
        assert_eq!(
            classify(Path::new("/var/log/agent.log")),
            StdioTarget::Other
        );
    }
}
