use std::io::{Read, Result};

const TIMESTAMP_MASK: u64 = (1_u64 << 60) - 1;
const RECORDING_BIT: u64 = 1_u64 << 63;
const STATUS_SHIFT: u32 = 60;

/// Availability of a frame published by NYPA DB.
///
/// Non-data statuses are heartbeats. They may repeat the last complete payload, so consumers
/// should use this status rather than payload length to determine whether new data is available.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum NypaDbFrameStatus {
    /// A complete data frame is present.
    #[default]
    Data = 0,
    /// The server is waiting for every configured source to provide its first frame.
    WaitingForSources = 1,
    /// Sources have data, but no common timestamp can currently be assembled.
    SourcesUnaligned = 2,
    /// The requested live or replay time is outside the available window.
    TimeUnavailable = 3,
    /// A selected recording could not be opened or is not ready for replay.
    RecordingUnavailable = 4,
    /// Publication was intentionally stopped.
    Stopped = 5,
    /// A configured source disconnected.
    SourceDisconnected = 6,
    /// The server could not classify an internal publication failure more precisely.
    InternalError = 7,
}

impl NypaDbFrameStatus {
    fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Data,
            1 => Self::WaitingForSources,
            2 => Self::SourcesUnaligned,
            3 => Self::TimeUnavailable,
            4 => Self::RecordingUnavailable,
            5 => Self::Stopped,
            6 => Self::SourceDisconnected,
            _ => Self::InternalError,
        }
    }
}

/// Origin of a frame's content.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NypaDbFrameSource {
    /// A live timestep published by NYPA DB.
    LiveDb,
    /// A recorded or replayed timestep published by NYPA DB.
    RecordedDb,
    /// A timestep read from the connector's flat-file fallback.
    FlatFileFallback,
    /// No current content is available.
    #[default]
    Idle,
}

/// One frame read from a NYPA DB publisher socket.
#[derive(Clone, Debug, Default)]
pub struct DataFrame {
    /// Simulation timestamp in microseconds.
    pub stamp_us: u64,
    /// Origin of this frame's content.
    pub source: NypaDbFrameSource,
    /// Whether this is data or an availability heartbeat.
    pub status: NypaDbFrameStatus,
    /// Published quantities for this stream.
    pub content: Vec<f32>,
}

impl DataFrame {
    /// Read the next complete frame from a publisher socket, reusing this frame's allocation.
    pub fn update_from(&mut self, source: &mut impl Read) -> Result<()> {
        let mut var_count = 0u64;
        source.read_exact(bytemuck::bytes_of_mut(&mut var_count))?;

        let mut time_stamp = 0u64;
        source.read_exact(bytemuck::bytes_of_mut(&mut time_stamp))?;

        self.status = NypaDbFrameStatus::from_code(((time_stamp >> STATUS_SHIFT) & 0b111) as u8);
        self.source = match self.status {
            NypaDbFrameStatus::Data if (time_stamp & RECORDING_BIT) != 0 => {
                NypaDbFrameSource::RecordedDb
            }
            NypaDbFrameStatus::Data => NypaDbFrameSource::LiveDb,
            _ => NypaDbFrameSource::Idle,
        };
        self.content.resize(var_count as usize, 0.0);
        source.read_exact(bytemuck::cast_slice_mut(&mut self.content))?;
        self.stamp_us = time_stamp & TIMESTAMP_MASK;

        Ok(())
    }

    pub fn copy_from(&mut self, source: &Self) {
        self.stamp_us = source.stamp_us;
        self.source = source.source;
        self.status = source.status;
        self.content.resize(source.content.len(), 0.0);
        self.content.copy_from_slice(&source.content);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn decodes_live_and_recorded_source_indicators() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&1_u64.to_ne_bytes());
        wire.extend_from_slice(&42_u64.to_ne_bytes());
        wire.extend_from_slice(&1.5_f32.to_ne_bytes());
        wire.extend_from_slice(&1_u64.to_ne_bytes());
        wire.extend_from_slice(&(RECORDING_BIT | 43).to_ne_bytes());
        wire.extend_from_slice(&2.5_f32.to_ne_bytes());
        let mut cursor = Cursor::new(wire);
        let mut frame = DataFrame::default();

        assert_eq!(frame.source, NypaDbFrameSource::Idle);

        frame.update_from(&mut cursor).unwrap();
        assert_eq!(frame.stamp_us, 42);
        assert_eq!(frame.source, NypaDbFrameSource::LiveDb);
        assert_eq!(frame.status, NypaDbFrameStatus::Data);
        assert_eq!(frame.content, [1.5]);

        frame.update_from(&mut cursor).unwrap();
        assert_eq!(frame.stamp_us, 43);
        assert_eq!(frame.source, NypaDbFrameSource::RecordedDb);
        assert_eq!(frame.status, NypaDbFrameStatus::Data);
        assert_eq!(frame.content, [2.5]);
    }

    #[test]
    fn decodes_status_heartbeat_with_repeated_content() {
        let packed =
            (NypaDbFrameStatus::SourcesUnaligned as u64) << STATUS_SHIFT | RECORDING_BIT | 99;
        let mut wire = Vec::new();
        wire.extend_from_slice(&1_u64.to_ne_bytes());
        wire.extend_from_slice(&packed.to_ne_bytes());
        wire.extend_from_slice(&3.5_f32.to_ne_bytes());
        let mut frame = DataFrame::default();

        frame.update_from(&mut Cursor::new(wire)).unwrap();

        assert_eq!(frame.stamp_us, 99);
        assert_eq!(frame.source, NypaDbFrameSource::Idle);
        assert_eq!(frame.status, NypaDbFrameStatus::SourcesUnaligned);
        assert_eq!(frame.content, [3.5]);
    }

    #[test]
    fn decodes_every_status_code() {
        let statuses = [
            NypaDbFrameStatus::Data,
            NypaDbFrameStatus::WaitingForSources,
            NypaDbFrameStatus::SourcesUnaligned,
            NypaDbFrameStatus::TimeUnavailable,
            NypaDbFrameStatus::RecordingUnavailable,
            NypaDbFrameStatus::Stopped,
            NypaDbFrameStatus::SourceDisconnected,
            NypaDbFrameStatus::InternalError,
        ];

        for expected in statuses {
            let packed = (expected as u64) << STATUS_SHIFT;
            let mut wire = Vec::new();
            wire.extend_from_slice(&0_u64.to_ne_bytes());
            wire.extend_from_slice(&packed.to_ne_bytes());
            let mut frame = DataFrame::default();

            frame.update_from(&mut Cursor::new(wire)).unwrap();

            assert_eq!(frame.status, expected);
        }
    }

    #[test]
    fn copy_preserves_frame_metadata() {
        let source = DataFrame {
            stamp_us: 12,
            source: NypaDbFrameSource::Idle,
            status: NypaDbFrameStatus::SourceDisconnected,
            content: vec![3.0],
        };
        let mut copy = DataFrame::default();

        copy.copy_from(&source);

        assert_eq!(copy.source, NypaDbFrameSource::Idle);
        assert_eq!(copy.status, NypaDbFrameStatus::SourceDisconnected);
    }
}
