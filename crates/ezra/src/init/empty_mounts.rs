use nix::unistd::User;
use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, CWD, Dir, Gid, Mode, OFlags, ResolveFlags, StatxAttributes, StatxFlags, Uid, fchown,
    openat2, statx,
};
use rustix::io;
use serde::Deserialize;

const OTHERS_WRITE_BIT: u16 = 0o002;

/// Whether init gives the agent its directories when they are empty mounts owned by root, from
/// `EZRA_CHOWN_EMPTY_MOUNTS`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmptyMountOwnership {
    #[default]
    On,
    Off,
}

impl EmptyMountOwnership {
    /// Runs as root, before anything else writes to the directories. Never fails the start: a
    /// directory it cannot hand over is left to the writable check after the switch.
    pub fn hand_over(self, directories: &[&str], agent: &User) {
        if self == Self::Off {
            return;
        }
        for directory in directories {
            match MountedDirectory::open(directory).and_then(|mount| mount.hand_over_to(agent)) {
                Ok(true) => tracing::info!(
                    "{directory} was an empty mount owned by root, so it now belongs to {}",
                    agent.name
                ),
                Ok(false) => {}
                Err(error) => tracing::debug!("left the owner of {directory} alone ({error})"),
            }
        }
    }
}

/// One of the agent's directories, opened without following a symlink anywhere in its path.
struct MountedDirectory(OwnedFd);

impl MountedDirectory {
    fn open(path: &str) -> io::Result<Self> {
        openat2(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .map(Self)
    }

    /// Hands the directory to `agent` when it is an empty mount owned by root, returning whether it did.
    fn hand_over_to(&self, agent: &User) -> io::Result<bool> {
        if !self.state()?.is_unclaimed() {
            return Ok(false);
        }
        fchown(
            &self.0,
            Some(Uid::from_raw(agent.uid.as_raw())),
            Some(Gid::from_raw(agent.gid.as_raw())),
        )?;
        Ok(true)
    }

    fn state(&self) -> io::Result<DirectoryState> {
        let status = statx(&self.0, "", AtFlags::EMPTY_PATH, StatxFlags::BASIC_STATS)?;
        let mut entries = Dir::read_from(&self.0)?;
        let mut is_empty = true;
        while let Some(entry) = entries.read() {
            if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
                is_empty = false;
                break;
            }
        }
        Ok(DirectoryState {
            owned_by_root: status.stx_uid == 0 && status.stx_gid == 0,
            writable_by_others: status.stx_mode & OTHERS_WRITE_BIT != 0,
            mount_root: status
                .stx_attributes_mask
                .contains(StatxAttributes::MOUNT_ROOT)
                .then(|| status.stx_attributes.contains(StatxAttributes::MOUNT_ROOT)),
            is_empty,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct DirectoryState {
    owned_by_root: bool,
    writable_by_others: bool,
    /// None when the kernel cannot tell, before Linux 5.8.
    mount_root: Option<bool>,
    is_empty: bool,
}

impl DirectoryState {
    fn is_unclaimed(self) -> bool {
        self.owned_by_root
            && !self.writable_by_others
            && self.mount_root == Some(true)
            && self.is_empty
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    const FRESH_MOUNT: DirectoryState = DirectoryState {
        owned_by_root: true,
        writable_by_others: false,
        mount_root: Some(true),
        is_empty: true,
    };

    #[test]
    fn only_an_empty_root_owned_mount_is_unclaimed() {
        assert!(FRESH_MOUNT.is_unclaimed());
        for state in [
            DirectoryState {
                owned_by_root: false,
                ..FRESH_MOUNT
            },
            DirectoryState {
                writable_by_others: true,
                ..FRESH_MOUNT
            },
            DirectoryState {
                mount_root: Some(false),
                ..FRESH_MOUNT
            },
            DirectoryState {
                mount_root: None,
                ..FRESH_MOUNT
            },
            DirectoryState {
                is_empty: false,
                ..FRESH_MOUNT
            },
        ] {
            assert!(!state.is_unclaimed(), "{state:?}");
        }
    }

    #[test]
    fn symlinks_are_not_followed() {
        let root = tempfile::tempdir().expect("temporary directory");
        let target = root.path().join("target");
        fs::create_dir(&target).expect("target directory");
        let link = root.path().join("link");
        symlink(&target, &link).expect("symlink");
        let parent_link = root.path().join("parent-link");
        symlink(root.path(), &parent_link).expect("symlink");

        for path in [link, parent_link.join("target")] {
            let error = MountedDirectory::open(path.to_str().expect("UTF-8 path"))
                .err()
                .expect("symlinked path is refused");
            assert_eq!(error, io::Errno::LOOP, "{}", path.display());
        }
    }

    #[test]
    fn contents_are_seen_through_the_descriptor() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().to_str().expect("UTF-8 path");
        let opened = MountedDirectory::open(path).expect("directory opens");
        assert!(opened.state().expect("state").is_empty);

        fs::write(directory.path().join("file"), "").expect("file");
        assert!(!opened.state().expect("state").is_empty);
    }
}
