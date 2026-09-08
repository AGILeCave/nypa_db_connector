# nypa_db_connector
A Bevy plugin to connect to a NYPA db

```rust
use nypa_db_connector::{FlatFileFormat, NypaDbClientPlugin};

// Default: discover and connect to NYPA DB publisher sockets.
app.add_plugins(NypaDbClientPlugin::default());

// Debug replay: read one f32 timestep from a flat file per Bevy update.
app.add_plugins(NypaDbClientPlugin::flat_file(
    "capture.bin",
    FlatFileFormat::F32 { variable_count: 128 },
));

// Debug replay: read sentinel-wrapped f64 timesteps and convert payloads to f32.
app.add_plugins(NypaDbClientPlugin::flat_file(
    "capture_f64.bin",
    FlatFileFormat::SentinelF64 { variable_count: 128 },
));
```
