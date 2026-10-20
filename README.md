# nypa_db_connector
A Bevy plugin to connect to a NYPA db

```rust
use nypa_db_connector::{
    FlatFileFormat, NypaDbClientPlugin, NypaDbPrerecordedSource,
};

// Connect to live sockets, with no prerecorded startup fallback.
app.add_plugins(NypaDbClientPlugin::new(None));

// Prefer live sockets. If none can be connected during startup, read one f32
// timestep from this prerecorded file per Bevy update.
app.add_plugins(NypaDbClientPlugin::new(Some(
    NypaDbPrerecordedSource::new(
        "capture.bin",
        FlatFileFormat::F32 { variable_count: 128 },
    ),
)));

// Faster replay: emit one timestep, skip three, then emit the next.
app.add_plugins(NypaDbClientPlugin::new(Some(
    NypaDbPrerecordedSource::new(
        "capture.bin",
        FlatFileFormat::F32 { variable_count: 128 },
    )
    .with_skip(3),
)));

// Debug replay: read sentinel-wrapped f64 timesteps and convert payloads to f32.
app.add_plugins(NypaDbClientPlugin::new(Some(
    NypaDbPrerecordedSource::new(
        "capture_f64.bin",
        FlatFileFormat::SentinelF64 { variable_count: 128 },
    ),
)));
```

The current source is queryable through `NYPADBLink::mode()`. Observe
`NypaDbConnectionModeChanged` to update UI or graphics whenever it changes. Live sockets always
take priority. A prerecorded source is considered only during startup; after startup, losing a live
connection changes the mode to `Idle`, never back to `Prerecorded`.

```rust
use bevy::prelude::*;
use nypa_db_connector::{
    NYPADBLink, NypaDbConnectionModeChanged,
};

fn current_mode(link: Res<NYPADBLink>) {
    println!("NYPA DB mode: {:?}", link.mode());
}

fn mode_changed(change: On<NypaDbConnectionModeChanged>) {
    println!("NYPA DB mode: {:?} -> {:?}", change.previous, change.current);
}

app.add_observer(mode_changed);
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
then trigger a throw at a target fault area. A successful request adds `FaultRequested`; `Faulted`
is added and removed only to reflect the DB's reported value. Faults remain active by default until
you add `FaultClearRequested` to request that their variable be set back to zero. Configure an
automatic clear explicitly with `FaultArea::with_auto_reset_after`.

Upgrading from commit `3abdaae`? See the [fault migration guide](FAULTS_MIGRATION.md).

The moving throw entity has a public `FaultThrow` component; observe that component being added to
attach your own model, lights, particles, or audio. Content attached as a child follows the
library-managed arc and is despawned with the throw on impact.

```rust
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use nypa_db_connector::{
    FaultArea, FaultClearRequested, FaultSpawnSource, FaultThrow, NypaDbControlPlugin,
    NypaDbFaultPlugin, NypaDbFaultTrigger, NypaDbSetVariableOptions,
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

// Call this later, once the fault is active.
fn clear_fault(commands: &mut Commands, fault: Entity) {
    commands.entity(fault).insert(FaultClearRequested);
}
```
