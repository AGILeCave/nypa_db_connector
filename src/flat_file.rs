use std::{
    fs::File,
    io::{BufReader, Error, ErrorKind, Read, Result},
    path::{Path, PathBuf},
};

use crate::DataFrame;

/// Wire format used for flat-file replay.
#[derive(Clone, Copy, Debug)]
pub enum FlatFileFormat {
    /// `f32 timestamp_seconds`, followed by `variable_count` payload `f32`s.
    F32 { variable_count: usize },
    /// `u32 marker`, `f64 timestamp_seconds`, `variable_count` payload `f64`s, `u32 marker`.
    SentinelF64 { variable_count: usize },
}

pub(crate) struct FlatFileReplay {
    path: Option<PathBuf>,
    reader: Option<BufReader<File>>,
    format: FlatFileFormat,
    finished: bool,
    f64_cache: Vec<f64>,
}

#[derive(Debug)]
pub(crate) enum FlatFileRead {
    Frame(DataFrame),
    Closed,
}

impl FlatFileReplay {
    pub(crate) fn open(path: &Path, format: FlatFileFormat) -> Result<Self> {
        Ok(Self {
            path: Some(path.to_path_buf()),
            reader: Some(BufReader::new(File::open(path)?)),
            format,
            finished: false,
            f64_cache: f64_cache_for_format(format),
        })
    }

    pub(crate) fn closed(format: FlatFileFormat) -> Self {
        Self {
            path: None,
            reader: None,
            format,
            finished: true,
            f64_cache: f64_cache_for_format(format),
        }
    }

    pub(crate) fn mark_finished(&mut self) {
        self.path = None;
        self.reader = None;
        self.finished = true;
    }

    pub(crate) fn read_next(&mut self) -> Result<FlatFileRead> {
        self.reopen_if_needed()?;

        let mut looped = false;

        loop {
            let Some(reader) = &mut self.reader else {
                self.finished = true;
                return Ok(FlatFileRead::Closed);
            };

            let result = match self.format {
                FlatFileFormat::F32 { variable_count } => {
                    read_flat_file_f32(reader, variable_count)
                }
                FlatFileFormat::SentinelF64 { variable_count } => {
                    read_flat_file_sentinel_f64(reader, variable_count, &mut self.f64_cache)
                }
            };

            match result {
                Ok(FlatFileRead::Frame(frame)) => return Ok(FlatFileRead::Frame(frame)),
                Ok(FlatFileRead::Closed) if self.path.is_some() && !looped => {
                    looped = true;
                    self.reopen()?;
                }
                Ok(FlatFileRead::Closed) => {
                    self.finished = true;
                    return Ok(FlatFileRead::Closed);
                }
                Err(err) => {
                    self.path = None;
                    self.reader = None;
                    self.finished = true;
                    return Err(err);
                }
            }
        }
    }

    fn reopen_if_needed(&mut self) -> Result<()> {
        if self.finished && self.path.is_some() {
            self.reopen()?;
        }

        Ok(())
    }

    fn reopen(&mut self) -> Result<()> {
        let Some(path) = &self.path else {
            self.finished = true;
            return Ok(());
        };

        self.reader = Some(BufReader::new(File::open(path)?));
        self.finished = false;
        Ok(())
    }
}

fn read_flat_file_f32(source: &mut impl Read, variable_count: usize) -> Result<FlatFileRead> {
    let mut time = [0_u8; std::mem::size_of::<f32>()];
    if !read_exact_or_closed(source, &mut time)? {
        return Ok(FlatFileRead::Closed);
    }

    let mut content = vec![0.0; variable_count];
    source.read_exact(bytemuck::cast_slice_mut(&mut content))?;

    let Some(stamp_us) = timestamp_f32_to_us(f32::from_ne_bytes(time))? else {
        return Ok(FlatFileRead::Closed);
    };

    Ok(FlatFileRead::Frame(DataFrame { stamp_us, content }))
}

fn read_flat_file_sentinel_f64(
    source: &mut impl Read,
    variable_count: usize,
    cache: &mut Vec<f64>,
) -> Result<FlatFileRead> {
    let byte_len = variable_count * std::mem::size_of::<f64>()
        + std::mem::size_of::<f64>()
        + 2 * std::mem::size_of::<u32>();
    let f64_len = byte_len / std::mem::size_of::<f64>();
    cache.resize(f64_len, 0.0);

    if !read_exact_or_closed(source, bytemuck::cast_slice_mut(cache))? {
        return Ok(FlatFileRead::Closed);
    }

    let buffer_ptr = cache.as_ptr() as *const u8;
    let data_end = std::mem::size_of::<u32>()
        + std::mem::size_of::<f64>()
        + variable_count * std::mem::size_of::<f64>();

    let start_marker = unsafe { *(buffer_ptr as *const u32) };
    let end_marker = unsafe { *(buffer_ptr.add(data_end) as *const u32) };

    if start_marker != end_marker {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "marker validation failed",
        ));
    }

    let time = unsafe {
        std::ptr::read_unaligned(buffer_ptr.add(std::mem::size_of::<u32>()) as *const f64)
    };
    let Some(stamp_us) = timestamp_f64_to_us(time)? else {
        return Ok(FlatFileRead::Closed);
    };

    let payload =
        unsafe { buffer_ptr.add(std::mem::size_of::<u32>() + std::mem::size_of::<f64>()) };
    let mut content = vec![0.0; variable_count];

    for (i, value) in content.iter_mut().enumerate() {
        let data = unsafe {
            std::ptr::read_unaligned(payload.add(i * std::mem::size_of::<f64>()) as *const f64)
        };
        *value = data as f32;
    }

    Ok(FlatFileRead::Frame(DataFrame { stamp_us, content }))
}

fn read_exact_or_closed(source: &mut impl Read, mut dest: &mut [u8]) -> Result<bool> {
    let original_len = dest.len();

    while !dest.is_empty() {
        match source.read(dest) {
            Ok(0) if dest.len() == original_len => return Ok(false),
            Ok(0) => return Err(Error::new(ErrorKind::UnexpectedEof, "partial frame")),
            Ok(n) => {
                dest = &mut dest[n..];
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }

    Ok(true)
}

fn timestamp_f32_to_us(timestamp_seconds: f32) -> Result<Option<u64>> {
    if !timestamp_seconds.is_finite() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("non-finite timestamp: {timestamp_seconds}"),
        ));
    }
    if timestamp_seconds < 0.0 {
        return Ok(None);
    }

    Ok(Some((timestamp_seconds * 1e6).round() as u64))
}

fn timestamp_f64_to_us(timestamp_seconds: f64) -> Result<Option<u64>> {
    if !timestamp_seconds.is_finite() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("non-finite timestamp: {timestamp_seconds}"),
        ));
    }
    if timestamp_seconds < 0.0 {
        return Ok(None);
    }

    Ok(Some((timestamp_seconds * 1e6).round() as u64))
}

fn f64_cache_for_format(format: FlatFileFormat) -> Vec<f64> {
    match format {
        FlatFileFormat::F32 { .. } => Vec::new(),
        FlatFileFormat::SentinelF64 { variable_count } => {
            let byte_len = variable_count * std::mem::size_of::<f64>()
                + std::mem::size_of::<f64>()
                + 2 * std::mem::size_of::<u32>();
            vec![0.0; byte_len / std::mem::size_of::<f64>()]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Cursor,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn flat_file_f32_reads_one_frame() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0.25f32.to_ne_bytes());
        bytes.extend_from_slice(bytemuck::cast_slice(&[2.0f32, 5.0, -10.5]));

        let result = read_flat_file_f32(&mut Cursor::new(bytes), 3).unwrap();

        let FlatFileRead::Frame(frame) = result else {
            panic!("expected frame");
        };
        assert_eq!(frame.stamp_us, 250_000);
        assert_eq!(frame.content, [2.0, 5.0, -10.5]);
    }

    #[test]
    fn flat_file_sentinel_f64_reads_one_frame_and_converts_payload() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3u32.to_ne_bytes());
        bytes.extend_from_slice(&2.25f64.to_ne_bytes());
        bytes.extend_from_slice(bytemuck::cast_slice(&[6.5f64, -7.25]));
        bytes.extend_from_slice(&3u32.to_ne_bytes());
        let mut cache = f64_cache_for_format(FlatFileFormat::SentinelF64 { variable_count: 2 });

        let result = read_flat_file_sentinel_f64(&mut Cursor::new(bytes), 2, &mut cache).unwrap();

        let FlatFileRead::Frame(frame) = result else {
            panic!("expected frame");
        };
        assert_eq!(frame.stamp_us, 2_250_000);
        assert_eq!(frame.content, [6.5, -7.25]);
    }

    #[test]
    fn flat_file_sentinel_f64_rejects_marker_mismatch() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3u32.to_ne_bytes());
        bytes.extend_from_slice(&2.25f64.to_ne_bytes());
        bytes.extend_from_slice(bytemuck::cast_slice(&[6.5f64, -7.25]));
        bytes.extend_from_slice(&4u32.to_ne_bytes());
        let mut cache = f64_cache_for_format(FlatFileFormat::SentinelF64 { variable_count: 2 });

        let err = read_flat_file_sentinel_f64(&mut Cursor::new(bytes), 2, &mut cache)
            .expect_err("marker mismatch should fail");

        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn flat_file_read_returns_closed_on_clean_eof() {
        let result = read_flat_file_f32(&mut Cursor::new(Vec::new()), 3).unwrap();

        assert!(matches!(result, FlatFileRead::Closed));
    }

    #[test]
    fn negative_flat_file_timestamp_closes_replay() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(-1.0f32).to_ne_bytes());
        bytes.extend_from_slice(bytemuck::cast_slice(&[2.0f32, 5.0, -10.5]));

        let result = read_flat_file_f32(&mut Cursor::new(bytes), 3).unwrap();

        assert!(matches!(result, FlatFileRead::Closed));
    }

    #[test]
    fn flat_file_replay_loops_after_eof() {
        let path = temp_file_path("loop_after_eof");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0.25f32.to_ne_bytes());
        bytes.extend_from_slice(bytemuck::cast_slice(&[2.0f32, 5.0]));
        std::fs::write(&path, bytes).unwrap();

        let mut replay =
            FlatFileReplay::open(&path, FlatFileFormat::F32 { variable_count: 2 }).unwrap();

        let FlatFileRead::Frame(first) = replay.read_next().unwrap() else {
            panic!("expected first frame");
        };
        let FlatFileRead::Frame(looped) = replay.read_next().unwrap() else {
            panic!("expected looped frame");
        };

        assert_eq!(first.stamp_us, looped.stamp_us);
        assert_eq!(first.content, looped.content);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn empty_flat_file_replay_does_not_spin() {
        let path = temp_file_path("empty_replay");
        std::fs::write(&path, []).unwrap();

        let mut replay =
            FlatFileReplay::open(&path, FlatFileFormat::F32 { variable_count: 2 }).unwrap();

        assert!(matches!(replay.read_next().unwrap(), FlatFileRead::Closed));

        std::fs::remove_file(path).unwrap();
    }

    fn temp_file_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("nypa_db_connector_{name}_{unique}.bin"))
    }
}
