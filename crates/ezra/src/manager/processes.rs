use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nix::sys::resource::{Resource, getrlimit, rlim_t, setrlimit};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::time::{Instant, sleep};

const DELETED_SUFFIX: &str = " (deleted)";
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A process as /proc describes it.
pub struct Process(PathBuf);

pub struct ProcessStat {
    pub parent: Pid,
    /// Exited and waiting for its parent to collect it.
    pub zombie: bool,
    /// When it started, in clock ticks after boot.
    pub started: u64,
}

impl Process {
    pub fn with_id(id: Pid) -> Self {
        Self(PathBuf::from(format!("/proc/{id}")))
    }

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
        let started = fields.nth(17)?.parse().ok()?;
        Some(ProcessStat {
            parent: Pid::from_raw(parent),
            zombie,
            started,
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

    /// The running processes whose executable lies inside `directory`, each with the
    /// executable's path in there.
    pub fn running_inside(directory: &Path) -> impl Iterator<Item = (Self, PathBuf)> + use<> {
        let directory = fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
        Self::all().filter_map(move |process| {
            let inside = process
                .executable()?
                .strip_prefix(&directory)
                .ok()?
                .to_path_buf();
            Some((process, inside))
        })
    }

    /// Names of the entries in `directory` that a running process executes from.
    pub fn running_from(directory: &Path) -> Vec<String> {
        Self::running_inside(directory)
            .filter_map(|(_, inside)| {
                let first = inside.components().next()?;
                Some(first.as_os_str().to_string_lossy().into_owned())
            })
            .collect()
    }
}

#[cfg(test)]
impl Process {
    /// Exists and has not exited.
    pub fn is_running(&self) -> bool {
        self.stat().is_some_and(|stat| !stat.zombie)
    }
}

/// A process and the processes it started, also those in process groups of their own, as they
/// were when read.
pub struct ProcessFamily {
    leader: Pid,
    members: Vec<(Process, ProcessStat)>,
}

impl ProcessFamily {
    /// Reads /proc, so it blocks.
    pub fn of(leader: Pid) -> Self {
        let mut children: HashMap<Pid, Vec<(Process, ProcessStat)>> = HashMap::new();
        let mut members = Vec::new();
        for process in Process::all() {
            let (Some(id), Some(stat)) = (process.id(), process.stat()) else {
                continue;
            };
            if id == leader {
                members.push((process, stat));
            } else {
                children
                    .entry(stat.parent)
                    .or_default()
                    .push((process, stat));
            }
        }
        let mut next = 0;
        while let Some((process, _)) = members.get(next) {
            if let Some(started) = process.id().and_then(|id| children.remove(&id)) {
                members.extend(started);
            }
            next = next.saturating_add(1);
        }
        Self { leader, members }
    }

    /// The leader as read, unless it had exited.
    pub fn leader(&self) -> Option<ProcessIdentity> {
        self.identities()
            .next()
            .filter(|first| first.id == self.leader)
    }

    /// The leader's children that have not exited.
    pub fn children(&self) -> impl Iterator<Item = &Process> {
        self.members
            .iter()
            .filter(|(_, stat)| stat.parent == self.leader && !stat.zombie)
            .map(|(process, _)| process)
    }

    /// Memory the members still running use, with shared memory counted once. Blocks.
    pub fn memory_bytes(&self) -> u64 {
        self.identities()
            .filter(|member| member.is_running())
            .filter_map(|member| Process::with_id(member.id).proportional_memory())
            .fold(0, u64::saturating_add)
    }

    /// Kills the members still running with the start time they had when read, never this
    /// process. Blocks.
    pub fn kill_survivors(&self) {
        for member in self.identities() {
            member.send(Signal::SIGKILL);
        }
    }

    /// The members that had not exited when read.
    fn identities(&self) -> impl Iterator<Item = ProcessIdentity> {
        self.members
            .iter()
            .filter(|(_, stat)| !stat.zombie)
            .filter_map(|(process, stat)| {
                Some(ProcessIdentity {
                    id: process.id()?,
                    started: stat.started,
                })
            })
    }
}

/// A process known by its pid and start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    id: Pid,
    /// When it started, in clock ticks after boot.
    started: u64,
}

impl ProcessIdentity {
    /// Has not exited. Blocks.
    fn is_running(self) -> bool {
        Process::with_id(self.id)
            .stat()
            .is_some_and(|stat| !stat.zombie && stat.started == self.started)
    }

    /// Sends `signal` unless it exited or is this process. Blocks.
    fn send(self, signal: Signal) {
        if self.id != Pid::this() && self.is_running() {
            let _already_gone = kill(self.id, signal);
        }
    }

    /// Sends `signal` unless it exited or is this process.
    pub async fn signal(self, signal: Signal) {
        let _sent = tokio::task::spawn_blocking(move || self.send(signal)).await;
    }

    /// Reads /proc until it exited, for at most `longest`. True once it has.
    pub async fn exits_within(self, longest: Duration) -> bool {
        let started = Instant::now();
        loop {
            let running = tokio::task::spawn_blocking(move || self.is_running()).await;
            if matches!(running, Ok(false)) {
                return true;
            }
            if started.elapsed() >= longest {
                return false;
            }
            sleep(EXIT_POLL_INTERVAL).await;
        }
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
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{self, Command, Stdio};
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use nix::errno::Errno;
    use nix::sys::resource::RLIM_INFINITY;
    use nix::sys::signal::{Signal, killpg};

    use super::*;

    #[test]
    fn a_copied_program_counts_as_running_from_its_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let installed = fs::canonicalize("/bin/sleep").expect("sleep is installed");
        let copy = directory.path().join("2.1.1");
        let copied = Command::new("cp").arg(&installed).arg(&copy).status();
        assert!(copied.expect("cp runs").success());
        let mut running = Command::new(&copy)
            .arg0("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .expect("the copy starts");
        fs::remove_file(&copy).expect("the copy is deleted while it runs");
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("the deadline fits");
        while Process::running_from(directory.path()).is_empty() {
            assert!(Instant::now() < deadline, "the copy did not start");
            sleep(Duration::from_millis(20));
        }

        assert_eq!(Process::running_from(directory.path()), ["2.1.1"]);

        running.kill().expect("the copy stops");
        running.wait().expect("the copy is reaped");
        assert!(Process::running_from(directory.path()).is_empty());
    }

    #[test]
    fn the_start_time_is_the_twenty_second_stat_field() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(
            directory.path().join("stat"),
            "42 (a (b) c) S 7 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 123456 2000000 100\n",
        )
        .expect("stat is written");
        let stat = Process(directory.path().to_path_buf())
            .stat()
            .expect("stat is readable");
        assert_eq!(
            (stat.parent, stat.zombie, stat.started),
            (Pid::from_raw(7), false, 123_456)
        );
    }

    #[test]
    fn a_family_is_the_leader_and_everything_it_started() {
        let mut leader = Command::new("sh")
            .args(["-c", "sh -c 'sleep 30; :' & sleep 30 & wait"])
            .process_group(0)
            .stdin(Stdio::null())
            .spawn()
            .expect("the leader starts");
        let leader_id = Pid::from_raw(i32::try_from(leader.id()).expect("the pid fits"));
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("the deadline fits");
        let family = loop {
            let family = ProcessFamily::of(leader_id);
            if family.members.len() == 4 {
                break family;
            }
            assert!(Instant::now() < deadline, "the family did not start");
            sleep(Duration::from_millis(20));
        };
        assert_eq!(family.children().count(), 2);
        assert!(family.memory_bytes() > 0);

        killpg(leader_id, Signal::SIGKILL).expect("the family is killed");
        leader.wait().expect("the leader is reaped");
        while killpg(leader_id, None) != Err(Errno::ESRCH) {
            assert!(Instant::now() < deadline, "the family did not exit");
            sleep(Duration::from_millis(20));
        }
        assert_eq!(family.memory_bytes(), 0);
        assert!(ProcessFamily::of(leader_id).members.is_empty());
    }

    #[test]
    fn children_leave_out_grandchildren_and_exited_children() {
        let member = |id: i32, parent: i32, zombie: bool| {
            (
                Process(PathBuf::from(format!("/proc/{id}"))),
                ProcessStat {
                    parent: Pid::from_raw(parent),
                    zombie,
                    started: 1,
                },
            )
        };
        let family = ProcessFamily {
            leader: Pid::from_raw(10),
            members: vec![
                member(10, 1, false),
                member(11, 10, false),
                member(12, 10, true),
                member(13, 11, false),
            ],
        };
        let children: Vec<Option<Pid>> = family.children().map(Process::id).collect();
        assert_eq!(children, [Some(Pid::from_raw(11))]);
    }

    #[test]
    fn memory_leaves_out_a_member_whose_pid_was_reused() {
        let own = PathBuf::from(format!("/proc/{}", process::id()));
        let started = Process(own.clone())
            .stat()
            .expect("this process has a stat")
            .started;
        let remembered = |started| ProcessFamily {
            leader: Pid::this(),
            members: vec![(
                Process(own.clone()),
                ProcessStat {
                    parent: Pid::parent(),
                    zombie: false,
                    started,
                },
            )],
        };
        assert!(remembered(started).memory_bytes() > 0);
        assert_eq!(remembered(started.saturating_add(1)).memory_bytes(), 0);
    }

    #[test]
    fn only_members_with_their_remembered_start_time_are_killed() {
        let sleep = || {
            Command::new("sleep")
                .arg("30")
                .stdin(Stdio::null())
                .spawn()
                .expect("sleep starts")
        };
        let (mut remembered, mut reused) = (sleep(), sleep());
        let member = |id: u32, ticks_later: u64| {
            let process = Process(PathBuf::from(format!("/proc/{id}")));
            let stat = process.stat().expect("the process has a stat");
            let stat = ProcessStat {
                started: stat.started.saturating_add(ticks_later),
                ..stat
            };
            (process, stat)
        };
        let family = ProcessFamily {
            leader: Pid::this(),
            members: vec![
                member(process::id(), 0),
                member(remembered.id(), 0),
                member(reused.id(), 1),
            ],
        };

        family.kill_survivors();

        let killed = remembered.wait().expect("sleep is reaped");
        assert_eq!(killed.signal(), Some(Signal::SIGKILL as i32));
        assert!(reused.try_wait().expect("sleep is readable").is_none());
        reused.kill().expect("sleep is killed");
        reused.wait().expect("sleep is reaped");
    }

    #[tokio::test]
    async fn only_the_leader_with_its_remembered_start_time_is_signalled() {
        let mut sleeping = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .expect("sleep starts");
        let id = Pid::from_raw(i32::try_from(sleeping.id()).expect("the pid fits"));
        let leader = ProcessFamily::of(id).leader().expect("sleep runs");
        let reused = ProcessIdentity {
            started: leader.started.saturating_add(1),
            ..leader
        };
        let this = ProcessFamily::of(Pid::this())
            .leader()
            .expect("this process runs");

        reused.signal(Signal::SIGKILL).await;
        this.signal(Signal::SIGKILL).await;
        assert!(reused.exits_within(Duration::ZERO).await);
        assert!(!leader.exits_within(Duration::from_millis(100)).await);

        leader.signal(Signal::SIGKILL).await;
        assert!(leader.exits_within(Duration::from_secs(5)).await);
        assert_eq!(ProcessFamily::of(id).leader(), None);
        let killed = sleeping.wait().expect("sleep is reaped");
        assert_eq!(killed.signal(), Some(Signal::SIGKILL as i32));
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
