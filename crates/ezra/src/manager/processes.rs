use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use nix::sys::resource::{Resource, getrlimit, rlim_t, setrlimit};
use nix::unistd::Pid;

const DELETED_SUFFIX: &str = " (deleted)";

/// A process as /proc describes it.
pub struct Process(PathBuf);

pub struct ProcessStat {
    pub parent: Pid,
    /// Exited and waiting for its parent to collect it.
    pub zombie: bool,
}

impl Process {
    pub fn all() -> impl Iterator<Item = Self> {
        fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.parse::<u32>().is_ok())
            })
            .map(|entry| Self(entry.path()))
    }

    pub fn id(&self) -> Option<Pid> {
        let id = self.0.file_name()?.to_str()?.parse().ok()?;
        Some(Pid::from_raw(id))
    }

    /// `leader` and the processes it started, also those in process groups of their own.
    pub fn family_of(leader: Pid) -> Vec<(Self, ProcessStat)> {
        let mut children: HashMap<Pid, Vec<(Self, ProcessStat)>> = HashMap::new();
        let mut family = Vec::new();
        for process in Self::all() {
            let (Some(id), Some(stat)) = (process.id(), process.stat()) else {
                continue;
            };
            if id == leader {
                family.push((process, stat));
            } else {
                children
                    .entry(stat.parent)
                    .or_default()
                    .push((process, stat));
            }
        }
        let mut next = 0;
        while let Some((process, _)) = family.get(next) {
            if let Some(started) = process.id().and_then(|id| children.remove(&id)) {
                family.extend(started);
            }
            next = next.saturating_add(1);
        }
        family
    }

    /// The program and its arguments.
    pub fn arguments(&self) -> Vec<String> {
        fs::read(self.0.join("cmdline"))
            .unwrap_or_default()
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| String::from_utf8_lossy(argument).into_owned())
            .collect()
    }

    pub fn stat(&self) -> Option<ProcessStat> {
        let stat = fs::read_to_string(self.0.join("stat")).ok()?;
        let (_, after_name) = stat.rsplit_once(')')?;
        let mut fields = after_name.split_whitespace();
        let zombie = fields.next()? == "Z";
        let parent = fields.next()?.parse().ok()?;
        Some(ProcessStat {
            parent: Pid::from_raw(parent),
            zombie,
        })
    }

    /// Resident memory with each shared page split between the processes sharing it.
    pub fn proportional_memory(&self) -> Option<u64> {
        let rollup = fs::read_to_string(self.0.join("smaps_rollup")).ok()?;
        let kilobytes: u64 = rollup
            .lines()
            .find_map(|line| line.strip_prefix("Pss:"))?
            .trim()
            .strip_suffix("kB")?
            .trim()
            .parse()
            .ok()?;
        kilobytes.checked_mul(1024)
    }

    /// The file the process runs, also after that file was deleted or replaced.
    pub fn executable(&self) -> Option<PathBuf> {
        let target = fs::read_link(self.0.join("exe")).ok()?;
        let target = target.to_str()?;
        Some(PathBuf::from(
            target.strip_suffix(DELETED_SUFFIX).unwrap_or(target),
        ))
    }

    /// Names of the entries in `directory` that a running process executes from.
    pub fn running_from(directory: &Path) -> Vec<String> {
        let directory = fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
        Self::all()
            .filter_map(|process| process.executable())
            .filter_map(|executable| {
                let inside = executable.strip_prefix(&directory).ok()?;
                let first = inside.components().next()?;
                Some(first.as_os_str().to_string_lossy().into_owned())
            })
            .collect()
    }
}

/// How many files a process can have open, which the processes it starts inherit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFileLimit {
    pub soft: rlim_t,
    pub hard: rlim_t,
}

impl OpenFileLimit {
    const HIGHEST_TARGET: rlim_t = 1_048_576;

    /// This process's limit.
    pub fn current() -> nix::Result<Self> {
        let (soft, hard) = getrlimit(Resource::RLIMIT_NOFILE)?;
        Ok(Self { soft, hard })
    }

    /// The soft limit to raise to under `hard`.
    pub fn target(hard: rlim_t) -> rlim_t {
        hard.min(Self::HIGHEST_TARGET)
    }

    /// Raises this process's soft limit to the target unless it is higher already.
    pub fn raise(self) -> nix::Result<Self> {
        let raised = Self {
            soft: self.soft.max(Self::target(self.hard)),
            ..self
        };
        if raised != self {
            setrlimit(Resource::RLIMIT_NOFILE, raised.soft, raised.hard)?;
        }
        Ok(raised)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    use nix::errno::Errno;
    use nix::sys::resource::RLIM_INFINITY;

    use super::*;

    #[test]
    fn a_copied_program_counts_as_running_from_its_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let sleep = fs::canonicalize("/bin/sleep").expect("sleep is installed");
        let copy = directory.path().join("2.1.1");
        fs::copy(&sleep, &copy).expect("sleep is copied");
        let mut running = Command::new(&copy)
            .arg0("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .expect("the copy starts");
        fs::remove_file(&copy).expect("the copy is deleted while it runs");

        assert_eq!(Process::running_from(directory.path()), ["2.1.1"]);

        running.kill().expect("the copy stops");
        running.wait().expect("the copy is reaped");
        assert!(Process::running_from(directory.path()).is_empty());
    }

    #[test]
    fn the_open_file_target_is_the_hard_limit_up_to_a_million() {
        assert_eq!(OpenFileLimit::target(524_288), 524_288);
        assert_eq!(OpenFileLimit::target(1_073_741_816), 1_048_576);
        assert_eq!(OpenFileLimit::target(RLIM_INFINITY), 1_048_576);
    }

    #[test]
    fn raising_the_open_file_limit_sets_the_soft_limit_to_the_target() {
        let OpenFileLimit { soft, hard } = OpenFileLimit::current().expect("the limit is readable");
        setrlimit(Resource::RLIMIT_NOFILE, soft.min(1024), hard)
            .expect("the soft limit is lowered");

        let raised = OpenFileLimit::current()
            .and_then(OpenFileLimit::raise)
            .expect("the soft limit is raised");

        assert_eq!(
            raised,
            OpenFileLimit {
                soft: OpenFileLimit::target(hard),
                hard
            }
        );
        assert_eq!(OpenFileLimit::current(), Ok(raised));
    }

    #[test]
    fn a_soft_limit_above_the_target_is_kept() {
        let limit = OpenFileLimit {
            soft: 2_097_152,
            hard: RLIM_INFINITY,
        };
        assert_eq!(limit.raise(), Ok(limit));
    }

    #[test]
    fn a_limit_the_kernel_refuses_is_an_error() {
        let limit = OpenFileLimit {
            soft: 0,
            hard: RLIM_INFINITY,
        };
        assert_eq!(limit.raise(), Err(Errno::EPERM));
    }
}
