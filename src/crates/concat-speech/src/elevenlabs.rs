// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Capcon contributors

//! Scribe v2 file transcription. Only the selected source window and audio
//! stream leave the device. PCM is spooled to an anonymous temporary file,
//! not collected in memory, so a long recording also fits on a phone.

use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use concat_media::{AudioDecoder, AudioOptions, SampleFormat};
use serde::Deserialize;

use crate::JobStage;
use crate::transcribe::{Segment, TranscribeRequest};

const ENDPOINT: &str = "https://api.elevenlabs.io/v1/speech-to-text";
const BOUNDARY: &str = "capcon-scribe-7d44d9c125e341d6";
const RESPONSE_LIMIT: u64 = 32 * 1024 * 1024;

/// Scribe request options, separate from the source audio window.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options<'a> {
    /// ISO language code, or automatic detection.
    pub language: Option<&'a str>,
    /// Ask Scribe v2 to omit fillers, false starts and non-speech sounds.
    pub clean_speech: bool,
}

/// A key can travel in a pane message without being printed by Debug.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ApiKey(String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey([redacted])")
    }
}

impl ApiKey {
    /// A key pasted into the password field, with outer whitespace removed.
    pub fn new(value: String) -> Self {
        Self(value.trim().to_owned())
    }

    /// Whether no key has been entered or restored.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// For the password field and the provider's authentication header.
    pub fn value(&self) -> &str {
        &self.0
    }

    /// Validate before HTTP header construction; errors never echo a key.
    pub fn validate(&self) -> Result<(), String> {
        if self.is_empty() || !self.0.bytes().all(|c| c.is_ascii_graphic()) {
            return Err("Enter a valid ElevenLabs API key.".to_owned());
        }
        Ok(())
    }
}

/// Keychain storage on the Apple platforms. Other platforms keep the key
/// for this session; it is never serialized to a project or preferences.
pub const CAN_SAVE_KEY: bool = cfg!(any(target_os = "macos", target_os = "ios"));

#[cfg(any(target_os = "macos", target_os = "ios"))]
const SERVICE: &str = "dev.jeshio.capcon.elevenlabs";

/// Restore a saved key, or an empty key if no credential exists.
pub fn load_key() -> Result<ApiKey, String> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        match security_framework::passwords::get_generic_password(SERVICE, "api-key") {
            Ok(bytes) => String::from_utf8(bytes)
                .map(ApiKey::new)
                .map_err(|_| "Could not read the ElevenLabs key from Keychain.".to_owned()),
            Err(error) if error.code() == -25300 => Ok(ApiKey::default()),
            Err(_) => Err("Could not read the ElevenLabs key from Keychain.".to_owned()),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    Ok(ApiKey::default())
}

/// Persist a key in the system credential store on Apple platforms.
pub fn save_key(key: &ApiKey) -> Result<(), String> {
    key.validate()?;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        security_framework::passwords::set_generic_password(SERVICE, "api-key", key.0.as_bytes())
            .map_err(|_| "Could not save the ElevenLabs key to Keychain.".to_owned())
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    Err("The ElevenLabs key is available for this session only.".to_owned())
}

/// Remove the saved credential. A missing credential is already removed.
pub fn forget_key() -> Result<(), String> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        match security_framework::passwords::delete_generic_password(SERVICE, "api-key") {
            Ok(()) => Ok(()),
            Err(error) if error.code() == -25300 => Ok(()),
            Err(_) => Err("Could not remove the ElevenLabs key from Keychain.".to_owned()),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    Ok(())
}

pub(crate) fn transcribe(
    request: &TranscribeRequest,
    key: &ApiKey,
    options: Options<'_>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(JobStage),
) -> Result<Vec<Segment>, String> {
    key.validate()?;
    if !request.window.is_finite()
        || request.window <= 0.0
        || !request.source_start.is_finite()
        || request.source_start < 0.0
    {
        return Err("The clip has an invalid audio window.".to_owned());
    }
    check_cancel(cancel)?;
    progress(JobStage::Reading);
    let (audio, size) = prepare_audio(request, cancel)?;
    progress(JobStage::Working {
        done: 0.0,
        next: 1.0,
    });
    let response = send(ENDPOINT, audio, size, key, options, cancel);
    check_cancel(cancel)?;
    let segments = captions(response?, request.window)?;
    progress(JobStage::Working {
        done: 1.0,
        next: 1.0,
    });
    Ok(segments)
}

fn prepare_audio(
    request: &TranscribeRequest,
    cancel: &AtomicBool,
) -> Result<(std::fs::File, u64), String> {
    check_cancel(cancel)?;
    let mut audio =
        tempfile::tempfile().map_err(|_| "Could not prepare temporary audio.".to_owned())?;
    let mut decoder = AudioDecoder::open(
        &request.path,
        &AudioOptions {
            start: Some(request.source_start),
            duration: Some(request.window),
            filters: Vec::new(),
            rate: 16_000,
            channels: 1,
            format: SampleFormat::I16,
            stream: request.audio_stream.map(|index| index as usize),
        },
    )
    .map_err(|error| error.to_string())?;
    let mut size = 0u64;
    while let Some(frame) = decoder.next_frame().map_err(|error| error.to_string())? {
        check_cancel(cancel)?;
        let bytes = decoder.bytes_of(&frame);
        size += bytes.len() as u64;
        // Leave room below ElevenLabs' upload limit. No API request is
        // issued for an oversized file or an empty stream.
        if size > 4_900_000_000 {
            return Err("The audio is too long for one ElevenLabs request.".to_owned());
        }
        audio
            .write_all(&bytes)
            .map_err(|_| "Could not prepare temporary audio.".to_owned())?;
    }
    if size < 3_200 {
        return Err("ElevenLabs needs at least 0.1 seconds of audio.".to_owned());
    }
    audio
        .seek(SeekFrom::Start(0))
        .map_err(|_| "Could not read temporary audio.".to_owned())?;
    Ok((audio, size))
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("transcription cancelled".to_owned())
    } else {
        Ok(())
    }
}

/// Cancellation is checked between reads and writes. While the provider
/// prepares its response the blocking HTTP call can only finish or time
/// out; callers retain the job slot and show Cancelling until then.
struct Cancellable<'a, R> {
    reader: R,
    cancel: &'a AtomicBool,
}

impl<R: Read> Read for Cancellable<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        check_cancel(self.cancel).map_err(std::io::Error::other)?;
        self.reader.read(buf)
    }
}

fn multipart(options: Options<'_>) -> Result<(String, String), String> {
    let mut prefix = String::new();
    for (name, value) in [
        ("model_id", "scribe_v2"),
        ("file_format", "pcm_s16le_16"),
        ("timestamps_granularity", "word"),
        ("tag_audio_events", "false"),
        ("diarize", "false"),
        (
            "no_verbatim",
            if options.clean_speech {
                "true"
            } else {
                "false"
            },
        ),
    ] {
        prefix.push_str(&format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        ));
    }
    if let Some(language) = options.language {
        if !(2..=3).contains(&language.len()) || !language.bytes().all(|c| c.is_ascii_lowercase()) {
            return Err("Invalid transcription language.".to_owned());
        }
        prefix.push_str(&format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"language_code\"\r\n\r\n{language}\r\n"));
    }
    prefix.push_str(&format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.pcm\"\r\nContent-Type: application/octet-stream\r\n\r\n"));
    Ok((prefix, format!("\r\n--{BOUNDARY}--\r\n")))
}

fn send(
    endpoint: &str,
    audio: impl Read,
    size: u64,
    key: &ApiKey,
    options: Options<'_>,
    cancel: &AtomicBool,
) -> Result<Transcript, String> {
    key.validate()?;
    check_cancel(cancel)?;
    let (prefix, suffix) = multipart(options)?;
    let length = prefix.len() as u64 + size + suffix.len() as u64;
    let body = prefix.as_bytes().chain(audio).chain(suffix.as_bytes());
    // No redirect may forward xi-api-key, and no automatic application
    // retry may submit a second paid transcription after an uncertain result.
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(20))
        .timeout_write(Duration::from_secs(60))
        .timeout_read(Duration::from_secs(300))
        .timeout(Duration::from_secs(1800))
        .build();
    let response = agent.post(endpoint)
        .set("xi-api-key", &key.0)
        .set("Content-Type", &format!("multipart/form-data; boundary={BOUNDARY}"))
        .set("Content-Length", &length.to_string())
        .send(Cancellable { reader: body, cancel })
        .map_err(|error| match error {
            ureq::Error::Status(401 | 403, _) => "ElevenLabs rejected the key. Check the key and Speech to Text permission.".to_owned(),
            ureq::Error::Status(402 | 429, _) => "ElevenLabs quota or rate limit reached. Check your balance and try again later.".to_owned(),
            ureq::Error::Status(status, _) => format!("ElevenLabs request failed (HTTP {status})."),
            // The provider's body and transport debug data can include
            // private input. Never echo them into the UI or app log.
            ureq::Error::Transport(_) => "Could not complete the ElevenLabs request. Check your connection. It has not been retried automatically.".to_owned(),
        })?;
    if response.status() != 200 {
        return Err(format!(
            "ElevenLabs request failed (HTTP {}).",
            response.status()
        ));
    }
    check_cancel(cancel)?;
    let mut data = Vec::new();
    Cancellable {
        reader: response.into_reader(),
        cancel,
    }
    .take(RESPONSE_LIMIT + 1)
    .read_to_end(&mut data)
    .map_err(|_| "Could not read the ElevenLabs response.".to_owned())?;
    check_cancel(cancel)?;
    if data.len() as u64 > RESPONSE_LIMIT {
        return Err("The ElevenLabs response is too large.".to_owned());
    }
    serde_json::from_slice(&data)
        .map_err(|_| "ElevenLabs returned an invalid transcript.".to_owned())
}

#[derive(Deserialize)]
struct Transcript {
    text: String,
    #[serde(default)]
    words: Vec<Word>,
}

#[derive(Deserialize)]
struct Word {
    text: String,
    #[serde(rename = "type")]
    kind: String,
    start: Option<f64>,
    end: Option<f64>,
}

/// A subtitle is a readable phrase, not a clip for every single word.
/// Split on sentence ends, pauses, six seconds, or roughly two short lines.
fn captions(transcript: Transcript, window: f64) -> Result<Vec<Segment>, String> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut cue: Option<Segment> = None;
    for word in transcript
        .words
        .into_iter()
        .filter(|word| word.kind == "word")
    {
        let text = word.text.trim();
        if text.is_empty() {
            continue;
        }
        let (Some(start), Some(end)) = (word.start, word.end) else {
            return Err("ElevenLabs returned words without timestamps.".to_owned());
        };
        // Scribe can align a short word to a point. Its phrase can still
        // have a useful duration; only reversed or invalid times are errors.
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end < start {
            return Err("ElevenLabs returned invalid word timestamps.".to_owned());
        }
        if start > window || (start == window && end > start) {
            continue;
        }
        let end = end.min(window);
        if let Some(current) = &cue {
            if start < current.start {
                return Err("ElevenLabs returned unordered word timestamps.".to_owned());
            }
            if start - current.end > 0.8
                || end - current.start > 6.0
                || current.text.chars().count() + text.chars().count() + 1 > 72
            {
                segments.push(cue.take().expect("cue exists"));
            }
        }
        let current = cue.get_or_insert_with(|| Segment {
            start,
            end,
            text: String::new(),
        });
        append_text(&mut current.text, text);
        current.end = current.end.max(end);
        if text.ends_with(['.', '!', '?', '…']) {
            segments.push(cue.take().expect("cue exists"));
        }
    }
    if let Some(cue) = cue {
        segments.push(cue);
    }
    if segments.is_empty() && !transcript.text.trim().is_empty() {
        return Err("ElevenLabs returned text without usable word timestamps.".to_owned());
    }
    finish_cues(segments, window)
}

fn append_text(target: &mut String, text: &str) {
    if !target.is_empty() && !text.starts_with(['.', ',', '!', '?', '…', ':', ';', ')', ']']) {
        target.push(' ');
    }
    target.push_str(text);
}

/// Keep point-aligned words instead of discarding a zero-length sentence.
/// Coincident sentences share one cue. A standalone point gets at most 200ms,
/// bounded by the next cue and the clip; an endpoint uses the preceding gap.
fn finish_cues(segments: Vec<Segment>, window: f64) -> Result<Vec<Segment>, String> {
    let mut cues: Vec<Segment> = Vec::with_capacity(segments.len());
    for segment in segments {
        if let Some(previous) = cues.last_mut() {
            if segment.start < previous.start {
                return Err("ElevenLabs returned unordered word timestamps.".to_owned());
            }
            if segment.start == previous.start {
                append_text(&mut previous.text, &segment.text);
                previous.end = previous.end.max(segment.end);
                continue;
            }
        }
        cues.push(segment);
    }
    // Word alignments can overlap slightly; subtitle clips must not.
    for index in 0..cues.len() {
        let limit = cues.get(index + 1).map_or(window, |next| next.start);
        let previous_end = if index == 0 { 0.0 } else { cues[index - 1].end };
        let current = &mut cues[index];
        current.end = current.end.min(limit);
        if current.end == current.start {
            if current.start < limit {
                current.end = (current.start + 0.2).min(limit);
            } else {
                current.start = (current.start - 0.2).max(previous_end);
            }
        }
    }
    // If the preceding cue already reaches the clip's endpoint, put the
    // final point's text there rather than creating an overlapping clip.
    if cues.last().is_some_and(|last| last.start == last.end) {
        let last = cues.pop().expect("last cue exists");
        if let Some(previous) = cues.last_mut() {
            append_text(&mut previous.text, &last.text);
        } else {
            return Err("ElevenLabs returned text without usable word timestamps.".to_owned());
        }
    }
    Ok(cues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_requested_audio_window_is_prepared() {
        let mut source = tempfile::NamedTempFile::new().unwrap();
        let samples = 32_000u32;
        let bytes = samples * 2;
        source.write_all(b"RIFF").unwrap();
        source.write_all(&(36 + bytes).to_le_bytes()).unwrap();
        source.write_all(b"WAVEfmt ").unwrap();
        for field in [16u32, 0x0001_0001, 16_000, 32_000, 0x0010_0002] {
            source.write_all(&field.to_le_bytes()).unwrap();
        }
        source.write_all(b"data").unwrap();
        source.write_all(&bytes.to_le_bytes()).unwrap();
        for n in 0..samples {
            let sample = if n < 16_000 { 0i16 } else { 1000 };
            source.write_all(&sample.to_le_bytes()).unwrap();
        }
        let request = TranscribeRequest {
            path: source.path().to_string_lossy().into(),
            audio_stream: Some(0),
            source_start: 1.0,
            window: 0.5,
            model_id: String::new(),
        };
        let (mut audio, size) = prepare_audio(&request, &AtomicBool::new(false)).unwrap();
        assert_eq!(size, 16_000, "half a second of mono 16kHz i16");
        let mut pcm = Vec::new();
        audio.read_to_end(&mut pcm).unwrap();
        assert!(
            pcm.chunks_exact(2)
                .all(|pair| i16::from_le_bytes([pair[0], pair[1]]) == 1000)
        );
    }

    #[test]
    fn russian_words_become_phrases_with_source_relative_times() {
        let transcript = serde_json::from_value(serde_json::json!({
            "text": "Привет, мир! Дальше пауза и фраза.", "words": [
                {"type":"audio_event","text":"(music)","start":0,"end":0.2},
                {"type":"word","text":"Привет,","start":0.5,"end":1},
                {"type":"spacing","text":" "},
                {"type":"word","text":"мир!","start":1.1,"end":1.5},
                {"type":"word","text":"Дальше","start":1.8,"end":2.2},
                {"type":"word","text":"пауза","start":2.3,"end":2.7},
                {"type":"word","text":"и","start":4,"end":4.2},
                {"type":"word","text":"фраза.","start":4.3,"end":5.2}
            ]
        }))
        .unwrap();
        assert_eq!(
            captions(transcript, 5.0).unwrap(),
            vec![
                Segment {
                    start: 0.5,
                    end: 1.5,
                    text: "Привет, мир!".into()
                },
                Segment {
                    start: 1.8,
                    end: 2.7,
                    text: "Дальше пауза".into()
                },
                Segment {
                    start: 4.0,
                    end: 5.0,
                    text: "и фраза.".into()
                },
            ]
        );
    }

    #[test]
    fn point_timestamps_preserve_every_word_and_keep_cues_inside_the_clip() {
        let transcript = serde_json::from_value(serde_json::json!({
            "text": "Да. Это работает. Точно! И потом. Конец!", "words": [
                {"type":"word","text":"Да.","start":0,"end":0},
                {"type":"word","text":"Это","start":0,"end":0},
                {"type":"word","text":"работает.","start":0.1,"end":0.7},
                {"type":"word","text":"Точно!","start":1,"end":1},
                {"type":"word","text":"И","start":1.3,"end":1.3},
                {"type":"word","text":"потом.","start":1.4,"end":1.8},
                {"type":"word","text":"Конец!","start":2,"end":2}
            ]
        }))
        .unwrap();
        let cues = captions(transcript, 2.0).unwrap();
        assert_eq!(
            cues.iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            "Да. Это работает. Точно! И потом. Конец!"
        );
        assert!(
            cues.iter()
                .all(|c| c.start >= 0.0 && c.end > c.start && c.end <= 2.0)
        );
        assert!(cues.windows(2).all(|pair| pair[0].end <= pair[1].start));
        assert_eq!((cues[0].start, cues[0].end), (0.0, 0.7));
        assert_eq!(cues.last().unwrap().end, 2.0);
    }

    #[test]
    fn coincident_sentences_and_a_short_clip_do_not_lose_text() {
        let transcript = serde_json::from_value(serde_json::json!({
            "text": "Да! Нет?", "words": [
                {"type":"word","text":"Да!","start":0,"end":0.01},
                {"type":"word","text":"Нет?","start":0,"end":0}
            ]
        }))
        .unwrap();
        assert_eq!(
            captions(transcript, 0.01).unwrap(),
            vec![Segment {
                start: 0.0,
                end: 0.01,
                text: "Да! Нет?".into(),
            }]
        );
    }

    #[test]
    fn a_standalone_point_and_an_endpoint_word_get_visible_bounded_cues() {
        for (window, words, expected) in [
            (
                0.1,
                serde_json::json!([
                    {"type":"word","text":"Да!","start":0,"end":0}
                ]),
                "Да!",
            ),
            (
                1.0,
                serde_json::json!([
                    {"type":"word","text":"Да!","start":1,"end":1}
                ]),
                "Да!",
            ),
            (
                1.0,
                serde_json::json!([
                    {"type":"word","text":"Фраза.","start":0,"end":1},
                    {"type":"word","text":"Конец!","start":1,"end":1}
                ]),
                "Фраза. Конец!",
            ),
            (
                1.0,
                serde_json::json!([
                    {"type":"word","text":"Да!","start":0.9,"end":0.9},
                    {"type":"word","text":"Нет!","start":1,"end":1}
                ]),
                "Да! Нет!",
            ),
        ] {
            let transcript = serde_json::from_value(serde_json::json!({
                "text": expected, "words": words,
            }))
            .unwrap();
            let cues = captions(transcript, window).unwrap();
            assert_eq!(
                cues.iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                expected
            );
            assert!(
                cues.iter()
                    .all(|c| c.start >= 0.0 && c.end > c.start && c.end <= window)
            );
            assert!(cues.windows(2).all(|pair| pair[0].end <= pair[1].start));
        }
    }

    #[test]
    fn missing_nonfinite_negative_and_reversed_word_timestamps_are_still_errors() {
        for (start, end) in [
            (None, Some(1.0)),
            (Some(0.0), None),
            (Some(f64::NAN), Some(1.0)),
            (Some(0.0), Some(f64::INFINITY)),
            (Some(-0.001), Some(1.0)),
            (Some(1.0), Some(0.9)),
        ] {
            let transcript = Transcript {
                text: "слово".into(),
                words: vec![Word {
                    text: "слово".into(),
                    kind: "word".into(),
                    start,
                    end,
                }],
            };
            assert!(captions(transcript, 2.0).is_err());
        }
    }

    #[test]
    fn refuses_missing_timing_and_caps_long_phrases() {
        assert!(
            captions(
                Transcript {
                    text: "text".into(),
                    words: vec![]
                },
                20.0
            )
            .is_err()
        );
        let words = (0..20)
            .map(|n| Word {
                text: "слово".into(),
                kind: "word".into(),
                start: Some(n as f64),
                end: Some(n as f64 + 0.5),
            })
            .collect();
        let cues = captions(
            Transcript {
                text: "слова".into(),
                words,
            },
            20.0,
        )
        .unwrap();
        assert_eq!(cues.len(), 4);
        assert!(cues.iter().all(|cue| cue.end - cue.start <= 6.0));
        assert_eq!(
            cues.iter()
                .map(|cue| cue.text.split_whitespace().count())
                .sum::<usize>(),
            20
        );
    }

    #[test]
    fn multipart_and_key_debug_do_not_expose_paths_or_secrets() {
        let (body, _) = multipart(Options {
            language: Some("ru"),
            clean_speech: true,
        })
        .unwrap();
        assert!(body.contains("scribe_v2") && body.contains("\r\nru\r\n"));
        assert!(body.contains("filename=\"audio.pcm\""));
        assert!(
            multipart(Options {
                language: Some("ru\r\nevil"),
                ..Options::default()
            })
            .is_err()
        );
        assert!(!format!("{:?}", ApiKey::new("private-key".into())).contains("private-key"));
        assert!(ApiKey::new("bad\nkey".into()).validate().is_err());
    }

    #[test]
    fn http_upload_uses_scribe_and_handles_auth_errors_without_echoing_body() {
        use std::net::TcpListener;
        for status in [200, 401, 429] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!(
                "http://{}/v1/speech-to-text",
                listener.local_addr().unwrap()
            );
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let headers = String::from_utf8(request).unwrap();
                assert!(headers.to_lowercase().contains("xi-api-key: test-key"));
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|value| value.parse().unwrap())
                    })
                    .unwrap();
                let mut body = vec![0; length];
                socket.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                assert!(body.contains("scribe_v2") && body.contains("\r\nru\r\n"));
                assert!(body.contains("AUDIO") && body.ends_with(&format!("--{BOUNDARY}--\r\n")));
                let json = if status == 200 {
                    r#"{"text":"","words":[]}"#
                } else {
                    "private server detail test-key"
                };
                write!(socket, "HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len()).unwrap();
            });
            let result = send(
                &endpoint,
                &b"AUDIO"[..],
                5,
                &ApiKey::new("test-key".into()),
                Options {
                    language: Some("ru"),
                    clean_speech: true,
                },
                &AtomicBool::new(false),
            );
            assert_eq!(result.is_ok(), status == 200);
            if let Err(error) = result {
                assert!(!error.contains("test-key"));
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn cancellation_prevents_upload() {
        let result = send(
            "http://127.0.0.1:1",
            &b""[..],
            0,
            &ApiKey::new("test-key".into()),
            Options::default(),
            &AtomicBool::new(true),
        );
        assert!(result.err().unwrap().contains("cancelled"));
    }

    #[test]
    fn clean_speech_sets_no_verbatim_without_enabling_audio_events() {
        for clean_speech in [false, true] {
            let (body, _) = multipart(Options {
                clean_speech,
                language: None,
            })
            .unwrap();
            assert!(body.contains(&format!("name=\"no_verbatim\"\r\n\r\n{clean_speech}\r\n")));
            assert!(body.contains("name=\"tag_audio_events\"\r\n\r\nfalse\r\n"));
            assert!(!body.contains("language_code"));
        }
    }
}
