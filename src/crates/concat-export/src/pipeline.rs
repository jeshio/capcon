// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Overlap drawing with conversion and encoding, with at most one queued
//! picture. The encoder is constructed on its thread: its FFmpeg contexts
//! never have to be sent between threads.

use concat_core::frame::Frame;
use concat_media::FrameSink;

const STOPPED: &str = "the video encoder stopped accepting frames";

enum Message {
    Frame(Frame),
    Finish,
}

pub(crate) fn run<S: FrameSink>(
    make: impl FnOnce() -> Result<S, String> + Send,
    draw: impl FnOnce(&mut dyn FnMut(Frame) -> Result<(), String>) -> Result<(), String>,
) -> Result<(), String> {
    std::thread::scope(|scope| {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = scope.spawn(move || {
            let mut sink = make()?;
            let mut elapsed = std::time::Duration::ZERO;
            let mut frames = 0;
            while let Ok(message) = receiver.recv() {
                match message {
                    Message::Frame(frame) => {
                        let started = std::time::Instant::now();
                        sink.write_frame(&frame)
                            .map_err(|error| error.to_string())?;
                        elapsed += started.elapsed();
                        frames += 1;
                    }
                    Message::Finish => {
                        let started = std::time::Instant::now();
                        sink.finish().map_err(|error| error.to_string())?;
                        elapsed += started.elapsed();
                        log::info!(
                            "export: encoded {frames} frames, conversion/encoding {:.3}s",
                            elapsed.as_secs_f64()
                        );
                        return Ok(());
                    }
                }
            }
            // The producer failed or was cancelled. Drop the encoder; never
            // finalize or publish an incomplete export as a success.
            Err(STOPPED.to_owned())
        });
        let mut wait = std::time::Duration::ZERO;
        let drawn = draw(&mut |frame| {
            let started = std::time::Instant::now();
            let sent = sender.send(Message::Frame(frame));
            wait += started.elapsed();
            sent.map_err(|_| STOPPED.to_owned())
        });
        if drawn.is_ok() {
            let _ = sender.send(Message::Finish);
        }
        drop(sender);
        // Always join before the caller removes the temporary file. Errors
        // and cancellation cannot leave a writer running in the background.
        let encoded = worker
            .join()
            .unwrap_or_else(|_| Err("the video encoder thread panicked".into()));
        log::info!("export: encoder queue wait {:.3}s", wait.as_secs_f64());
        match drawn {
            Err(error) if error != STOPPED => Err(error),
            _ => encoded,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl FrameSink for Sink {
        fn write_frame(&mut self, frame: &Frame) -> concat_media::Result<()> {
            self.0.lock().unwrap().push(frame.pixels()[0]);
            Ok(())
        }
        fn finish(&mut self) -> concat_media::Result<()> {
            self.0.lock().unwrap().push(255);
            Ok(())
        }
    }

    #[test]
    fn preserves_order_and_finishes_after_the_last_frame() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&seen);
        run(
            move || Ok(Sink(output)),
            |send| {
                for value in 0..20 {
                    send(Frame::from_rgba(1, 1, vec![value, 0, 0, 255]).unwrap())?;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            (0..20).chain([255]).collect::<Vec<_>>()
        );
    }

    #[test]
    fn reports_initialization_failure_even_when_the_queue_closes() {
        let result = run::<Sink>(
            || Err("encoder unavailable".into()),
            |send| {
                for _ in 0..10 {
                    send(Frame::black(1, 1))?;
                }
                Ok(())
            },
        );
        assert_eq!(result, Err("encoder unavailable".into()));
    }

    #[test]
    fn cancellation_joins_the_writer_without_finalizing() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&seen);
        let result = run(
            move || Ok(Sink(output)),
            |send| {
                send(Frame::black(1, 1))?;
                Err("cancelled".into())
            },
        );
        assert_eq!(result, Err("cancelled".into()));
        assert_eq!(*seen.lock().unwrap(), vec![0]);
    }

    #[test]
    fn reports_an_encoder_panic_instead_of_hanging() {
        let result = run::<Sink>(
            || panic!("failed encoder"),
            |send| {
                for _ in 0..10 {
                    send(Frame::black(1, 1))?;
                }
                Ok(())
            },
        );
        assert_eq!(result, Err("the video encoder thread panicked".into()));
    }

    #[test]
    fn a_write_failure_unblocks_the_producer_and_keeps_its_reason() {
        struct Failing;
        impl FrameSink for Failing {
            fn write_frame(&mut self, _: &Frame) -> concat_media::Result<()> {
                Err(concat_media::Error::Ffi {
                    operation: "test write",
                    path: Default::default(),
                    detail: "disk full".into(),
                })
            }
            fn finish(&mut self) -> concat_media::Result<()> {
                panic!("a failed writer must not finalize");
            }
        }
        let error = run(
            || Ok(Failing),
            |send| {
                for _ in 0..20 {
                    send(Frame::black(1, 1))?;
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.contains("disk full"), "{error}");
    }
}
