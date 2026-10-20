// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Export the first few seconds of a saved project without changing it.
//! Usage: export_bench PROJECT OUTPUT CODEC SECONDS [software]

use std::sync::{Arc, atomic::AtomicBool};
use std::time::Instant;

use concat_host::{dirs::AppDirs, export, session::Session, titles::Titles};

struct Logger;
impl log::Log for Logger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        if record.level() <= log::Level::Info {
            eprintln!("{}", record.args());
        }
    }
    fn flush(&self) {}
}

fn main() -> Result<(), String> {
    log::set_logger(&Logger).map_err(|error| error.to_string())?;
    log::set_max_level(log::LevelFilter::Info);
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        return Err("PROJECT OUTPUT CODEC SECONDS [software]".into());
    }
    let seconds: f64 = args[4].parse().map_err(|_| "invalid seconds")?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("invalid seconds".into());
    }
    if std::path::Path::new(&args[2]).exists() {
        return Err("output already exists".into());
    }
    let session = Session::open(
        &args[1],
        concat_project::doc::DocumentSettings {
            name: "Benchmark".into(),
            width: 1920,
            height: 1080,
            rate_num: 24,
            rate_den: 1,
        },
    )?;
    let settings = session.settings();
    let mut project = session.project().clone();
    project
        .active_mut()
        .clips
        .retain(|clip| clip.start < seconds);
    for clip in &mut project.active_mut().clips {
        let clip = Arc::make_mut(clip);
        clip.duration = clip.duration.min(seconds - clip.start);
    }
    let cache = std::path::Path::new(&args[2])
        .parent()
        .ok_or("output needs a folder")?
        .join("title-cache");
    let titles = Titles::new(&AppDirs {
        data: cache.clone(),
        config: cache,
    });
    let started = Instant::now();
    let painted = titles.clips(&project, settings.width, settings.height);
    eprintln!("title preparation: {:.3}s", started.elapsed().as_secs_f64());
    let spec = export::ExportSpec {
        output: args[2].clone(),
        crf: 20,
        preset: "veryfast".into(),
        codec: export::VideoCodec::parse(&args[3]).ok_or("invalid codec")?,
        software: args.get(5).is_some_and(|arg| arg == "software"),
        rate_mode: export::RateMode::Average,
        bitrate_kbps: 26_200,
        ..Default::default()
    };
    let mut request = export::request(
        &session,
        &spec,
        painted.into_iter().map(|title| title.clip).collect(),
    );
    request.clips.retain(|clip| clip.start < seconds);
    for clip in &mut request.clips {
        clip.duration = clip.duration.min(seconds - clip.start);
    }
    let started = Instant::now();
    export::run(&request, &AtomicBool::new(false), |_| {})?;
    eprintln!(
        "export: {:.3}s for {:.3}s of video",
        started.elapsed().as_secs_f64(),
        seconds
    );
    Ok(())
}
