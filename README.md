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

// Faster replay: emit one timestep, skip three, then emit the next.
app.add_plugins(NypaDbClientPlugin::flat_file_with_skip(
    "capture.bin",
    FlatFileFormat::F32 { variable_count: 128 },
    3,
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
use nypa_db_connector::{
    NypaDbControl, NypaDbControlPlugin, NypaDbSetVariableOptions, NypaDbVariableUpdate,
    NypaDbVariables,
};

app.add_plugins(NypaDbControlPlugin::default());

fn load_variables(control: Res<NypaDbControl>) {
    let _ = control.request_variables(0);
}

fn change_gain(control: Res<NypaDbControl>) {
    let _ = control.set_variable(0, "example_gain", 2.5);
}

fn change_operating_state(control: Res<NypaDbControl>) {
    // Tuples are accepted for concise, inline batches.
    let _ = control.set_variables(
        0,
        [("state_breaker", 1.0), ("state_setpoint", 0.75)],
    );

    // The named type is convenient when updates are assembled dynamically.
    let updates = vec![NypaDbVariableUpdate {
        name: "state_setpoint".into(),
        value: 0.5,
    }];
    let _ = control.set_variables(0, updates);
}

fn change_gain_and_record(control: Res<NypaDbControl>) {
    let _ = control.set_variable_with_options(
        0,
        "example_gain",
        2.5,
        NypaDbSetVariableOptions::start_region("gain step"),
    );
}

fn draw_variables(variables: Res<NypaDbVariables>) {
    if let Some(gain) = variables.variable(0, "example_gain") {
        // Render a slider/toggle from gain.semantic, gain.min, gain.max, and gain.value.
    }
}
```

Fault behavior and variable activation are available through `NypaDbFaultPlugin`. Add `FaultArea`
to world entities that can be faulted, add one `FaultSpawnSource` where fault throws should start,
then trigger a throw at a target fault area. The moving throw entity has a public `FaultThrow`
component; observe that component being added to attach your own model, lights, particles, or audio.
Content attached as a child follows the library-managed arc and is despawned with the throw on
impact.

```rust
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use nypa_db_connector::{
    FaultArea, FaultSpawnSource, FaultThrow, NypaDbControlPlugin, NypaDbFaultPlugin,
    NypaDbFaultTrigger, NypaDbSetVariableOptions,
};

app.add_plugins((NypaDbControlPlugin::default(), NypaDbFaultPlugin));
app.add_observer(attach_fault_throw_visual);

fn attach_fault_throw_visual(
    add: On<Add, FaultThrow>,
    mut commands: Commands,
    assets: Res<AssetServer>,
) {
    let scene = assets.load(GltfAssetLabel::Scene(0).from_asset("models/my_fault.glb"));
    commands.entity(add.entity).with_children(|parent| {
        parent.spawn((
            SceneRoot(scene),
            Transform::from_scale(Vec3::splat(1.2)),
        ));
    });
}

commands.spawn((FaultSpawnSource, Transform::from_xyz(0.0, 1.5, 0.0)));

let fault = commands
    .spawn((
        FaultArea::variable_with_options(
            0,
            "example_fault",
            NypaDbSetVariableOptions::start_region("example fault"),
        ),
        Transform::from_xyz(2.0, 0.0, 0.0),
    ))
    .id();

commands.trigger(NypaDbFaultTrigger { target: fault });
```
