use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::environment::EnvironmentOverride;

pub struct BrowserCache(PathBuf);

impl BrowserCache {
    pub fn from_environment(overrides: &[EnvironmentOverride]) -> Option<Self> {
        if let Some(path) = env::var_os("PLAYWRIGHT_BROWSERS_PATH") {
            return (path != "0").then(|| Self(PathBuf::from(path)));
        }
        let cache = env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                overrides
                    .iter()
                    .find(|variable| variable.name == "HOME")
                    .map(|variable| variable.value.clone())
                    .or_else(|| env::var_os("HOME"))
                    .map(|home| PathBuf::from(home).join(".cache"))
            })?;
        Some(Self(cache.join("ms-playwright")))
    }

    pub fn link_bundled(&self, bundled: &Path) -> io::Result<()> {
        let entries = match fs::read_dir(bundled) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            fs::create_dir_all(&self.0)?;
            let destination = self.0.join(entry.file_name());
            match fs::symlink_metadata(&destination) {
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            symlink(entry.path(), destination)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_image_revisions_are_linked_without_replacing_user_downloads() {
        let root = tempfile::tempdir().expect("temporary directory");
        let bundled = root.path().join("bundled");
        let cache = BrowserCache(root.path().join("home/.cache/ms-playwright"));
        fs::create_dir_all(bundled.join("ffmpeg-1")).expect("first revision");
        cache.link_bundled(&bundled).expect("first start");
        assert_eq!(
            fs::read_link(cache.0.join("ffmpeg-1")).expect("link"),
            bundled.join("ffmpeg-1")
        );
        fs::create_dir_all(bundled.join("ffmpeg-2")).expect("new revision");
        fs::create_dir_all(cache.0.join("ffmpeg-3")).expect("user download");
        fs::create_dir_all(bundled.join("ffmpeg-3")).expect("matching revision");
        cache.link_bundled(&bundled).expect("next image");
        assert_eq!(
            fs::read_link(cache.0.join("ffmpeg-2")).expect("new link"),
            bundled.join("ffmpeg-2")
        );
        assert!(
            !fs::symlink_metadata(cache.0.join("ffmpeg-3"))
                .expect("download")
                .file_type()
                .is_symlink()
        );
        cache.link_bundled(&bundled).expect("repeated start");
    }
}
