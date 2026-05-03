use std::path::{Path, PathBuf};

/// Guards a temporary test file and removes it on drop.
pub(crate) struct TempFile {
    path: PathBuf,
}

impl TempFile {
    /// Creates a file at `path` with `contents`, replacing any existing file at that path.
    pub(crate) fn with_contents(path: PathBuf, contents: &[u8]) -> std::io::Result<Self> {
        std::fs::write(&path, contents)?;
        Ok(Self { path })
    }

    /// Returns the temporary file path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Guards a temporary test directory and removes it recursively on drop.
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates a directory at `path`, removing any stale directory from a previous failed run.
    pub(crate) fn create(path: PathBuf) -> std::io::Result<Self> {
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// Returns the temporary directory path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn test_path(name: &str) -> PathBuf {
        let id = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("lava-flow-{name}-{id}"))
    }

    #[test]
    fn temp_file_removes_file_on_drop() {
        let path = test_path("temp-file-drop");
        {
            let file = TempFile::with_contents(path.clone(), b"temporary").expect("create file");
            assert_eq!(file.path(), path.as_path());
            assert_eq!(std::fs::read(&path).expect("read file"), b"temporary");
        }
        assert!(!path.exists());
    }

    #[test]
    fn temp_dir_removes_directory_on_drop() {
        let path = test_path("temp-dir-drop");
        {
            let directory = TempDir::create(path.clone()).expect("create directory");
            assert_eq!(directory.path(), path.as_path());
            std::fs::write(path.join("child"), b"temporary").expect("write child file");
            assert!(path.join("child").exists());
        }
        assert!(!path.exists());
    }
}
