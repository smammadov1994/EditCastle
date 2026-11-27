mod accounts;
mod app;
mod assets;
mod attached_agents;
mod audio;
mod auto_edit;
mod billing;
mod caption_export;
mod captions;
mod clock;
mod director_preview;
mod director_prompt;
mod draft_merge;
mod editor;
mod export_queue;
mod grade;
mod library;
mod mcp_activity;
mod mcp_bridge;
mod mcp_bundle;
mod mcp_setup;
mod media;
mod playback;
mod provider;
mod provider_stream;
mod public_web;
mod render;
mod render_placement;
mod retime;
mod room;
mod search;
mod source_color;
mod speech;
mod studio_history;
mod timeline;
mod tools;
mod transform;
mod transition;
mod vision;

use anyhow::Result;
use std::env;

pub const FPS: u32 = 24;
pub const MAX_SHOTS: usize = 24;

pub fn timing(seconds: f64) -> Option<(u32, u32, u32)> {
    if !(1.0..=15.0).contains(&seconds) {
        return None;
    }
    let visible = (seconds * f64::from(FPS)).round() as u32;
    let padded = visible.max(124);
    let rendered = padded + (5 + 17 - padded % 17) % 17;
    Some((visible, rendered, rendered.div_ceil(FPS)))
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next();
    if command.as_deref() == Some("mcp-stdio") {
        let endpoint = args
            .next()
            .filter(|_| args.next().is_none())
            .ok_or_else(|| {
                anyhow::anyhow!("Usage: editcastle-web mcp-stdio <project MCP endpoint>")
            })?;
        return mcp_bridge::run(&endpoint).await;
    }
    if command.as_deref() == Some("source-color") {
        for sample in args {
            if let Some([r, g, b]) = source_color::replay(&sample) {
                println!("{r:.7} {g:.7} {b:.7}");
            } else {
                println!("invalid color");
            }
        }
        return Ok(());
    }
    if command.as_deref() == Some("auto-plan") {
        let input = args.next().unwrap_or_default();
        println!("{}", auto_edit::planner::replay(&input)?);
        return Ok(());
    }
    if command.as_deref() == Some("import-room") {
        use std::path::PathBuf;
        let file = args
            .next()
            .ok_or_else(|| anyhow::anyhow!("Pass a native Editroom .room file."))?;
        let data = env::var_os("EDITROOM_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("studio-data"));
        let project = room::migrate(std::path::Path::new(&file), &data).await?;
        println!("Imported project {project}");
        return Ok(());
    }
    if command.as_deref() == Some("timing") {
        for value in args {
            match value.parse().ok().and_then(timing) {
                Some((visible, rendered, credits)) => println!("{visible} {rendered} {credits}"),
                None => println!("invalid duration"),
            }
        }
        return Ok(());
    }
    if command.as_deref() == Some("sequence-snap") {
        for raw in args {
            let sample = raw.split_once(',').and_then(|(rate, time)| {
                let rate: f64 = rate.parse().ok()?;
                let time: f64 = time.parse().ok()?;
                (clock::EDIT_RATES.contains(&rate) && (0.0..7200.0).contains(&time))
                    .then_some((rate, time))
            });
            if let Some((rate, time)) = sample {
                let step = clock::step(rate);
                println!("{}", clock::snap_seconds(time, step) / step);
            } else {
                println!("invalid edit time");
            }
        }
        return Ok(());
    }
    if command.as_deref() == Some("fade-envelope") {
        for raw in args {
            let sample = raw.split_once(',').and_then(|(name, progress)| {
                let curve: editor::FadeCurve =
                    serde_json::from_value(serde_json::json!(name)).ok()?;
                let progress: f64 = progress.parse().ok()?;
                progress.is_finite().then_some((curve, progress))
            });
            if let Some((curve, progress)) = sample {
                println!("{:.6}", curve.apply(progress));
            } else {
                println!("invalid fade");
            }
        }
        return Ok(());
    }
    if command.as_deref() == Some("duck-envelope") {
        for raw in args {
            let values = raw
                .split(',')
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let [db, at, bounds @ ..] = values.as_slice() else {
                anyhow::bail!("Pass reduction, sample time, and start/end pairs.");
            };
            anyhow::ensure!(
                bounds.len() % 2 == 0 && values.iter().all(|value| value.is_finite()),
                "Pass reduction, sample time, and start/end pairs."
            );
            let duck = audio::Duck {
                low: 10_f64.powf(db / 20.0),
                spans: bounds
                    .chunks_exact(2)
                    .map(|pair| [clock::ticks(pair[0]), clock::ticks(pair[1])])
                    .collect(),
            };
            println!("{:.6}", duck.gain(clock::ticks(*at)));
        }
        return Ok(());
    }
    if command.as_deref() == Some("speed-ramp") {
        for raw in args {
            let values = raw
                .split(',')
                .filter_map(|value| value.parse::<f64>().ok())
                .collect::<Vec<_>>();
            let valid = values.len() == 10
                && values.iter().all(|value| value.is_finite())
                && values[4..7].iter().all(|value| *value > 0.0)
                && values[0] <= values[1]
                && values[1] <= values[2]
                && values[2] <= values[3];
            if !valid {
                println!("invalid ramp");
                continue;
            }
            let ramp = retime::Ramp {
                from: values[1],
                to: values[2],
                ease_in: values[1] - values[0],
                ease_out: values[3] - values[2],
                start_speed: values[4],
                end_speed: values[5],
                middle_speed: values[6],
            };
            println!(
                "{:.6} {:.6}",
                ramp.span(values[7], values[8]),
                ramp.source(values[7], values[9])
            );
        }
        return Ok(());
    }
    app::serve().await
}
