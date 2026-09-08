//! Bare-bones Bevy ECS client for NYPA DB publisher streams.
//!
//! This uses only `bevy_app` and `bevy_ecs` from Bevy. The NYPA DB publisher protocol helpers are
//! included so the crate can be used without depending on the `nypa_db` Rust crate API.
//!
//! By default, the plugin discovers active NYPA DB publisher sockets, spawns one blocking reader
//! thread per stream, keeps one frame buffer per stream, and triggers `NypaDbFramesChanged` whenever
//! a stream buffer is updated. For testing and debugging, the plugin can also replay a flat file one
//! timestep per Bevy update.

mod discovery;
mod flat_file;
mod frame;
mod plugin;
mod socket;

pub use discovery::find_publisher_sockets;
pub use flat_file::FlatFileFormat;
pub use frame::DataFrame;
pub use plugin::{
    NYPADBLink, NypaDbClientPlugin, NypaDbClientSource, NypaDbFramesChanged, PerStreamContent,
};
