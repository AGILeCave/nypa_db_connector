use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use bevy::prelude::*;

use crate::{
    DataFrame, FlatFileFormat,
    discovery::{find_publisher_sockets, stream_id_from_socket_path},
    flat_file::{FlatFileRead, FlatFileReplay},
    socket::{ReaderEventKind, ReaderHandle, spawn_reader},
};

/// Configuration for the optional prerecorded fallback used during startup.
#[derive(Clone, Debug)]
pub struct NypaDbPrerecordedSource {
    pub path: PathBuf,
    pub format: FlatFileFormat,
    pub stream_id: usize,
    /// Number of complete timesteps to discard between emitted timesteps.
    pub skip: usize,
}

impl NypaDbPrerecordedSource {
    /// Replay one stream from a flat file, advancing one timestep per Bevy update.
    ///
    /// Frames default to stream `0`. Use [`Self::with_stream_id`] to assign the fallback to a
    /// different stream.
    pub fn new(path: impl Into<PathBuf>, format: FlatFileFormat) -> Self {
        Self {
            path: path.into(),
            format,
            stream_id: 0,
            skip: 0,
        }
    }

    /// Set the stream id assigned to frames from this source.
    pub fn with_stream_id(mut self, stream_id: usize) -> Self {
        self.stream_id = stream_id;
        self
    }

    /// Set the number of complete timesteps discarded between emitted timesteps.
    pub fn with_skip(mut self, skip: usize) -> Self {
        self.skip = skip;
        self
    }
}

#[derive(Clone, Debug)]
pub struct NypaDbClientPlugin {
    prerecorded: Option<NypaDbPrerecordedSource>,
}

impl NypaDbClientPlugin {
    /// Prefer live publisher sockets, using `prerecorded` only when no socket can be connected
    /// during startup.
    pub fn new(prerecorded: Option<NypaDbPrerecordedSource>) -> Self {
        Self { prerecorded }
    }
}

impl Plugin for NypaDbClientPlugin {
    fn build(&self, app: &mut App) {
        let link = NYPADBLink::start(self.prerecorded.as_ref());

        app.insert_resource(link)
            .add_systems(Startup, announce_initial_connection_mode)
            .add_systems(Update, (drain_nypa_frames, check_for_source_change).chain());
    }
}

/// The source currently supplying NYPA DB frames.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NypaDbConnectionMode {
    /// One or more publisher sockets are connected.
    Streaming,
    /// The startup fallback file is supplying frames.
    Prerecorded,
    /// No frame source is currently available.
    #[default]
    Idle,
}

/// Triggered whenever the active frame source mode changes.
#[derive(Clone, Copy, Debug, Event, Eq, PartialEq)]
pub struct NypaDbConnectionModeChanged {
    pub previous: NypaDbConnectionMode,
    pub current: NypaDbConnectionMode,
}

#[derive(Resource)]
pub struct NYPADBLink {
    pub streams: Vec<PerStreamContent>,
    source: NypaDbSourceState,
    stop: Arc<AtomicBool>,
    last_check: Instant,
}

impl NYPADBLink {
    fn start(prerecorded: Option<&NypaDbPrerecordedSource>) -> Self {
        let stop = Arc::new(AtomicBool::default());
        let streams = connect_socket_streams(stop.clone());

        if !streams.is_empty() {
            return Self {
                streams,
                source: NypaDbSourceState::Streaming,
                stop,
                last_check: Instant::now(),
            };
        }
        if let Some(config) = prerecorded {
            return Self::start_prerecorded(config, stop);
        }
        Self::idle(stop)
    }

    fn start_prerecorded(config: &NypaDbPrerecordedSource, stop: Arc<AtomicBool>) -> Self {
        match FlatFileReplay::open(&config.path, config.format, config.skip) {
            Ok(replay) => Self {
                streams: vec![PerStreamContent {
                    stream_id: config.stream_id,
                    frame: DataFrame::default(),
                    handle: None,
                }],
                source: NypaDbSourceState::Prerecorded(replay),
                stop,
                last_check: Instant::now(),
            },
            Err(err) => {
                eprintln!("NYPA DB prerecorded file open failed: {err}");
                Self::idle(stop)
            }
        }
    }

    fn idle(stop: Arc<AtomicBool>) -> Self {
        Self {
            streams: vec![],
            source: NypaDbSourceState::Idle,
            stop,
            last_check: Instant::now(),
        }
    }

    /// Return the source currently supplying frames.
    pub fn mode(&self) -> NypaDbConnectionMode {
        match self.source {
            NypaDbSourceState::Streaming => NypaDbConnectionMode::Streaming,
            NypaDbSourceState::Prerecorded(_) => NypaDbConnectionMode::Prerecorded,
            NypaDbSourceState::Idle => NypaDbConnectionMode::Idle,
        }
    }

    fn replace_with_idle(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.stop = Arc::new(AtomicBool::default());
        self.streams.clear();
        self.source = NypaDbSourceState::Idle;
        self.last_check = Instant::now();
    }
}

impl Drop for NYPADBLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Latest published DB frame content for one publisher stream.
pub struct PerStreamContent {
    /// Publisher stream id.
    pub stream_id: usize,
    pub frame: DataFrame,
    /// Internal handle to reader thread.
    handle: Option<ReaderHandle>,
}

/// Sent when `PerStreamContent` changed.
#[derive(Clone, Debug, Event)]
pub struct NypaDbFramesChanged {
    /// Publisher stream id.
    pub stream_id: usize,
    /// New time.
    pub timestamp_us: u64,
}

enum NypaDbSourceState {
    Streaming,
    Prerecorded(FlatFileReplay),
    Idle,
}

fn connect_socket_streams(stop: Arc<AtomicBool>) -> Vec<PerStreamContent> {
    let sockets = match find_publisher_sockets() {
        Ok(sockets) => sockets,
        Err(err) => {
            eprintln!("NYPA DB publisher discovery failed: {err}");
            return vec![];
        }
    };

    sockets
        .into_iter()
        .filter_map(|path| {
            let stream_id = stream_id_from_socket_path(&path)?;
            let handle = match spawn_reader(stop.clone(), path) {
                Ok(reader) => reader,
                Err(err) => {
                    eprintln!("NYPA DB stream {stream_id} connect failed: {err}");
                    return None;
                }
            };
            Some(PerStreamContent {
                stream_id,
                frame: DataFrame::default(),
                handle: Some(handle),
            })
        })
        .collect()
}

fn announce_initial_connection_mode(link: Res<NYPADBLink>, mut commands: Commands) {
    let current = link.mode();
    if current != NypaDbConnectionMode::Idle {
        commands.trigger(NypaDbConnectionModeChanged {
            previous: NypaDbConnectionMode::Idle,
            current,
        });
    }
}

fn check_for_source_change(mut link: ResMut<NYPADBLink>, mut commands: Commands) {
    let now = Instant::now();
    if now.duration_since(link.last_check).as_secs_f32() < 1.0 {
        return;
    }
    link.last_check = now;

    match link.mode() {
        NypaDbConnectionMode::Prerecorded | NypaDbConnectionMode::Idle => {
            let candidate_stop = Arc::new(AtomicBool::default());
            let streams = connect_socket_streams(candidate_stop.clone());
            if streams.is_empty() {
                return;
            }

            let previous = link.mode();
            link.stop.store(true, Ordering::Release);
            link.stop = candidate_stop;
            link.streams = streams;
            link.source = NypaDbSourceState::Streaming;
            link.last_check = Instant::now();
            commands.trigger(NypaDbConnectionModeChanged {
                previous,
                current: NypaDbConnectionMode::Streaming,
            });
        }
        NypaDbConnectionMode::Streaming => {
            let discovered = match find_publisher_sockets() {
                Ok(sockets) => sockets,
                Err(_) => return,
            };
            let discovered_ids: HashSet<_> = discovered
                .iter()
                .filter_map(|path| stream_id_from_socket_path(path))
                .collect();
            let connected_ids: HashSet<_> =
                link.streams.iter().map(|stream| stream.stream_id).collect();

            if discovered_ids == connected_ids {
                return;
            }

            let candidate_stop = Arc::new(AtomicBool::default());
            let streams = connect_socket_streams(candidate_stop.clone());
            if streams.is_empty() {
                candidate_stop.store(true, Ordering::Release);
                link.replace_with_idle();
                commands.trigger(NypaDbConnectionModeChanged {
                    previous: NypaDbConnectionMode::Streaming,
                    current: NypaDbConnectionMode::Idle,
                });
            } else {
                link.stop.store(true, Ordering::Release);
                link.stop = candidate_stop;
                link.streams = streams;
                link.source = NypaDbSourceState::Streaming;
                link.last_check = Instant::now();
            }
        }
    }
}

fn drain_nypa_frames(mut link: ResMut<NYPADBLink>, mut commands: Commands) {
    match link.source {
        NypaDbSourceState::Streaming => drain_socket_frames(&mut link, &mut commands),
        NypaDbSourceState::Prerecorded(_) => drain_file_frame(&mut link, &mut commands),
        NypaDbSourceState::Idle => {}
    }
}

fn drain_socket_frames(link: &mut NYPADBLink, commands: &mut Commands) {
    link.streams.retain_mut(|stream| {
        let Some(handle) = &mut stream.handle else {
            return true;
        };
        let mut keep_stream = true;
        let changed = handle.drain(|event| match &event.kind {
            ReaderEventKind::Frame => {
                stream.frame.copy_from(&event.payload);
                commands.trigger(NypaDbFramesChanged {
                    stream_id: stream.stream_id,
                    timestamp_us: event.payload.stamp_us,
                });
            }
            ReaderEventKind::Disconnected { reason } => {
                eprintln!("NYPA DB stream {} disconnected: {reason}", stream.stream_id);
                keep_stream = false;
            }
        });
        !changed || keep_stream
    });

    if link.streams.is_empty() {
        link.replace_with_idle();
        commands.trigger(NypaDbConnectionModeChanged {
            previous: NypaDbConnectionMode::Streaming,
            current: NypaDbConnectionMode::Idle,
        });
    }
}

fn drain_file_frame(link: &mut NYPADBLink, commands: &mut Commands) {
    let result = match &mut link.source {
        NypaDbSourceState::Prerecorded(replay) => replay.read_next(),
        _ => return,
    };

    match result {
        Ok(FlatFileRead::Frame(frame)) => {
            if let Some(stream) = link.streams.first_mut() {
                stream.frame.copy_from(&frame);
                commands.trigger(NypaDbFramesChanged {
                    stream_id: stream.stream_id,
                    timestamp_us: frame.stamp_us,
                });
            }
        }
        Ok(FlatFileRead::Closed) => {}
        Err(err) => {
            eprintln!("NYPA DB prerecorded file read failed: {err}");
            link.replace_with_idle();
            commands.trigger(NypaDbConnectionModeChanged {
                previous: NypaDbConnectionMode::Prerecorded,
                current: NypaDbConnectionMode::Idle,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[derive(Resource, Default)]
    struct ObservedModeChanges(Vec<NypaDbConnectionModeChanged>);

    #[derive(Resource, Default)]
    struct ObservedFrameChanges(Vec<NypaDbFramesChanged>);

    fn record_mode_change(
        change: On<NypaDbConnectionModeChanged>,
        mut observed: ResMut<ObservedModeChanges>,
    ) {
        observed.0.push(*change);
    }

    fn record_frame_change(
        change: On<NypaDbFramesChanged>,
        mut observed: ResMut<ObservedFrameChanges>,
    ) {
        observed.0.push(change.clone());
    }

    #[test]
    fn prerecorded_builder_sets_optional_playback_values() {
        let source =
            NypaDbPrerecordedSource::new("capture.bin", FlatFileFormat::F32 { variable_count: 2 })
                .with_stream_id(7)
                .with_skip(3);

        assert_eq!(source.path, Path::new("capture.bin"));
        assert_eq!(source.stream_id, 7);
        assert_eq!(source.skip, 3);
    }

    #[test]
    fn prerecorded_frames_use_selected_stream_id() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nypa_db_connector_stream_{}_{}.bin",
            std::process::id(),
            unique
        ));
        let values = [1.25_f32, 2.5_f32];
        fs::write(&path, bytemuck::cast_slice(&values)).unwrap();

        {
            let source =
                NypaDbPrerecordedSource::new(&path, FlatFileFormat::F32 { variable_count: 1 })
                    .with_stream_id(7);
            let link = NYPADBLink::start_prerecorded(&source, Arc::new(AtomicBool::default()));
            let mut app = App::new();
            app.insert_resource(link)
                .init_resource::<ObservedFrameChanges>()
                .add_observer(record_frame_change)
                .add_systems(Update, drain_nypa_frames);

            app.update();

            let link = app.world().resource::<NYPADBLink>();
            assert_eq!(link.streams[0].stream_id, 7);
            assert_eq!(link.streams[0].frame.content, [2.5]);
            assert_eq!(
                app.world().resource::<ObservedFrameChanges>().0[0].stream_id,
                7
            );
        }

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_prerecorded_file_is_idle() {
        let stop = Arc::new(AtomicBool::default());
        let source = NypaDbPrerecordedSource::new(
            "/a/path/that/does/not/exist/capture.bin",
            FlatFileFormat::F32 { variable_count: 2 },
        );
        let link = NYPADBLink::start_prerecorded(&source, stop);

        assert_eq!(link.mode(), NypaDbConnectionMode::Idle);
        assert!(link.streams.is_empty());
    }

    #[test]
    fn initial_non_idle_mode_is_observable() {
        let mut app = App::new();
        app.insert_resource(NYPADBLink {
            streams: vec![],
            source: NypaDbSourceState::Streaming,
            stop: Arc::new(AtomicBool::default()),
            last_check: Instant::now(),
        })
        .init_resource::<ObservedModeChanges>()
        .add_observer(record_mode_change)
        .add_systems(Startup, announce_initial_connection_mode);

        app.update();

        assert_eq!(
            app.world().resource::<ObservedModeChanges>().0,
            [NypaDbConnectionModeChanged {
                previous: NypaDbConnectionMode::Idle,
                current: NypaDbConnectionMode::Streaming,
            }]
        );
    }
}
