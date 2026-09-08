use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::*;

use crate::{
    DataFrame, FlatFileFormat,
    discovery::{find_publisher_sockets, stream_id_from_socket_path},
    flat_file::{FlatFileRead, FlatFileReplay},
    socket::{ReaderEventKind, ReaderHandle, spawn_reader},
};

#[derive(Clone, Debug)]
pub struct NypaDbClientPlugin {
    source: NypaDbClientSource,
}

impl Default for NypaDbClientPlugin {
    fn default() -> Self {
        Self::sockets()
    }
}

impl NypaDbClientPlugin {
    /// Connect to active NYPA DB publisher sockets.
    pub fn sockets() -> Self {
        Self {
            source: NypaDbClientSource::Sockets,
        }
    }

    /// Replay one stream from a flat file, advancing one timestep per Bevy update.
    pub fn flat_file(path: impl Into<PathBuf>, format: FlatFileFormat) -> Self {
        Self::flat_file_with_stream_id(path, format, 0)
    }

    /// Replay one stream from a flat file with a caller-selected stream id.
    pub fn flat_file_with_stream_id(
        path: impl Into<PathBuf>,
        format: FlatFileFormat,
        stream_id: usize,
    ) -> Self {
        Self {
            source: NypaDbClientSource::FlatFile {
                path: path.into(),
                format,
                stream_id,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub enum NypaDbClientSource {
    Sockets,
    FlatFile {
        path: PathBuf,
        format: FlatFileFormat,
        stream_id: usize,
    },
}

impl Plugin for NypaDbClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, drain_nypa_frames);

        app.insert_resource(NYPADBLink::with_source(self.source.clone()));

        app.add_systems(Update, check_for_reconnect);
    }
}

#[derive(Resource)]
pub struct NYPADBLink {
    pub streams: Vec<PerStreamContent>,
    source_config: NypaDbClientSource,
    source: NypaDbSourceState,
    stop: Arc<AtomicBool>,
    last_check: Instant,
}

impl Default for NYPADBLink {
    fn default() -> Self {
        Self {
            streams: Default::default(),
            source_config: NypaDbClientSource::Sockets,
            source: NypaDbSourceState::Sockets,
            stop: Default::default(),
            last_check: Instant::now(),
        }
    }
}

impl NYPADBLink {
    pub fn start() -> Self {
        Self::with_source(NypaDbClientSource::Sockets)
    }

    pub fn with_source(source_config: NypaDbClientSource) -> Self {
        let stop = Arc::new(AtomicBool::default());

        match &source_config {
            NypaDbClientSource::Sockets => start_socket_link(source_config, stop),
            NypaDbClientSource::FlatFile {
                path,
                format,
                stream_id,
            } => start_file_link(source_config.clone(), stop, path, *format, *stream_id),
        }
    }

    pub fn restart(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        *self = Self::with_source(self.source_config.clone());
    }
}

fn start_socket_link(source_config: NypaDbClientSource, stop: Arc<AtomicBool>) -> NYPADBLink {
    let sockets = match find_publisher_sockets() {
        Ok(sockets) => sockets,
        Err(err) => {
            eprintln!("NYPA DB publisher discovery failed: {err}");
            return NYPADBLink {
                streams: vec![],
                source_config,
                source: NypaDbSourceState::Sockets,
                stop,
                last_check: Instant::now(),
            };
        }
    };

    let streams = sockets
        .into_iter()
        .filter_map(|path| {
            let Some(stream_id) = stream_id_from_socket_path(&path) else {
                return None;
            };

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
        .collect();

    NYPADBLink {
        streams,
        source_config,
        source: NypaDbSourceState::Sockets,
        stop,
        last_check: Instant::now(),
    }
}

fn start_file_link(
    source_config: NypaDbClientSource,
    stop: Arc<AtomicBool>,
    path: &Path,
    format: FlatFileFormat,
    stream_id: usize,
) -> NYPADBLink {
    let (source, streams) = match FlatFileReplay::open(path, format) {
        Ok(replay) => (
            NypaDbSourceState::FlatFile(replay),
            vec![PerStreamContent {
                stream_id,
                frame: DataFrame::default(),
                handle: None,
            }],
        ),
        Err(err) => {
            eprintln!("NYPA DB flat-file replay open failed: {err}");
            (
                NypaDbSourceState::FlatFile(FlatFileReplay::closed(format)),
                vec![],
            )
        }
    };

    NYPADBLink {
        streams,
        source_config,
        source,
        stop,
        last_check: Instant::now(),
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
    Sockets,
    FlatFile(FlatFileReplay),
}

fn check_for_reconnect(mut frames: ResMut<NYPADBLink>) {
    if !matches!(frames.source, NypaDbSourceState::Sockets) {
        return;
    }

    let new_now = Instant::now();

    if new_now.duration_since(frames.last_check).as_secs_f32() < 1.0 {
        return;
    }

    frames.last_check = new_now;

    let Ok(sockets) = find_publisher_sockets() else {
        return;
    };

    let sockets: HashSet<_> = sockets
        .into_iter()
        .filter_map(|x| stream_id_from_socket_path(&x))
        .collect();

    let current_sockets: HashSet<_> = frames.streams.iter().map(|x| x.stream_id).collect();

    if sockets != current_sockets {
        frames.restart();
    }
}

fn drain_nypa_frames(mut frames: ResMut<NYPADBLink>, mut commands: Commands) {
    if matches!(frames.source, NypaDbSourceState::Sockets) {
        drain_socket_frames(&mut frames, &mut commands);
    } else {
        drain_file_frame(&mut frames, &mut commands);
    }
}

fn drain_socket_frames(frames: &mut NYPADBLink, commands: &mut Commands) {
    frames.streams.retain_mut(|stream| {
        let Some(handle) = &mut stream.handle else {
            return true;
        };

        let Some(event) = handle.update() else {
            return true;
        };

        match event.kind {
            ReaderEventKind::Frame => {
                stream.frame.copy_from(&event.payload);
                commands.trigger(NypaDbFramesChanged {
                    stream_id: stream.stream_id,
                    timestamp_us: event.payload.stamp_us,
                });
                true
            }
            ReaderEventKind::Disconnected { reason } => {
                eprintln!("NYPA DB stream {} disconnected: {reason}", stream.stream_id);
                false
            }
        }
    });
}

fn drain_file_frame(frames: &mut NYPADBLink, commands: &mut Commands) {
    let result = match &mut frames.source {
        NypaDbSourceState::FlatFile(replay) => replay.read_next(),
        NypaDbSourceState::Sockets => return,
    };

    match result {
        Ok(FlatFileRead::Frame(frame)) => {
            if let Some(stream) = frames.streams.first_mut() {
                stream.frame.copy_from(&frame);
                commands.trigger(NypaDbFramesChanged {
                    stream_id: stream.stream_id,
                    timestamp_us: frame.stamp_us,
                });
            }
        }
        Ok(FlatFileRead::Closed) => {}
        Err(err) => {
            eprintln!("NYPA DB flat-file replay read failed: {err}");
            if let NypaDbSourceState::FlatFile(replay) = &mut frames.source {
                replay.mark_finished();
            }
        }
    }
}
