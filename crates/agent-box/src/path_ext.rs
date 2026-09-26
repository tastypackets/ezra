use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub trait PathExt {
    /// Entries of this directory, or none when it does not exist.
    fn entries_or_empty(&self) -> io::Result<Vec<PathBuf>>;

    /// Removes a file, link or whole directory. A missing path is not an error.
    fn remove_if_present(&self) -> io::Result<()>;

    /// Size of everything at or below this path, without following links. A missing path is zero.
    fn total_bytes(&self) -> io::Result<u64>;

    /// Files with this extension in this directory, and in its subdirectories when `recursive`.
    fn count_files_with_extension(&self, extension: &str, recursive: bool) -> io::Result<u64>;

    /// SHA-256 of the file's contents as lowercase hex.
    fn sha256_hex(&self) -> io::Result<String>;

    /// Points this link at `target`, replacing any existing link in one step.
    fn replace_symlink(&self, target: &Path) -> io::Result<()>;
}

impl PathExt for Path {
    fn entries_or_empty(&self) -> io::Result<Vec<PathBuf>> {
        match fs::read_dir(self) {
            Ok(entries) => entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    fn remove_if_present(&self) -> io::Result<()> {
        let outcome = match fs::symlink_metadata(self) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(self),
            Ok(_) => fs::remove_file(self),
            Err(error) => Err(error),
        };
        match outcome {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            outcome => outcome,
        }
    }

    fn total_bytes(&self) -> io::Result<u64> {
        let metadata = match fs::symlink_metadata(self) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            metadata => metadata?,
        };
        if !metadata.is_dir() {
            return Ok(metadata.len());
        }
        self.entries_or_empty()?
            .iter()
            .try_fold(0_u64, |total, entry| {
                Ok(total.saturating_add(entry.total_bytes()?))
            })
    }

    fn count_files_with_extension(&self, extension: &str, recursive: bool) -> io::Result<u64> {
        self.entries_or_empty()?
            .iter()
            .try_fold(0_u64, |count, entry| {
                let metadata = fs::symlink_metadata(entry)?;
                let found = if metadata.is_dir() && recursive {
                    entry.count_files_with_extension(extension, true)?
                } else {
                    u64::from(
                        metadata.is_file()
                            && entry.extension().is_some_and(|found| found == extension),
                    )
                };
                Ok(count.saturating_add(found))
            })
    }

    fn sha256_hex(&self) -> io::Result<String> {
        let mut file = File::open(self)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 1 << 20];
        loop {
            let read = file.read(&mut buffer)?;
            let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
                break;
            };
            hasher.update(chunk);
        }
        Ok(hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    fn replace_symlink(&self, target: &Path) -> io::Result<()> {
        let directory = self.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(directory)?;
        let staging_link = directory.join(format!(
            ".{}.new",
            self.file_name().unwrap_or_default().to_string_lossy()
        ));
        staging_link.remove_if_present()?;
        symlink(target, &staging_link)?;
        fs::rename(&staging_link, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("test paths have a parent"))
            .expect("parent directory is created");
        fs::write(path, contents).expect("test file is written");
    }

    #[test]
    fn missing_directory_has_no_entries() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("missing");
        assert!(
            missing
                .entries_or_empty()
                .expect("listing works")
                .is_empty()
        );
        assert_eq!(missing.total_bytes().expect("size works"), 0);
        missing
            .remove_if_present()
            .expect("removing a missing path is fine");
    }

    #[test]
    fn total_bytes_adds_up_nested_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        write(&directory.path().join("a"), "12345");
        write(&directory.path().join("nested/b"), "123");
        assert_eq!(directory.path().total_bytes().expect("size works"), 8);
    }

    #[test]
    fn files_are_counted_by_extension() {
        let directory = tempfile::tempdir().expect("temporary directory");
        write(&directory.path().join("one.jsonl"), "");
        write(&directory.path().join("notes.txt"), "");
        write(&directory.path().join("2026/09/two.jsonl"), "");
        let count = |recursive| {
            directory
                .path()
                .count_files_with_extension("jsonl", recursive)
                .expect("counting works")
        };
        assert_eq!(count(false), 1);
        assert_eq!(count(true), 2);
    }

    #[test]
    fn sha256_is_lowercase_hex() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("file");
        write(&path, "abc");
        assert_eq!(
            path.sha256_hex().expect("hashing works"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn symlink_is_replaced_in_place() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let link = directory.path().join("bin/tool");
        link.replace_symlink(Path::new("/first"))
            .expect("link is created");
        link.replace_symlink(Path::new("/second"))
            .expect("link is replaced");
        assert_eq!(
            fs::read_link(&link).expect("link is readable"),
            Path::new("/second")
        );
    }

    #[test]
    fn directories_are_removed_recursively() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let tree = directory.path().join("tree");
        write(&tree.join("deep/file"), "x");
        tree.remove_if_present().expect("tree is removed");
        assert!(!tree.exists());
    }
}
