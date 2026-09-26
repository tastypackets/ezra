use std::fs::{self, File};
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

pub trait PathExt {
    /// Entries of this directory, or none when it does not exist.
    fn entries_or_empty(&self) -> io::Result<Vec<PathBuf>>;

    /// Removes a file, link or whole directory. A missing path is not an error.
    fn remove_if_present(&self) -> io::Result<()>;

    /// Size of everything at or below this path, without following links. A missing path is zero.
    fn total_bytes(&self) -> io::Result<u64>;

    /// Unpacks this `.tar.gz` into `destination`, refusing entries that would land outside it. Blocks.
    fn unpack_tar_gz_into(&self, destination: &Path) -> io::Result<()>;

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

    fn unpack_tar_gz_into(&self, destination: &Path) -> io::Result<()> {
        fs::create_dir_all(destination)?;
        tar::Archive::new(GzDecoder::new(File::open(self)?)).unpack(destination)
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
    fn tar_gz_unpacks_into_the_destination() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let archive_path = directory.path().join("package.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            File::create(&archive_path).expect("archive is created"),
            flate2::Compression::fast(),
        );
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "bin/tool", &b"hello"[..])
            .expect("entry is added");
        builder
            .into_inner()
            .and_then(flate2::write::GzEncoder::finish)
            .expect("archive is finished");

        let destination = directory.path().join("unpacked");
        archive_path
            .unpack_tar_gz_into(&destination)
            .expect("archive unpacks");
        assert_eq!(
            fs::read_to_string(destination.join("bin/tool")).expect("entry is readable"),
            "hello"
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
