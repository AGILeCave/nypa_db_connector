use std::{collections::HashSet, time::Duration};

use bevy::{
    asset::{embedded_asset, load_embedded_asset},
    color::palettes::tailwind::AMBER_200,
    light::{NotShadowCaster, NotShadowReceiver},
    prelude::*,
};

use crate::{
    NypaDbControl, NypaDbControlError, NypaDbControlOperation, NypaDbSetVariableOptions,
    NypaDbVariableSet, NypaDbVariables,
};

const FAULT_SPAWNER_SECONDS: f32 = 1.0;
const FAULT_SPAWNER_ARC_HEIGHT: f32 = 0.15;
const FAULT_SPAWNER_REVOLUTIONS_PER_SECOND: f32 = 1.0;
const FAULT_POLL_SECONDS: f32 = 1.0;
const SPARK_COUNT: usize = 30;
const SPARK_SIZE: f32 = 0.003;
const SPARK_MIN_LIFETIME: f32 = 1.0;
const SPARK_MAX_LIFETIME: f32 = 3.0;
const SPARK_SPEED: f32 = 0.55;
const SPARK_UPWARD_SPEED: f32 = 0.45;
const SPARK_GRAVITY: f32 = 1.25;
const IMPACT_LIGHT_SECONDS: f32 = 1.0;
const IMPACT_LIGHT_INTENSITY: f32 = 420_000.0;
const IMPACT_LIGHT_RANGE: f32 = 3.0;
const IMPACT_FLARE_SIZE: f32 = 0.32;
const IMPACT_FLARE_SECONDS: f32 = 0.75;

pub struct NypaDbFaultPlugin;

impl Plugin for NypaDbFaultPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "assets/FaultOption.glb");
        embedded_asset!(app, "assets/FaultActive.glb");
        embedded_asset!(app, "assets/flare.png");

        app.insert_resource(FaultVariablePollTimer(Timer::from_seconds(
            FAULT_POLL_SECONDS,
            TimerMode::Repeating,
        )))
        .add_observer(trigger_fault_throw)
        .add_observer(handle_fault_clear_requested)
        .add_observer(handle_fault_variable_set)
        .add_observer(handle_fault_control_error)
        .add_systems(
            Update,
            (
                attach_fault_markers,
                sync_faulted_from_variables,
                poll_fault_variables,
                update_fault_marker_visibility,
                animate_fault_senders,
                auto_reset_faults,
                spawn_fault_impacts,
                update_fault_impacts,
                rotate_option_markers,
            )
                .chain(),
        );
    }
}

#[derive(Component, Clone, Debug)]
pub struct FaultArea {
    pub variable: Option<FaultVariable>,
    pub set_variable_options: NypaDbSetVariableOptions,
    pub throw_speed: Option<f32>,
    /// How long a connector-requested fault remains active before requesting a reset.
    ///
    /// Defaults to `None`, leaving the fault active until [`FaultClearRequested`] is added.
    pub auto_reset_after: Option<Duration>,
}

impl FaultArea {
    pub fn new(variable: Option<FaultVariable>) -> Self {
        Self {
            variable,
            set_variable_options: NypaDbSetVariableOptions::default(),
            throw_speed: None,
            auto_reset_after: None,
        }
    }

    pub fn with_options(
        variable: Option<FaultVariable>,
        set_variable_options: NypaDbSetVariableOptions,
    ) -> Self {
        Self {
            variable,
            set_variable_options,
            throw_speed: None,
            auto_reset_after: None,
        }
    }

    pub fn variable(stream_id: usize, name: impl Into<String>) -> Self {
        Self::new(Some(FaultVariable {
            stream_id,
            name: name.into(),
        }))
    }

    pub fn variable_with_options(
        stream_id: usize,
        name: impl Into<String>,
        set_variable_options: NypaDbSetVariableOptions,
    ) -> Self {
        Self::with_options(
            Some(FaultVariable {
                stream_id,
                name: name.into(),
            }),
            set_variable_options,
        )
    }

    pub fn with_auto_reset_after(mut self, timeout: Duration) -> Self {
        self.auto_reset_after = Some(timeout);
        self
    }
}

impl Default for FaultArea {
    fn default() -> Self {
        Self::new(None)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultVariable {
    pub stream_id: usize,
    pub name: String,
}

#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FaultSpawnSource;

/// Indicates that the DB currently reports this fault as active.
///
/// This component is maintained by [`NypaDbFaultPlugin`]. Treat it as read-only state. Activate a
/// fault through [`NypaDbFaultTrigger`] and clear it by adding [`FaultClearRequested`].
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Faulted;

/// Indicates that this fault has been requested but not yet confirmed by the DB.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FaultRequested;

/// Requests that an active fault's DB variable be set back to zero.
///
/// The connector removes this component when the request succeeds or fails. [`Faulted`] is removed
/// separately, only after the DB reports that the fault is no longer active.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FaultClearRequested;

#[derive(Clone, Copy, Debug, Event)]
pub struct NypaDbFaultTrigger {
    pub target: Entity,
}

/// Marks the moving root entity for a fault throw.
///
/// Observe `Add<FaultThrow>` to attach application-owned models, lights, particles, or audio as
/// children. The connector controls this entity's transform and despawns it on impact.
#[derive(Component, Clone, Copy, Debug)]
pub struct FaultThrow {
    pub target: Entity,
}

#[derive(Component)]
struct FaultAreaMarkers {
    option_marker: Entity,
    active_marker: Entity,
}

#[derive(Component)]
struct FaultOptionMarker;

#[derive(Component)]
struct FaultThrowAnimation {
    fault: Entity,
    start: Vec3,
    end: Vec3,
    elapsed: f32,
    duration: f32,
}

#[derive(Component)]
struct FaultAutoReset(Timer);

#[derive(Resource)]
struct FaultVariablePollTimer(Timer);

#[derive(Component)]
struct FaultImpactSpark {
    velocity: Vec3,
    age: f32,
    lifetime: f32,
    initial_scale: Vec3,
}

#[derive(Component)]
struct FaultImpactLight {
    age: f32,
    duration: f32,
    base_intensity: f32,
}

#[derive(Component)]
struct FaultImpactFlare {
    age: f32,
    duration: f32,
    material: Handle<StandardMaterial>,
}

type SparkQueryFilter = (Without<FaultImpactLight>, Without<FaultImpactFlare>);
type ImpactLightQueryFilter = (Without<FaultImpactSpark>, Without<FaultImpactFlare>);
type FlareQueryFilter = (Without<FaultImpactSpark>, Without<FaultImpactLight>);
type UnmarkedFaultAreaFilter = (With<FaultArea>, Without<FaultAreaMarkers>);
type FaultTriggerQueryItem<'a> = (
    Entity,
    Has<Faulted>,
    Has<FaultRequested>,
    &'a GlobalTransform,
    &'a FaultArea,
);

fn trigger_fault_throw(
    trigger: On<NypaDbFaultTrigger>,
    mut commands: Commands,
    control: Option<Res<NypaDbControl>>,
    sources: Query<&GlobalTransform, With<FaultSpawnSource>>,
    faults: Query<FaultTriggerQueryItem<'_>>,
    throws: Query<&FaultThrowAnimation>,
) {
    let target = trigger.event().target;
    let Ok((fault_entity, is_faulted, is_requested, fault_transform, fault_area)) =
        faults.get(target)
    else {
        warn!("Cannot trigger NYPA DB fault: target entity is not a FaultArea");
        return;
    };

    if is_faulted || is_requested || throws.iter().any(|throw| throw.fault == target) {
        return;
    }

    if fault_area.variable.is_none() {
        warn!("Cannot trigger NYPA DB fault: FaultArea does not have a variable");
        return;
    }

    if control.is_none() {
        warn!("Cannot trigger NYPA DB fault: NypaDbControl is not available");
        return;
    }

    let Ok(source_transform) = sources.single() else {
        warn!("Cannot trigger NYPA DB fault: expected exactly one FaultSpawnSource");
        return;
    };

    let duration = fault_area.throw_speed.unwrap_or(FAULT_SPAWNER_SECONDS);

    if duration <= 0.0 {
        request_fault_activation(&mut commands, control.as_deref(), fault_entity, fault_area);
        return;
    }

    spawn_fault_sender(
        &mut commands,
        target,
        source_transform.translation(),
        fault_transform.translation(),
        duration,
    );
}

fn attach_fault_markers(
    mut commands: Commands,
    candidates: Query<(Entity, Has<Faulted>), UnmarkedFaultAreaFilter>,
) {
    for (entity, is_faulted) in &candidates {
        let option_marker = commands
            .spawn((
                Transform::default(),
                if is_faulted {
                    Visibility::Hidden
                } else {
                    Visibility::Visible
                },
                ChildOf(entity),
                FaultOptionMarker,
            ))
            .id();

        let active_marker = commands
            .spawn((
                Transform::default(),
                if is_faulted {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                },
                ChildOf(entity),
            ))
            .id();

        commands.entity(entity).insert(FaultAreaMarkers {
            option_marker,
            active_marker,
        });
    }
}

fn poll_fault_variables(
    time: Res<Time>,
    mut timer: ResMut<FaultVariablePollTimer>,
    control: Option<Res<NypaDbControl>>,
    fault_areas: Query<&FaultArea>,
) {
    timer.0.tick(time.delta());

    if !timer.0.just_finished() {
        return;
    }

    let Some(control) = control else {
        return;
    };

    let mut stream_ids = HashSet::new();
    for area in &fault_areas {
        let Some(variable) = &area.variable else {
            continue;
        };

        if stream_ids.insert(variable.stream_id) {
            let _ = control.request_variables(variable.stream_id);
        }
    }
}

fn sync_faulted_from_variables(
    mut commands: Commands,
    variables: Option<Res<NypaDbVariables>>,
    fault_areas: Query<(Entity, &FaultArea, Has<Faulted>, Has<FaultRequested>)>,
) {
    let Some(variables) = variables else {
        return;
    };

    for (entity, area, is_faulted, is_requested) in &fault_areas {
        let Some(variable) = &area.variable else {
            continue;
        };

        let Some(value) = variables
            .variable(variable.stream_id, &variable.name)
            .map(|variable| variable.value)
        else {
            continue;
        };

        let should_be_faulted = value >= 0.5;

        match (is_faulted, should_be_faulted) {
            (false, true) => {
                let mut entity_commands = commands.entity(entity);
                entity_commands.insert(Faulted);

                if is_requested {
                    entity_commands.remove::<FaultRequested>();
                    if let Some(timeout) = area.auto_reset_after {
                        entity_commands
                            .insert(FaultAutoReset(Timer::new(timeout, TimerMode::Once)));
                    }
                }
            }
            (true, false) => {
                commands
                    .entity(entity)
                    .remove::<(Faulted, FaultAutoReset, FaultClearRequested)>();
            }
            _ => {}
        }
    }
}

fn handle_fault_variable_set(
    update: On<NypaDbVariableSet>,
    mut commands: Commands,
    fault_areas: Query<(
        Entity,
        &FaultArea,
        Has<FaultRequested>,
        Has<FaultClearRequested>,
    )>,
) {
    let update = update.event();

    for (entity, area, activation_requested, clear_requested) in &fault_areas {
        let Some(variable) = &area.variable else {
            continue;
        };
        if variable.stream_id != update.stream_id || variable.name != update.name {
            continue;
        }

        if activation_requested && update.value < 0.5 {
            commands.entity(entity).remove::<FaultRequested>();
        }

        if clear_requested {
            commands.entity(entity).remove::<FaultClearRequested>();
        }
    }
}

fn handle_fault_clear_requested(
    request: On<Add, FaultClearRequested>,
    mut commands: Commands,
    control: Option<Res<NypaDbControl>>,
    fault_areas: Query<(&FaultArea, Has<Faulted>)>,
) {
    let entity = request.entity;
    let Ok((area, is_faulted)) = fault_areas.get(entity) else {
        commands.entity(entity).remove::<FaultClearRequested>();
        return;
    };
    let Some(variable) = area.variable.as_ref().filter(|_| is_faulted) else {
        commands.entity(entity).remove::<FaultClearRequested>();
        return;
    };
    let Some(control) = control.as_deref() else {
        commands.entity(entity).remove::<FaultClearRequested>();
        return;
    };

    if control
        .set_variable(variable.stream_id, &variable.name, 0.0)
        .is_err()
    {
        commands.entity(entity).remove::<FaultClearRequested>();
    }
}

fn handle_fault_control_error(
    error: On<NypaDbControlError>,
    mut commands: Commands,
    fault_areas: Query<(
        Entity,
        &FaultArea,
        Has<FaultRequested>,
        Has<FaultClearRequested>,
    )>,
) {
    let NypaDbControlOperation::SetVariable {
        stream_id,
        name,
        value,
        ..
    } = &error.event().operation
    else {
        return;
    };

    for (entity, area, activation_requested, clear_requested) in &fault_areas {
        let Some(variable) = &area.variable else {
            continue;
        };
        if variable.stream_id != *stream_id || variable.name != *name {
            continue;
        }

        if activation_requested && *value >= 0.5 {
            commands.entity(entity).remove::<FaultRequested>();
        }

        if clear_requested && *value < 0.5 {
            commands.entity(entity).remove::<FaultClearRequested>();
        }
    }
}

fn update_fault_marker_visibility(
    faulted: Query<&FaultAreaMarkers, Added<Faulted>>,
    mut removed_faulted: RemovedComponents<Faulted>,
    fault_locations: Query<&FaultAreaMarkers>,
    mut visibility: Query<&mut Visibility>,
) {
    for markers in &faulted {
        set_fault_marker_visibility(markers, true, &mut visibility);
    }

    for entity in removed_faulted.read() {
        let Ok(markers) = fault_locations.get(entity) else {
            continue;
        };

        set_fault_marker_visibility(markers, false, &mut visibility);
    }
}

fn animate_fault_senders(
    mut commands: Commands,
    time: Res<Time>,
    control: Option<Res<NypaDbControl>>,
    areas: Query<&FaultArea>,
    mut senders: Query<(Entity, &mut Transform, &mut FaultThrowAnimation)>,
) {
    for (entity, mut transform, mut sender) in &mut senders {
        sender.elapsed += time.delta_secs();
        let t = (sender.elapsed / sender.duration).clamp(0.0, 1.0);

        transform.translation = arc_position(sender.start, sender.end, t);
        transform.rotation = Quat::from_rotation_y(
            sender.elapsed * std::f32::consts::TAU * FAULT_SPAWNER_REVOLUTIONS_PER_SECOND,
        );

        if t >= 1.0 {
            commands.entity(entity).despawn();

            if let Ok(area) = areas.get(sender.fault) {
                request_fault_activation(&mut commands, control.as_deref(), sender.fault, area);
            }
        }
    }
}

fn auto_reset_faults(
    mut commands: Commands,
    time: Res<Time>,
    mut faults: Query<(Entity, &mut FaultAutoReset), With<Faulted>>,
) {
    for (entity, mut reset) in &mut faults {
        reset.0.tick(time.delta());
        if !reset.0.just_finished() {
            continue;
        }

        commands
            .entity(entity)
            .remove::<FaultAutoReset>()
            .insert(FaultClearRequested);
    }
}

fn request_fault_activation(
    commands: &mut Commands,
    control: Option<&NypaDbControl>,
    fault: Entity,
    area: &FaultArea,
) {
    let Some(variable) = &area.variable else {
        return;
    };
    let Some(control) = control else {
        return;
    };

    if control
        .set_variable_with_options(
            variable.stream_id,
            &variable.name,
            1.0,
            area.set_variable_options.clone(),
        )
        .is_ok()
    {
        commands.entity(fault).insert(FaultRequested);
    }
}

fn spawn_fault_impacts(
    mut commands: Commands,
    server: Res<AssetServer>,
    faulted: Query<&GlobalTransform, (With<FaultArea>, Added<Faulted>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for transform in &faulted {
        //commands.entity(entity).insert(FaultImpactSpawned);

        spawn_fault_impact(
            &mut commands,
            &server,
            &mut meshes,
            &mut materials,
            transform.translation(),
        );
    }
}

fn update_fault_impacts(
    mut commands: Commands,
    time: Res<Time>,
    mut sparks: Query<(Entity, &mut Transform, &mut FaultImpactSpark), SparkQueryFilter>,
    mut lights: Query<(Entity, &mut PointLight, &mut FaultImpactLight), ImpactLightQueryFilter>,
    mut flares: Query<(Entity, &mut Transform, &mut FaultImpactFlare), FlareQueryFilter>,
    cameras: Query<&GlobalTransform, With<Camera3d>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let delta = time.delta_secs();
    let camera_position = cameras.iter().next().map(GlobalTransform::translation);

    for (entity, mut transform, mut spark) in &mut sparks {
        spark.age += delta;

        if spark.age >= spark.lifetime {
            commands.entity(entity).despawn();
            continue;
        }

        spark.velocity += Vec3::NEG_Y * SPARK_GRAVITY * delta;
        transform.translation += spark.velocity * delta;

        let age_t = (spark.age / spark.lifetime).clamp(0.0, 1.0);
        transform.scale = spark.initial_scale * (1.0 - age_t);
    }

    for (entity, mut light, mut impact_light) in &mut lights {
        impact_light.age += delta;

        if impact_light.age >= impact_light.duration {
            commands.entity(entity).despawn();
            continue;
        }

        let t = (impact_light.age / impact_light.duration).clamp(0.0, 1.0);
        let fade = 1.0 - t;
        let flicker = random_between(0.72, 1.18);
        light.intensity = impact_light.base_intensity * fade * flicker;
    }

    for (entity, mut transform, mut flare) in &mut flares {
        flare.age += delta;

        if flare.age >= flare.duration {
            commands.entity(entity).despawn();
            continue;
        }

        let t = (flare.age / flare.duration).clamp(0.0, 1.0);
        let fade = 1.0 - t;
        transform.scale = Vec3::splat(1.0 + t * 1.35);

        if let Some(camera_position) = camera_position {
            transform.look_at(camera_position, Vec3::Y);
        }

        if let Some(material) = materials.get_mut(&flare.material) {
            material.base_color = Color::srgba(0.35, 0.75, 1.0, fade);
            material.emissive = LinearRgba::rgb(8.0 * fade, 18.0 * fade, 32.0 * fade);
        }
    }
}

fn rotate_option_markers(
    time: Res<Time>,
    mut opt_q: Query<&mut Transform, With<FaultOptionMarker>>,
) {
    const RAD_PER_SEC: f32 = 5.0f32.to_radians();
    let delta_rad = RAD_PER_SEC * time.delta_secs();

    for mut tf in &mut opt_q {
        tf.rotate(Quat::from_rotation_y(delta_rad));
    }
}

fn spawn_fault_sender(
    commands: &mut Commands,
    fault: Entity,
    start: Vec3,
    end: Vec3,
    duration: f32,
) {
    commands.spawn((
        Transform::from_translation(start),
        Visibility::Visible,
        Pickable::IGNORE,
        FaultThrow { target: fault },
        FaultThrowAnimation {
            fault,
            start,
            end,
            elapsed: 0.0,
            duration,
        },
    ));
}

fn spawn_fault_impact(
    commands: &mut Commands,
    server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    position: Vec3,
) {
    let spark_mesh = meshes.add(Sphere::new(SPARK_SIZE).mesh().ico(1).unwrap());
    let spark_material = materials.add(StandardMaterial {
        base_color: AMBER_200.into(),
        emissive: LinearRgba::rgb(18.0, 18.0, 18.0),
        unlit: true,
        ..default()
    });

    commands.spawn((
        PointLight {
            color: Color::srgb(0.1, 0.55, 1.0),
            intensity: IMPACT_LIGHT_INTENSITY,
            range: IMPACT_LIGHT_RANGE,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_translation(position),
        FaultImpactLight {
            age: 0.0,
            duration: IMPACT_LIGHT_SECONDS,
            base_intensity: IMPACT_LIGHT_INTENSITY,
        },
    ));

    let flare_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.35, 0.75, 1.0, 1.0),
        base_color_texture: Some(load_embedded_asset!(server, "assets/flare.png")),
        emissive: LinearRgba::rgb(8.0, 18.0, 32.0),
        alpha_mode: AlphaMode::Add,
        unlit: true,
        ..default()
    });

    commands.spawn((
        Mesh3d(meshes.add(Rectangle::from_size(Vec2::splat(IMPACT_FLARE_SIZE)))),
        MeshMaterial3d(flare_material.clone()),
        Transform::from_translation(position),
        Pickable::IGNORE,
        NotShadowCaster,
        NotShadowReceiver,
        FaultImpactFlare {
            age: 0.0,
            duration: IMPACT_FLARE_SECONDS,
            material: flare_material,
        },
    ));

    for _ in 0..SPARK_COUNT {
        commands.spawn((
            Mesh3d(spark_mesh.clone()),
            MeshMaterial3d(spark_material.clone()),
            Transform::from_translation(position),
            NotShadowCaster,
            NotShadowReceiver,
            FaultImpactSpark {
                velocity: spark_velocity(),
                age: 0.0,
                lifetime: random_between(SPARK_MIN_LIFETIME, SPARK_MAX_LIFETIME),
                initial_scale: Vec3::ONE,
            },
        ));
    }
}

fn arc_position(start: Vec3, end: Vec3, t: f32) -> Vec3 {
    let arc = Vec3::Y * (FAULT_SPAWNER_ARC_HEIGHT * 4.0 * t * (1.0 - t));
    start.lerp(end, t) + arc
}

fn spark_velocity() -> Vec3 {
    let horizontal = Vec3::new(random_between(-1.0, 1.0), 0.0, random_between(-1.0, 1.0))
        .normalize_or_zero()
        * random_between(SPARK_SPEED * 0.35, SPARK_SPEED);

    horizontal + Vec3::Y * random_between(SPARK_UPWARD_SPEED * 0.35, SPARK_UPWARD_SPEED)
}

fn random_between(min: f32, max: f32) -> f32 {
    min + rand::random::<f32>() * (max - min)
}

fn set_fault_marker_visibility(
    markers: &FaultAreaMarkers,
    is_faulted: bool,
    visibility: &mut Query<&mut Visibility>,
) {
    if let Ok(mut option_visibility) = visibility.get_mut(markers.option_marker) {
        *option_visibility = if is_faulted {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }

    if let Ok(mut active_visibility) = visibility.get_mut(markers.active_marker) {
        *active_visibility = if is_faulted {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        NypaDbVariable, NypaDbVariableSemantic, NypaDbVariableStrategy, NypaDbVariableStream,
    };

    #[test]
    fn arc_position_reaches_endpoints() {
        let start = Vec3::new(1.0, 2.0, 3.0);
        let end = Vec3::new(4.0, 5.0, 6.0);

        assert_eq!(arc_position(start, end, 0.0), start);
        assert_eq!(arc_position(start, end, 1.0), end);
    }

    #[test]
    fn arc_position_lifts_midpoint() {
        let start = Vec3::ZERO;
        let end = Vec3::X;
        let midpoint = arc_position(start, end, 0.5);

        assert_eq!(midpoint.x, 0.5);
        assert!(midpoint.y > 0.0);
    }

    #[test]
    fn fault_area_defaults_to_manual_clear() {
        assert_eq!(FaultArea::default().auto_reset_after, None);
    }

    #[test]
    fn db_confirmation_replaces_requested_with_faulted_and_starts_reset() {
        let mut app = App::new();
        app.insert_resource(test_variables(1.0));
        app.add_systems(Update, sync_faulted_from_variables);
        let fault = app
            .world_mut()
            .spawn((
                FaultArea::variable(0, "fault").with_auto_reset_after(Duration::from_secs(1)),
                FaultRequested,
            ))
            .id();

        app.update();

        let entity = app.world().entity(fault);
        assert!(entity.contains::<Faulted>());
        assert!(!entity.contains::<FaultRequested>());
        assert!(entity.contains::<FaultAutoReset>());
    }

    #[test]
    fn latched_fault_does_not_start_reset_timer() {
        let mut app = App::new();
        app.insert_resource(test_variables(1.0));
        app.add_systems(Update, sync_faulted_from_variables);
        let fault = app
            .world_mut()
            .spawn((FaultArea::variable(0, "fault"), FaultRequested))
            .id();

        app.update();

        let entity = app.world().entity(fault);
        assert!(entity.contains::<Faulted>());
        assert!(!entity.contains::<FaultRequested>());
        assert!(!entity.contains::<FaultAutoReset>());
    }

    #[test]
    fn externally_activated_fault_does_not_start_reset_timer() {
        let mut app = App::new();
        app.insert_resource(test_variables(1.0));
        app.add_systems(Update, sync_faulted_from_variables);
        let fault = app.world_mut().spawn(FaultArea::variable(0, "fault")).id();

        app.update();

        let entity = app.world().entity(fault);
        assert!(entity.contains::<Faulted>());
        assert!(!entity.contains::<FaultAutoReset>());
    }

    #[test]
    fn db_reset_removes_only_authoritative_fault_state() {
        let mut app = App::new();
        app.insert_resource(test_variables(0.0));
        app.add_systems(Update, sync_faulted_from_variables);
        let fault = app
            .world_mut()
            .spawn((
                FaultArea::variable(0, "fault"),
                Faulted,
                FaultAutoReset(Timer::new(Duration::from_secs(1), TimerMode::Once)),
                FaultClearRequested,
            ))
            .id();

        app.update();

        let entity = app.world().entity(fault);
        assert!(!entity.contains::<Faulted>());
        assert!(!entity.contains::<FaultAutoReset>());
        assert!(!entity.contains::<FaultClearRequested>());
    }

    #[test]
    fn elapsed_auto_reset_requests_the_normal_clear_path() {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default());
        app.add_systems(Update, auto_reset_faults);
        let fault = app
            .world_mut()
            .spawn((
                Faulted,
                FaultAutoReset(Timer::new(Duration::from_millis(1), TimerMode::Once)),
            ))
            .id();
        app.world_mut()
            .resource_mut::<Time<()>>()
            .advance_by(Duration::from_millis(2));

        app.update();

        let entity = app.world().entity(fault);
        assert!(!entity.contains::<FaultAutoReset>());
        assert!(entity.contains::<FaultClearRequested>());
    }

    #[test]
    fn clear_request_is_removed_when_it_cannot_be_queued() {
        let mut app = App::new();
        app.add_observer(handle_fault_clear_requested);
        let fault = app
            .world_mut()
            .spawn((FaultArea::variable(0, "fault"), Faulted))
            .id();

        app.world_mut()
            .entity_mut(fault)
            .insert(FaultClearRequested);
        app.update();

        let entity = app.world().entity(fault);
        assert!(entity.contains::<Faulted>());
        assert!(!entity.contains::<FaultClearRequested>());
    }

    #[test]
    fn clear_request_is_removed_after_successful_response() {
        let mut app = App::new();
        app.add_observer(handle_fault_variable_set);
        let fault = app
            .world_mut()
            .spawn((
                FaultArea::variable(0, "fault"),
                Faulted,
                FaultClearRequested,
            ))
            .id();

        app.world_mut().trigger(NypaDbVariableSet {
            stream_id: 0,
            name: "fault".to_string(),
            value: 0.0,
        });
        app.update();

        assert!(!app.world().entity(fault).contains::<FaultClearRequested>());
    }

    #[test]
    fn clear_request_is_removed_after_failed_response() {
        let mut app = App::new();
        app.add_observer(handle_fault_control_error);
        let fault = app
            .world_mut()
            .spawn((
                FaultArea::variable(0, "fault"),
                Faulted,
                FaultClearRequested,
            ))
            .id();

        app.world_mut().trigger(NypaDbControlError {
            stream_id: 0,
            operation: NypaDbControlOperation::SetVariable {
                stream_id: 0,
                name: "fault".to_string(),
                value: 0.0,
                options: NypaDbSetVariableOptions::default(),
            },
            message: "rejected".to_string(),
        });
        app.update();

        assert!(app.world().entity(fault).contains::<Faulted>());
        assert!(!app.world().entity(fault).contains::<FaultClearRequested>());
    }

    fn test_variables(value: f32) -> NypaDbVariables {
        NypaDbVariables {
            streams: vec![NypaDbVariableStream {
                stream_id: 0,
                strategy: NypaDbVariableStrategy::JsonRpc,
                variables: vec![NypaDbVariable {
                    name: "Fault".to_string(),
                    internal_name: "fault".to_string(),
                    description: None,
                    initial_value: 0.0,
                    value,
                    min: Some(0.0),
                    max: Some(1.0),
                    semantic: NypaDbVariableSemantic::Bool,
                    index: None,
                    source: None,
                }],
            }],
        }
    }
}
