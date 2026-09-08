use std::io::{Read, Result};

const TIMESTAMP_MASK: u64 = (1_u64 << 60) - 1;

/// One frame read from a NYPA DB publisher socket.
#[derive(Clone, Debug, Default)]
pub struct DataFrame {
    /// Simulation timestamp in microseconds.
    pub stamp_us: u64,
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

        self.content.resize(var_count as usize, 0.0);
        source.read_exact(bytemuck::cast_slice_mut(&mut self.content))?;
        self.stamp_us = time_stamp & TIMESTAMP_MASK;

        Ok(())
    }

    pub fn copy_from(&mut self, source: &Self) {
        self.stamp_us = source.stamp_us;
        self.content.resize(source.content.len(), 0.0);
        self.content.copy_from_slice(&source.content);
    }
}
