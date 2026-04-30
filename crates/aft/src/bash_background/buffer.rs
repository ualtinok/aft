use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MEMORY_LIMIT_BYTES: usize = 1024 * 1024;
pub const DISK_LIMIT_BYTES: u64 = 100 * 1024 * 1024;
pub const DISK_RETAIN_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

pub struct BgBuffer {
    memory: Vec<u8>,
    spill_path: PathBuf,
    spill: Option<BufWriter<File>>,
    spilled_bytes: u64,
    rotated: bool,
    #[cfg(test)]
    fail_next_reopen: bool,
    #[cfg(test)]
    fail_next_retain: bool,
}

impl BgBuffer {
    pub fn new(task_id: &str, output_dir: PathBuf) -> Self {
        Self {
            memory: Vec::new(),
            spill_path: output_dir.join(format!("{task_id}.log")),
            spill: None,
            spilled_bytes: 0,
            rotated: false,
            #[cfg(test)]
            fail_next_reopen: false,
            #[cfg(test)]
            fail_next_retain: false,
        }
    }

    pub fn append(&mut self, _kind: StreamKind, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }

        if self.spill.is_none() && self.memory.len() + chunk.len() <= MEMORY_LIMIT_BYTES {
            self.memory.extend_from_slice(chunk);
            return;
        }

        if self.spill.is_none() && self.open_spill().is_err() {
            self.memory.extend_from_slice(chunk);
            if self.memory.len() > MEMORY_LIMIT_BYTES {
                let keep_from = self.memory.len().saturating_sub(MEMORY_LIMIT_BYTES);
                self.memory.drain(..keep_from);
                self.rotated = true;
            }
            return;
        }

        self.write_spill_chunk(chunk);
    }

    pub fn read_tail(&self, max_bytes: usize) -> (String, bool) {
        if max_bytes == 0 {
            return (String::new(), self.total_len() > 0 || self.rotated);
        }

        if self.spill.is_some() {
            return self.read_spill_tail(max_bytes);
        }

        let truncated = self.memory.len() > max_bytes || self.rotated;
        let start = self.memory.len().saturating_sub(max_bytes);
        (
            String::from_utf8_lossy(&self.memory[start..]).into_owned(),
            truncated,
        )
    }

    pub fn read_all(&self) -> io::Result<String> {
        if self.spill.is_some() {
            let mut bytes = Vec::new();
            File::open(&self.spill_path)?.read_to_end(&mut bytes)?;
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        Ok(String::from_utf8_lossy(&self.memory).into_owned())
    }

    pub fn spill_path(&self) -> Option<PathBuf> {
        self.spill.as_ref().map(|_| self.spill_path.clone())
    }

    pub fn cleanup(&self) {
        let _ = fs::remove_file(&self.spill_path);
    }

    fn open_spill(&mut self) -> io::Result<()> {
        if let Some(parent) = self.spill_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.spill_path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&self.memory)?;
        writer.flush()?;
        self.spilled_bytes = writer
            .get_ref()
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(self.memory.len() as u64);
        self.memory.clear();
        self.spill = Some(writer);
        Ok(())
    }

    fn write_spill_chunk(&mut self, mut chunk: &[u8]) {
        if self.spilled_bytes >= DISK_LIMIT_BYTES {
            self.rotated = true;
            return;
        }

        let remaining = (DISK_LIMIT_BYTES - self.spilled_bytes) as usize;
        if chunk.len() > remaining {
            chunk = &chunk[..remaining];
            self.rotated = true;
        }

        if let Some(writer) = self.spill.as_mut() {
            if writer.write_all(chunk).and_then(|_| writer.flush()).is_ok() {
                self.spilled_bytes += chunk.len() as u64;
                if self.spilled_bytes >= DISK_LIMIT_BYTES {
                    self.rotate_spill_file();
                }
            }
        }
    }

    fn rotate_spill_file(&mut self) {
        let Some(mut writer) = self.spill.take() else {
            return;
        };
        if writer.flush().is_err() {
            self.spill = Some(writer);
            return;
        }
        drop(writer);

        if self.retain_spill_tail().is_err() {
            self.spilled_bytes = 0;
            self.rotated = true;
            self.memory.clear();
            let _ = fs::remove_file(&self.spill_path);
            return;
        }
        self.rotated = true;

        match self.reopen_spill_append() {
            Ok(file) => {
                self.spilled_bytes = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                self.spill = Some(BufWriter::new(file));
            }
            Err(_) => {
                // Keep accounting aligned with the in-memory fallback. If the
                // append re-open fails, leaving spilled_bytes at the retained
                // disk size would make the next append truncate the file while
                // still reporting bytes that are no longer addressable.
                self.spilled_bytes = 0;
                if let Ok(mut retained) = fs::read(&self.spill_path) {
                    if retained.len() > MEMORY_LIMIT_BYTES {
                        let keep_from = retained.len() - MEMORY_LIMIT_BYTES;
                        retained.drain(..keep_from);
                    }
                    self.memory = retained;
                }
                let _ = fs::remove_file(&self.spill_path);
            }
        }
    }

    fn reopen_spill_append(&mut self) -> io::Result<File> {
        #[cfg(test)]
        if self.fail_next_reopen {
            self.fail_next_reopen = false;
            return Err(io::Error::other("injected spill reopen failure"));
        }

        OpenOptions::new().append(true).open(&self.spill_path)
    }

    fn retain_spill_tail(&self) -> io::Result<()> {
        #[cfg(test)]
        if self.fail_next_retain {
            return Err(io::Error::other("injected spill retain failure"));
        }

        let mut file = File::open(&self.spill_path)?;
        let len = file.metadata()?.len();
        let keep = len.min(DISK_RETAIN_BYTES);
        file.seek(SeekFrom::End(-(keep as i64)))?;
        let mut tail = Vec::with_capacity(keep as usize);
        file.read_to_end(&mut tail)?;
        fs::write(&self.spill_path, tail)
    }

    fn read_spill_tail(&self, max_bytes: usize) -> (String, bool) {
        let mut file = match File::open(&self.spill_path) {
            Ok(file) => file,
            Err(_) => return (String::new(), self.rotated),
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let read_len = len.min(max_bytes as u64);
        if file.seek(SeekFrom::End(-(read_len as i64))).is_err() {
            return (String::new(), self.rotated || len > max_bytes as u64);
        }
        let mut bytes = Vec::with_capacity(read_len as usize);
        if file.read_to_end(&mut bytes).is_err() {
            return (String::new(), self.rotated || len > max_bytes as u64);
        }
        (
            String::from_utf8_lossy(&bytes).into_owned(),
            self.rotated || len > max_bytes as u64,
        )
    }

    fn total_len(&self) -> u64 {
        if self.spill.is_some() {
            self.spilled_bytes
        } else {
            self.memory.len() as u64
        }
    }
}

pub fn default_output_dir(storage_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = std::env::var_os("AFT_CACHE_DIR") {
        return PathBuf::from(dir).join("aft").join("bash-output");
    }
    if let Some(dir) = storage_dir {
        return dir.join("bash-output");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("aft").join("bash-output")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_spill_file_reopen_failure_preserves_consistent_fallback_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut buffer = BgBuffer::new("rotate-reopen-fails", dir.path().to_path_buf());
        buffer.append(StreamKind::Stdout, &vec![b'a'; MEMORY_LIMIT_BYTES + 1]);
        let spill_path = buffer.spill_path.clone();

        buffer.spilled_bytes = DISK_LIMIT_BYTES;
        buffer.fail_next_reopen = true;
        buffer.rotate_spill_file();

        assert!(buffer.spill.is_none());
        assert_eq!(buffer.spilled_bytes, 0);
        assert!(buffer.memory.len() <= MEMORY_LIMIT_BYTES);
        assert!(buffer.rotated);
        assert!(!spill_path.exists());

        buffer.append(StreamKind::Stdout, b"after-failure");
        assert!(buffer.total_len() > 0);
        assert!(buffer.read_tail(64).0.contains("after-failure"));
    }

    #[test]
    fn rotate_spill_file_retain_failure_deletes_orphan_and_recovers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut buffer = BgBuffer::new("rotate-retain-fails", dir.path().to_path_buf());
        buffer.append(StreamKind::Stdout, &vec![b'a'; MEMORY_LIMIT_BYTES + 1]);
        let spill_path = buffer.spill_path.clone();
        assert!(spill_path.exists());

        buffer.spilled_bytes = DISK_LIMIT_BYTES;
        buffer.fail_next_retain = true;
        buffer.rotate_spill_file();

        assert!(buffer.spill.is_none());
        assert_eq!(buffer.spilled_bytes, 0);
        assert!(buffer.rotated);
        assert!(!spill_path.exists());

        buffer.append(StreamKind::Stdout, b"after-retain-failure");
        assert!(buffer.read_tail(64).0.contains("after-retain-failure"));
    }

    #[test]
    fn open_spill_appends_without_truncating_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut buffer = BgBuffer::new("existing-spill", dir.path().to_path_buf());
        let spill_path = buffer.spill_path.clone();
        fs::create_dir_all(dir.path()).expect("create output dir");
        fs::write(&spill_path, b"preexisting-").expect("seed spill file");

        buffer.append(StreamKind::Stdout, &vec![b'n'; MEMORY_LIMIT_BYTES + 1]);
        let contents = fs::read(&spill_path).expect("read spill file");

        assert!(contents.starts_with(b"preexisting-"));
        assert!(contents.len() > MEMORY_LIMIT_BYTES);
    }
}
