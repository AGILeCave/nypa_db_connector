//! Bare-bones Bevy ECS client for NYPA DB publisher streams.
//!
//! This uses only `bevy_app` and `bevy_ecs` from Bevy. The NYPA DB publisher protocol helpers are
//! included so the crate can be used without depending on the `nypa_db` Rust crate API.
//!
//! The plugin first tries to connect to active NYPA DB publisher sockets. If none can be connected
//! during startup, it can replay an optional prerecorded file. It keeps one frame buffer per stream
//! and triggers `NypaDbFramesChanged` whenever a stream buffer is updated.

mod control;
mod discovery;
mod faults;
mod flat_file;
mod frame;
mod plugin;
mod socket;

pub use control::{
    NypaDbControl, NypaDbControlError, NypaDbControlOperation, NypaDbControlPlugin,
    NypaDbControlQueueError, NypaDbSetVariableOptions, NypaDbStartRegion, NypaDbVariable,
    NypaDbVariableReset, NypaDbVariableSemantic, NypaDbVariableSet, NypaDbVariableStrategy,
    NypaDbVariableStream, NypaDbVariableUpdate, NypaDbVariables, NypaDbVariablesChanged,
    NypaDbVariablesSet,
};
pub use discovery::find_publisher_sockets;
pub use faults::{
    FaultArea, FaultClearRequested, FaultImpact, FaultRequested, FaultSpawnSource, FaultThrow,
    FaultVariable, Faulted, NypaDbFaultPlugin, NypaDbFaultTrigger,
};
pub use flat_file::FlatFileFormat;
pub use frame::DataFrame;
pub use plugin::{
    NYPADBLink, NypaDbClientPlugin, NypaDbConnectionMode, NypaDbConnectionModeChanged,
    NypaDbFramesChanged, NypaDbPrerecordedSource, PerStreamContent,
};
