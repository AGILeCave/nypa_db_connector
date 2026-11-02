use std::io::{Read, Result};

const TIMESTAMP_MASK: u64 = (1_u64 << 60) - 1;
const RECORDING_BIT: u64 = 1_u64 << 63;

/// Origin of a frame's content.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NypaDbFrameSource {
    /// A live timestep published by NYPA DB.
    LiveDb,
    /// A recorded or replayed timestep published by NYPA DB.
    RecordedDb,
    /// A timestep read from the connector's flat-file fallback.
    FlatFileFallback,
    /// No content has been received.
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

        self.source = if (time_stamp & RECORDING_BIT) != 0 {
            NypaDbFrameSource::RecordedDb
        } else {
            NypaDbFrameSource::LiveDb
        };
        self.content.resize(var_count as usize, 0.0);
        source.read_exact(bytemuck::cast_slice_mut(&mut self.content))?;
        self.stamp_us = time_stamp & TIMESTAMP_MASK;

        Ok(())
    }

    pub fn copy_from(&mut self, source: &Self) {
        self.stamp_us = source.stamp_us;
        self.source = source.source;
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
        assert_eq!(frame.content, [1.5]);

        frame.update_from(&mut cursor).unwrap();
        assert_eq!(frame.stamp_us, 43);
        assert_eq!(frame.source, NypaDbFrameSource::RecordedDb);
        assert_eq!(frame.content, [2.5]);
    }

    #[test]
    fn copy_preserves_source() {
        let source = DataFrame {
            stamp_us: 12,
            source: NypaDbFrameSource::RecordedDb,
            content: vec![3.0],
        };
        let mut copy = DataFrame::default();

        copy.copy_from(&source);

        assert_eq!(copy.source, NypaDbFrameSource::RecordedDb);
    }
}
