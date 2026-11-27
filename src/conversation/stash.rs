use huggler::ThreadId;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::{self, ErrorKind},
    path::PathBuf,
};

pub(super) struct Stash {
    directory: PathBuf,
    entries: HashSet<String>,
}

impl Stash {
    pub(super) fn load(directory: PathBuf) -> (Self, Option<io::Error>) {
        let mut stash = Self {
            directory,
            entries: HashSet::new(),
        };
        let result = (|| {
            for entry in fs::read_dir(&stash.directory)? {
                let entry = entry?;
                if entry.file_type()?.is_file()
                    && let Ok(key) = entry.file_name().into_string()
                {
                    stash.entries.insert(key);
                }
            }
            Ok::<(), io::Error>(())
        })();
        let error = result
            .err()
            .filter(|error| error.kind() != ErrorKind::NotFound);
        (stash, error)
    }

    pub(super) fn contains(&self, thread: &ThreadId) -> bool {
        self.entries.contains(&Self::key(thread))
    }

    pub(super) fn set(&mut self, thread: &ThreadId, stashed: bool) -> io::Result<()> {
        let key = Self::key(thread);
        let path = self.directory.join(&key);
        if stashed {
            fs::create_dir_all(&self.directory)?;
            // One marker per thread keeps updates to different conversations independent.
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(path)?;
            self.entries.insert(key);
        } else {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            self.entries.remove(&key);
        }
        Ok(())
    }

    fn key(thread: &ThreadId) -> String {
        format!("{:x}", Sha256::digest(thread.0.as_bytes()))
    }
}
