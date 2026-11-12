use super::workspace::valid_relative;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use ropey::{Rope, RopeBuilder};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub enum ReadEvent {
    Chunk(Rope),
    Complete,
    Error(io::Error),
}
pub type ReadSink = Arc<dyn Fn(ReadEvent) + Send + Sync>;
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub directory: bool,
    pub link: bool,
}

pub struct LocalStorage {
    root: PathBuf,
}
impl LocalStorage {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        if !valid_relative(path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A workspace-relative path is required",
            ));
        }
        // Canonicalizing the parent also prevents symlinks from escaping the workspace.
        let root = self.root.canonicalize()?;
        let path: PathBuf = path
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(name) => Some(name),
                _ => None,
            })
            .collect();
        if path.as_os_str().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The workspace root cannot be changed as a file",
            ));
        }
        let target = root.join(path);
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("Missing parent folder"))?
            .canonicalize()?;
        if !parent.starts_with(&root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Path leaves the workspace",
            ));
        }
        if target.exists() && !target.canonicalize()?.starts_with(&root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Symlink leaves the workspace",
            ));
        }
        Ok(target)
    }
    pub fn read(&self, path: &Path, sink: ReadSink) {
        let storage = Self::new(self.root.clone());
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let result = storage
                .resolve(&path)
                .and_then(|target| stream_utf8(File::open(target)?, &*sink));
            match result {
                Ok(()) => sink(ReadEvent::Complete),
                Err(error) => sink(ReadEvent::Error(error)),
            }
        });
    }
    pub fn write(&self, path: &Path, text: &Rope) -> io::Result<()> {
        let mut target = self.resolve(path)?;
        // Write through a symlink to its file instead of replacing the link.
        if target.is_symlink() {
            target = target.canonicalize()?;
        }
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        // A fixed-length name keeps long file names under the 255-character component limit.
        let name = format!(".{}-{}.tmp", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
        target.file_name().ok_or_else(|| io::Error::other("Missing file name"))?;
        let temporary = target.with_file_name(name);
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        let result = (|| {
            text.write_to(&mut file)?;
            file.sync_all()?;
            // The handle must be closed before the rename on Windows.
            drop(file);
            if target.exists() {
                fs::set_permissions(&temporary, fs::metadata(&target)?.permissions())?;
            }
            // A short-lived reader or antivirus handle can delay replacement on Windows.
            let mut attempt = 0;
            loop {
                match fs::rename(&temporary, &target) {
                    Err(error) if attempt < 4 && error.kind() == io::ErrorKind::PermissionDenied => {
                        std::thread::sleep(std::time::Duration::from_millis(10 << attempt));
                        attempt += 1;
                    }
                    result => return result,
                }
            }
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    pub fn list(&self, path: &Path) -> io::Result<Vec<Entry>> {
        let directory = if valid_relative(path)
            && path.components().all(|c| c == std::path::Component::CurDir)
        {
            self.root.canonicalize()?
        } else {
            self.resolve(path)?
        };
        fs::read_dir(directory)?
            .map(|entry| {
                let entry = entry?;
                let path = path
                    .join(entry.file_name())
                    .components()
                    .filter_map(|c| match c {
                        std::path::Component::Normal(name) => Some(name),
                        _ => None,
                    })
                    .collect();
                Ok(Entry {
                    path,
                    // fs::metadata follows junctions and directory symlinks.
                    directory: entry.file_type()?.is_dir()
                        || fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()),
                    link: entry.file_type()?.is_symlink(),
                })
            })
            .collect()
    }
    pub fn watch(&self, changed: Arc<dyn Fn(PathBuf) + Send + Sync>) -> io::Result<RecommendedWatcher> {
        let root = self.root.canonicalize()?;
        let base = root.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: Result<notify::Event, notify::Error>| {
                if let Ok(event) = event {
                    if event.kind.is_modify() || event.kind.is_create() || event.kind.is_remove() {
                        for path in event.paths {
                            if let Ok(relative) = path.strip_prefix(&base) {
                                changed(relative.to_path_buf());
                            }
                        }
                    }
                }
            })
            .map_err(io::Error::other)?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(io::Error::other)?;
        Ok(watcher)
    }
    pub fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let target = self.resolve(to)?;
        let source = self.resolve(from)?;
        // A case-only rename finds the same file on Windows.
        let same = source.canonicalize().ok() == target.canonicalize().ok();
        if target.exists() && !same {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "A file with that name already exists",
            ));
        }
        fs::rename(source, target)
    }
    pub fn trash(&self, path: &Path) -> io::Result<()> {
        crate::platform::recycle(&self.resolve(path)?)
    }
    pub fn create(&self, path: &Path, folder: bool) -> io::Result<()> {
        let target = self.resolve(path)?;
        if folder {
            fs::create_dir(target)
        } else {
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(target)?
                .sync_all()
        }
    }
    pub fn write_checked(&self, path: &Path, text: &Rope, expected: &Rope) -> io::Result<Option<Rope>> {
        let disk = match self.read_complete(path) {
            Ok(disk) => disk,
            // The file was deleted outside the app: write it again so the edits are kept.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return self.write(path, text).map(|_| None);
            }
            Err(error) => return Err(error),
        };
        if disk != *expected {
            return Ok(Some(disk));
        }
        self.write(path, text)?;
        Ok(None)
    }
    pub fn read_complete(&self, path: &Path) -> io::Result<Rope> {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.read(
            path,
            Arc::new(move |event| {
                let _ = sender.send(event);
            }),
        );
        let mut text = Rope::new();
        while let Ok(event) = receiver.recv() {
            match event {
                ReadEvent::Chunk(chunk) => text.append(chunk),
                ReadEvent::Complete => return Ok(text),
                ReadEvent::Error(error) => return Err(error),
            }
        }
        Err(io::Error::other("File loader stopped"))
    }
}

fn stream_utf8(mut file: impl Read, sink: &dyn Fn(ReadEvent)) -> io::Result<()> {
    let mut bytes = vec![0; 8192];
    let mut pending = Vec::new();
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            if !pending.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "File ends with incomplete UTF-8",
                ));
            }
            return Ok(());
        }
        pending.extend_from_slice(&bytes[..count]);
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "This file is not UTF-8",
                ));
            }
        };
        if valid > 0 {
            let mut builder = RopeBuilder::new();
            builder.append(std::str::from_utf8(&pending[..valid]).map_err(io::Error::other)?);
            sink(ReadEvent::Chunk(builder.finish()));
            pending.drain(..valid);
        }
        // Send the first screen early, then amortize the rest in 1 MiB batches.
        if bytes.len() < 1024 * 1024 {
            bytes.resize(1024 * 1024, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_utf8_and_atomic_replace() {
        let mut data = "a".repeat(8191);
        data.push_str("日本語\r\n");
        let loaded = std::sync::Mutex::new(String::new());
        stream_utf8(data.as_bytes(), &|event| {
            if let ReadEvent::Chunk(chunk) = event {
                loaded.lock().unwrap().push_str(&chunk.to_string());
            }
        })
        .unwrap();
        assert_eq!(*loaded.lock().unwrap(), data);
        assert!(stream_utf8(&[0xff][..], &|_| {}).is_err());
        let root = std::env::temp_dir().join(format!("jame-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let store = LocalStorage::new(root.clone());
        store
            .write(Path::new("test.md"), &Rope::from_str("first"))
            .unwrap();
        store
            .write(Path::new("test.md"), &Rope::from_str(&data))
            .unwrap();
        assert_eq!(fs::read_to_string(root.join("test.md")).unwrap(), data);
        let conflict = store
            .write_checked(
                Path::new("test.md"),
                &Rope::from_str("wrong"),
                &Rope::from_str("first"),
            )
            .unwrap();
        assert_eq!(conflict.unwrap().to_string(), data);
        assert_eq!(fs::read_to_string(root.join("test.md")).unwrap(), data);
        assert!(store.create(Path::new("test.md"), false).is_err());
        let entries = store.list(Path::new(".")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, Path::new("test.md"));
        assert!(store.trash(Path::new(".")).is_err());
        assert!(
            store
                .write(Path::new("../escape.md"), &Rope::new())
                .is_err()
        );
        store.rename(Path::new("test.md"), Path::new("Test.md")).unwrap();
        fs::remove_file(root.join("Test.md")).unwrap();
        let kept = store
            .write_checked(Path::new("gone.md"), &Rope::from_str("mine"), &Rope::from_str("old"))
            .unwrap();
        assert!(kept.is_none());
        assert_eq!(fs::read_to_string(root.join("gone.md")).unwrap(), "mine");
        #[cfg(windows)]
        store.trash(Path::new("gone.md")).unwrap();
        #[cfg(not(windows))]
        fs::remove_file(root.join("gone.md")).unwrap();
        assert!(!root.join("gone.md").exists());
        fs::remove_dir(root).unwrap();
    }
    #[test]
    fn long_file_name_saves() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("{}.md", "a".repeat(250));
        LocalStorage::new(dir.path().to_path_buf())
            .write(Path::new(&name), &Rope::from_str("x"))
            .unwrap();
        assert_eq!(fs::read_to_string(dir.path().join(name)).unwrap(), "x");
    }
}
