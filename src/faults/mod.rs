use std::collections::HashSet;

use bevy::{
    asset::{embedded_asset, load_embedded_asset},
    color::palettes::tailwind::AMBER_200,
    light::{NotShadowCaster, NotShadowReceiver},
    prelude::*,
};

use crate::{NypaDbControl, NypaDbSetVariableOptions, NypaDbVariables};

const FAULT_SPAWNER_SECONDS: f32 = 1.0;
const FAULT_SPAWNER_ARC_HEIGHT: f32 = 0.15;
const FAULT_SPAWNER_SCALE: f32 = 0.6;
const FAULT_SPAWNER_REVOLUTIONS_PER_SECOND: f32 = 1.0;
const FAULT_SENDER_LIGHT_INTENSITY: f32 = 180_000.0;
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
        embedded_asset!(app, "assets/SpawnerGraphic.glb");
        embedded_asset!(app, "assets/flare.png");

        app.insert_resource(FaultVariablePollTimer(Timer::from_seconds(
            FAULT_POLL_SECONDS,
            TimerMode::Repeating,
        )))
        .add_observer(trigger_fault_throw)
        .add_systems(
            Update,
            (
                attach_fault_markers,
                sync_faulted_from_variables,
                poll_fault_variables,
                update_fault_marker_visibility,
                animate_fault_senders,
                disable_fault_sender_shadows,
                spawn_fault_impacts,
                update_fault_impacts,
                rotate_option_markers,
            )
                .chain(),
        );
    }
}

#[derive(Component, Clone, Debug, Default)]
pub struct FaultArea {
    pub variable: Option<FaultVariable>,
    pub set_variable_options: NypaDbSetVariableOptions,
}

impl FaultArea {
    pub fn new(variable: Option<FaultVariable>) -> Self {
        Self {
            variable,
            set_variable_options: NypaDbSetVariableOptions::default(),
        }
    }

    pub fn with_options(
        variable: Option<FaultVariable>,
        set_variable_options: NypaDbSetVariableOptions,
    ) -> Self {
        Self {
            variable,
            set_variable_options,
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultVariable {
    pub stream_id: usize,
    pub name: String,
}

#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FaultSpawnSource;

#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Faulted;

#[derive(Clone, Copy, Debug, Event)]
pub struct NypaDbFaultTrigger {
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
struct FaultSpawnerGraphic {
    fault: Entity,
    start: Vec3,
    end: Vec3,
    elapsed: f32,
    duration: f32,
}

#[derive(Component)]
struct FaultSenderLight {
    base_intensity: f32,
}

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

type ShadowedFaultSenderPart = (
    Or<(With<Mesh3d>, With<SceneRoot>)>,
    Without<NotShadowCaster>,
);
type SparkQueryFilter = (Without<FaultImpactLight>, Without<FaultImpactFlare>);
type ImpactLightQueryFilter = (Without<FaultImpactSpark>, Without<FaultImpactFlare>);
type FlareQueryFilter = (Without<FaultImpactSpark>, Without<FaultImpactLight>);

fn trigger_fault_throw(
    trigger: On<NypaDbFaultTrigger>,
    mut commands: Commands,
    server: Res<AssetServer>,
    sources: Query<&GlobalTransform, With<FaultSpawnSource>>,
    faults: Query<(Has<Faulted>, &GlobalTransform), With<FaultArea>>,
) {
    let target = trigger.event().target;
    let Ok((is_faulted, fault_transform)) = faults.get(target) else {
        warn!("Cannot trigger NYPA DB fault: target entity is not a FaultArea");
        return;
    };

    if is_faulted {
        return;
    }

    let Ok(source_transform) = sources.single() else {
        warn!("Cannot trigger NYPA DB fault: expected exactly one FaultSpawnSource");
        return;
    };

    spawn_fault_sender(
        &mut commands,
        &server,
        target,
        source_transform.translation(),
        fault_transform.translation(),
    );
}

fn attach_fault_markers(
    mut commands: Commands,
    server: Res<AssetServer>,
    candidates: Query<(Entity, Has<Faulted>), (With<FaultArea>, Without<FaultAreaMarkers>)>,
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
                SceneRoot(load_embedded_gltf_scene(&server, "FaultOption.glb")),
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
                SceneRoot(load_embedded_gltf_scene(&server, "FaultActive.glb")),
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
    fault_areas: Query<(Entity, &FaultArea, Has<Faulted>)>,
) {
    let Some(variables) = variables else {
        return;
    };

    for (entity, area, is_faulted) in &fault_areas {
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
                commands.entity(entity).insert(Faulted);
            }
            (true, false) => {
                commands.entity(entity).remove::<Faulted>();
            }
            _ => {}
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
    mut senders: Query<(Entity, &mut Transform, &mut FaultSpawnerGraphic)>,
    mut lights: Query<(&mut PointLight, &FaultSenderLight)>,
) {
    for (entity, mut transform, mut sender) in &mut senders {
        sender.elapsed += time.delta_secs();
        let t = (sender.elapsed / sender.duration).clamp(0.0, 1.0);

        transform.translation = arc_position(sender.start, sender.end, t);
        transform.rotation = Quat::from_rotation_y(
            sender.elapsed * std::f32::consts::TAU * FAULT_SPAWNER_REVOLUTIONS_PER_SECOND,
        );

        if t >= 1.0 {
            commands.entity(sender.fault).insert(Faulted);
            commands.entity(entity).despawn();

            if let Ok(area) = areas.get(sender.fault)
                && let Some(variable) = &area.variable
                && let Some(control) = control.as_ref()
            {
                let _ = control.set_variable_with_options(
                    variable.stream_id,
                    &variable.name,
                    1.0,
                    area.set_variable_options.clone(),
                );
            }
        }
    }

    for (mut light, sender_light) in &mut lights {
        light.intensity = sender_light.base_intensity * random_between(0.68, 1.24);
    }
}

fn disable_fault_sender_shadows(
    mut commands: Commands,
    senders: Query<Entity, With<FaultSpawnerGraphic>>,
    children: Query<&ChildOf>,
    shadowed: Query<Entity, ShadowedFaultSenderPart>,
) {
    if senders.is_empty() {
        return;
    }

    for entity in &shadowed {
        if is_descendant_of_any(entity, &senders, &children) {
            commands
                .entity(entity)
                .insert((Pickable::IGNORE, NotShadowCaster, NotShadowReceiver));
        }
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
    server: &AssetServer,
    fault: Entity,
    start: Vec3,
    end: Vec3,
) {
    let sender = commands
        .spawn((
            Transform::from_translation(start).with_scale(Vec3::splat(FAULT_SPAWNER_SCALE)),
            Visibility::Visible,
            Pickable::IGNORE,
            NotShadowCaster,
            NotShadowReceiver,
            FaultSpawnerGraphic {
                fault,
                start,
                end,
                elapsed: 0.0,
                duration: FAULT_SPAWNER_SECONDS,
            },
            SceneRoot(load_embedded_gltf_scene(server, "SpawnerGraphic.glb")),
        ))
        .id();

    commands.spawn((
        PointLight {
            color: Color::srgb(1.0, 0.82, 0.12),
            intensity: FAULT_SENDER_LIGHT_INTENSITY,
            range: 2.5,
            shadows_enabled: false,
            ..default()
        },
        FaultSenderLight {
            base_intensity: FAULT_SENDER_LIGHT_INTENSITY,
        },
        Transform::default(),
        ChildOf(sender),
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

fn is_descendant_of_any(
    entity: Entity,
    ancestors: &Query<Entity, With<FaultSpawnerGraphic>>,
    children: &Query<&ChildOf>,
) -> bool {
    let mut current = entity;

    while let Ok(parent) = children.get(current) {
        current = parent.parent();

        if ancestors.contains(current) {
            return true;
        }
    }

    false
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

fn load_embedded_gltf_scene(server: &AssetServer, file_name: &str) -> Handle<Scene> {
    let path = format!("embedded://nypa_db_connector/faults/assets/{file_name}");
    server.load(GltfAssetLabel::Scene(0).from_asset(path))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
