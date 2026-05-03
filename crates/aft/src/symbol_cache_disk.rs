use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::parser::SymbolCache;
use crate::symbols::Symbol;
use crate::{slog_info, slog_warn};

const MAGIC: &[u8; 8] = b"AFTSYM1\0";
const VERSION: u32 = 1;
const MAX_ENTRIES: usize = 2_000_000;
const MAX_PATH_BYTES: usize = 16 * 1024;
const MAX_SYMBOL_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DiskSymbolCache {
    pub(crate) project_root: PathBuf,
    pub(crate) entries: Vec<DiskSymbolEntry>,
}

#[derive(Debug, Clone)]
pub(crate) struct DiskSymbolEntry {
    pub(crate) relative_path: PathBuf,
    pub(crate) mtime: SystemTime,
    pub(crate) size: u64,
    pub(crate) symbols: Vec<Symbol>,
}

impl DiskSymbolCache {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub(crate) fn cache_path(storage_dir: &Path, project_key: &str) -> PathBuf {
    storage_dir
        .join("symbols")
        .join(project_key)
        .join("symbols.bin")
}

pub fn read_from_disk(storage_dir: &Path, project_key: &str) -> Option<DiskSymbolCache> {
    let data_path = cache_path(storage_dir, project_key);
    if !data_path.exists() {
        return None;
    }

    match read_cache_file(&data_path) {
        Ok(cache) => Some(cache),
        Err(error) => {
            slog_warn!(
                "corrupt symbol cache at {}: {}, rebuilding",
                data_path.display(),
                error
            );
            None
        }
    }
}

pub fn write_to_disk(
    cache: &SymbolCache,
    storage_dir: &Path,
    project_key: &str,
) -> std::io::Result<()> {
    if cache.len() == 0 {
        slog_info!("skipping symbol cache persistence (0 entries)");
        return Ok(());
    }

    let project_root = cache.project_root().ok_or_else(|| {
        std::io::Error::other("symbol cache project root is not set; cannot persist relative paths")
    })?;

    let dir = storage_dir.join("symbols").join(project_key);
    fs::create_dir_all(&dir)?;

    let data_path = dir.join("symbols.bin");
    let tmp_path = dir.join("symbols.bin.tmp");
    let write_result = write_cache_file(cache, &project_root, &tmp_path).and_then(|()| {
        fs::rename(&tmp_path, &data_path)?;
        if let Ok(dir_file) = File::open(&dir) {
            let _ = dir_file.sync_all();
        }
        Ok(())
    });

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    write_result
}

fn read_cache_file(path: &Path) -> Result<DiskSymbolCache, String> {
    let mut reader = BufReader::new(File::open(path).map_err(|error| error.to_string())?);

    let mut magic = [0u8; 8];
    reader
        .read_exact(&mut magic)
        .map_err(|error| format!("failed to read symbol cache magic: {error}"))?;
    if &magic != MAGIC {
        return Err("invalid symbol cache magic".to_string());
    }

    let version = read_u32(&mut reader)?;
    if version != VERSION {
        return Err(format!(
            "unsupported symbol cache version: {version} (expected {VERSION})"
        ));
    }

    let root_len = read_u32(&mut reader)? as usize;
    let entry_count = read_u32(&mut reader)? as usize;
    if root_len > MAX_PATH_BYTES {
        return Err(format!("project root path too large: {root_len} bytes"));
    }
    if entry_count > MAX_ENTRIES {
        return Err(format!("too many symbol cache entries: {entry_count}"));
    }

    let project_root = PathBuf::from(read_string_with_len(&mut reader, root_len)?);
    let mut entries = Vec::with_capacity(entry_count);

    for _ in 0..entry_count {
        let path_len = read_u32(&mut reader)? as usize;
        if path_len > MAX_PATH_BYTES {
            return Err(format!("cached path too large: {path_len} bytes"));
        }
        let relative_path = PathBuf::from(read_string_with_len(&mut reader, path_len)?);
        let mtime_secs = read_i64(&mut reader)?;
        let mtime_nanos = read_u32(&mut reader)?;
        let size = read_u64(&mut reader)?;
        let symbol_bytes_len = read_u32(&mut reader)? as usize;
        if symbol_bytes_len > MAX_SYMBOL_BYTES {
            return Err(format!(
                "cached symbol payload too large: {symbol_bytes_len} bytes"
            ));
        }

        let mut symbol_bytes = vec![0u8; symbol_bytes_len];
        reader
            .read_exact(&mut symbol_bytes)
            .map_err(|error| format!("failed to read symbol payload: {error}"))?;
        let symbols: Vec<Symbol> = serde_json::from_slice(&symbol_bytes)
            .map_err(|error| format!("failed to decode cached symbols: {error}"))?;

        entries.push(DiskSymbolEntry {
            relative_path,
            mtime: system_time_from_parts(mtime_secs, mtime_nanos)?,
            size,
            symbols,
        });
    }

    Ok(DiskSymbolCache {
        project_root,
        entries,
    })
}

fn write_cache_file(
    cache: &SymbolCache,
    project_root: &Path,
    tmp_path: &Path,
) -> std::io::Result<()> {
    let mut writer = BufWriter::new(File::create(tmp_path)?);
    let entries = cache.disk_entries();
    let root = project_root.to_string_lossy();
    let root_len = u32::try_from(root.len())
        .map_err(|_| std::io::Error::other("project root too large to cache"))?;
    let entry_count = u32::try_from(entries.len())
        .map_err(|_| std::io::Error::other("too many symbol cache entries"))?;

    writer.write_all(MAGIC)?;
    write_u32(&mut writer, VERSION)?;
    write_u32(&mut writer, root_len)?;
    write_u32(&mut writer, entry_count)?;
    writer.write_all(root.as_bytes())?;

    for (path, mtime, size, symbols) in entries {
        if symbols.is_empty() {
            continue;
        }
        let relative_path = path.strip_prefix(project_root).unwrap_or(path.as_path());
        let path_bytes = relative_path.to_string_lossy();
        let path_len = u32::try_from(path_bytes.len())
            .map_err(|_| std::io::Error::other("cached path too large"))?;
        let (secs, nanos) = system_time_parts(mtime);
        let symbol_bytes = serde_json::to_vec(symbols).map_err(|error| {
            std::io::Error::other(format!("symbol serialization failed: {error}"))
        })?;
        let symbol_len = u32::try_from(symbol_bytes.len())
            .map_err(|_| std::io::Error::other("cached symbol payload too large"))?;

        write_u32(&mut writer, path_len)?;
        writer.write_all(path_bytes.as_bytes())?;
        write_i64(&mut writer, secs)?;
        write_u32(&mut writer, nanos)?;
        write_u64(&mut writer, size)?;
        write_u32(&mut writer, symbol_len)?;
        writer.write_all(&symbol_bytes)?;
    }

    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn system_time_parts(time: SystemTime) -> (i64, u32) {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => (
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            duration.subsec_nanos(),
        ),
        Err(error) => {
            let duration = error.duration();
            let nanos = duration.subsec_nanos();
            if nanos == 0 {
                (-(duration.as_secs() as i64), 0)
            } else {
                (-(duration.as_secs() as i64) - 1, 1_000_000_000 - nanos)
            }
        }
    }
}

fn system_time_from_parts(secs: i64, nanos: u32) -> Result<SystemTime, String> {
    if nanos >= 1_000_000_000 {
        return Err(format!(
            "invalid symbol cache mtime nanos: {nanos} >= 1_000_000_000"
        ));
    }

    if secs >= 0 {
        let duration = Duration::new(secs as u64, nanos);
        UNIX_EPOCH
            .checked_add(duration)
            .ok_or_else(|| format!("symbol cache mtime overflows SystemTime: {secs}.{nanos}"))
    } else {
        let whole = Duration::new(secs.unsigned_abs(), 0);
        let base = UNIX_EPOCH.checked_sub(whole).ok_or_else(|| {
            format!("symbol cache negative mtime overflows SystemTime: {secs}.{nanos}")
        })?;
        base.checked_add(Duration::new(0, nanos)).ok_or_else(|| {
            format!("symbol cache negative mtime overflows SystemTime: {secs}.{nanos}")
        })
    }
}

fn read_string_with_len<R: Read>(reader: &mut R, len: usize) -> Result<String, String> {
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("failed to read string: {error}"))?;
    String::from_utf8(bytes).map_err(|error| format!("invalid utf-8 string: {error}"))
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32, String> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("failed to read u32: {error}"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_i64<R: Read>(reader: &mut R) -> Result<i64, String> {
    let mut bytes = [0u8; 8];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("failed to read i64: {error}"))?;
    Ok(i64::from_le_bytes(bytes))
}

fn read_u64<R: Read>(reader: &mut R) -> Result<u64, String> {
    let mut bytes = [0u8; 8];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("failed to read u64: {error}"))?;
    Ok(u64::from_le_bytes(bytes))
}

fn write_u32<W: Write>(writer: &mut W, value: u32) -> std::io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_i64<W: Write>(writer: &mut W, value: i64) -> std::io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_u64<W: Write>(writer: &mut W, value: u64) -> std::io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}
