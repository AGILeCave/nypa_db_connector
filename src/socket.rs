use std::{
    io::{ErrorKind, Result},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use triple_buffer::{Input, Output};

use crate::DataFrame;

pub(crate) struct ReaderHandle {
    _thread: JoinHandle<()>,
    rx: Output<ReaderEvent>,
}

#[derive(Default, Clone)]
pub(crate) enum ReaderEventKind {
    #[default]
    Frame,
    Disconnected {
        reason: String,
    },
}

#[derive(Default, Clone)]
pub(crate) struct ReaderEvent {
    pub(crate) kind: ReaderEventKind,
    pub(crate) payload: DataFrame,
}

impl ReaderHandle {
    pub(crate) fn drain(&mut self, f: impl FnOnce(&ReaderEvent)) -> bool {
        if !self.rx.update() {
            return false;
        }

        f(self.rx.output_buffer());
        true
    }
}

/// Create a reader for a socket stream.
pub(crate) fn spawn_reader(stopper: Arc<AtomicBool>, path: PathBuf) -> Result<ReaderHandle> {
    let socket = UnixStream::connect(path)?;

    // Prevent stalls.
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;

    let (tx, rx) = triple_buffer::triple_buffer(&ReaderEvent::default());

    let thread = thread::spawn(move || reader_thread(stopper, socket, tx));

    Ok(ReaderHandle {
        _thread: thread,
        rx,
    })
}

fn reader_thread(reader_stop: Arc<AtomicBool>, mut socket: UnixStream, mut tx: Input<ReaderEvent>) {
    let mut frame = DataFrame::default();

    while !reader_stop.load(Ordering::Acquire) {
        match frame.update_from(&mut socket) {
            Ok(()) => {
                let mut publisher = tx.input_buffer_publisher();

                publisher.kind = ReaderEventKind::Frame;
                publisher.payload.copy_from(&frame);
                continue;
            }

            Err(err) => {
                match err.kind() {
                    // If it was a blocker or a timed out, try again.
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted => {
                        continue;
                    }

                    _ => {
                        let mut publisher = tx.input_buffer_publisher();
                        publisher.kind = ReaderEventKind::Disconnected {
                            reason: err.to_string(),
                        };
                    }
                }

                return;
            }
        }
    }
}
