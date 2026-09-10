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

Variable command/control is a separate plugin. It uses NYPA DB's JSON-RPC CnC endpoint and queues
requests on a worker thread so Bevy systems do not block on HTTP.

```rust
use nypa_db_connector::{NypaDbControl, NypaDbControlPlugin, NypaDbVariables};

app.add_plugins(NypaDbControlPlugin::default());

fn load_variables(control: Res<NypaDbControl>) {
    let _ = control.request_variables(0);
}

fn change_gain(control: Res<NypaDbControl>) {
    let _ = control.set_variable(0, "example_gain", 2.5);
}

fn draw_variables(variables: Res<NypaDbVariables>) {
    if let Some(gain) = variables.variable(0, "example_gain") {
        // Render a slider/toggle from gain.semantic, gain.min, gain.max, and gain.value.
    }
}
```
