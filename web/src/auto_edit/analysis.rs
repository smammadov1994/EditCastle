use crate::{
    clock,
    editor::{Source, Store, private_write},
    media,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{process::Command, time::timeout};

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(super) struct Sample {
    pub time: f64,
    pub luma: f64,
    pub sharpness: f64,
    pub motion: f64,
    pub color: f64,
    pub faces: usize,
    pub aesthetic: Option<f64>,
    pub utility: Option<bool>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Analysis {
    pub duration: f64,
    pub created: Option<String>,
    pub samples: Vec<Sample>,
}

pub(super) fn original(store: &Store, source: &Source) -> PathBuf {
    let id = source.storage_id();
    let path = store.original(id);
    if path.is_file() {
        path
    } else {
        store.media(id, source.video)
    }
}

pub(super) async fn decode(path: &Path, options: &[&str]) -> Result<Vec<u8>> {
    let output = timeout(
        Duration::from_secs(240),
        Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-v",
                "error",
                "-protocol_whitelist",
                "file",
                "-threads",
                "2",
                "-filter_threads",
                "2",
                "-i",
            ])
            .arg(path)
            .args(options)
            .arg("pipe:1")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "FFmpeg could not analyze this file."
    );
    Ok(output.stdout)
}

fn png_frames(mut buffer: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        if !buffer.starts_with(b"\x89PNG\r\n\x1a\n") {
            return None;
        }
        let mut offset = 8usize;
        loop {
            let header = buffer.get(offset..offset + 8)?;
            let count = u32::from_be_bytes(header[..4].try_into().ok()?) as usize;
            offset = offset.checked_add(count + 12)?;
            if &header[4..8] == b"IEND" {
                let (frame, rest) = buffer.split_at_checked(offset)?;
                buffer = rest;
                return Some(frame);
            }
        }
    })
}

fn measure(frame: &[u8], previous: &mut Vec<f64>, time: f64) -> Result<Sample> {
    let decoder = png::Decoder::new(Cursor::new(frame));
    let mut reader = decoder.read_info()?;
    let mut bytes = vec![
        0;
        reader
            .output_buffer_size()
            .context("Unsupported analysis frame.")?
    ];
    let info = reader.next_frame(&mut bytes)?;
    ensure!(
        info.color_type == png::ColorType::Rgb && info.bit_depth == png::BitDepth::Eight,
        "Expected an RGB frame."
    );
    let sample = |x: usize, y: usize, width: usize, height: usize| {
        let bx = (x * info.width as usize + info.width as usize / 2) / width;
        let by = (y * info.height as usize + info.height as usize / 2) / height;
        let offset = 3 * (by * info.width as usize + bx);
        [bytes[offset], bytes[offset + 1], bytes[offset + 2]].map(f64::from)
    };
    let (w, h) = (160, 90);
    let gray: Vec<_> = (0..w * h)
        .map(|i| {
            let [r, g, b] = sample(i % w, i / w, w, h);
            r * 0.2126 + g * 0.7152 + b * 0.0722
        })
        .collect();
    let luma = gray.iter().sum::<f64>() / (255 * w * h) as f64;
    let mut sum = 0.0;
    let mut squares = 0.0;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let edge = gray[i - 1] + gray[i + 1] + gray[i - w] + gray[i + w] - 4.0 * gray[i];
            sum += edge;
            squares += edge * edge;
        }
    }
    let n = ((w - 2) * (h - 2)) as f64;
    let sharpness = (squares / n - (sum / n).powi(2)).max(0.0);
    let motion = gray
        .iter()
        .zip(previous.iter())
        .map(|(a, b)| (a - b).abs())
        .sum::<f64>()
        / (255 * w * h) as f64;
    *previous = gray;
    let mut rg = (0.0, 0.0);
    let mut yb = (0.0, 0.0);
    let (w, h) = (48, 27);
    for i in 0..w * h {
        let [r, g, b] = sample(i % w, i / w, w, h);
        let first = r - g;
        let second = 0.5 * (r + g) - b;
        rg.0 += first;
        rg.1 += first * first;
        yb.0 += second;
        yb.1 += second * second;
    }
    let n = (w * h) as f64;
    let mean = (rg.0 / n).hypot(yb.0 / n);
    let variance = (rg.1 + yb.1) / n - mean * mean;
    let color = ((variance.max(0.0).sqrt() + 0.3 * mean) / 110.0).min(1.5);
    Ok(Sample {
        time,
        luma,
        sharpness,
        motion,
        color,
        faces: 0,
        aesthetic: None,
        utility: None,
    })
}

pub(super) async fn footage(store: &Store, source: &Source) -> Result<Analysis> {
    let path = original(store, source);
    let normalized = crate::source_color::original_profile(store, source, &path)
        .await?
        .needs_conversion();
    let cache = store
        .media(source.storage_id(), true)
        .with_extension(if normalized {
            "auto-edit-sdr-frames-v1.json"
        } else {
            "auto-edit-frames-v1.json"
        });
    if let Ok(data) = tokio::fs::read(&cache).await
        && let Ok(saved) = serde_json::from_slice(&data)
    {
        return Ok(saved);
    }
    let data = media::details(&path).await?;
    let created = data["format"]["tags"]["creation_time"]
        .as_str()
        .map(str::to_owned);
    let duration = if source.image {
        5.0
    } else {
        clock::seconds(source.duration)
    };
    let step = (duration / 60.0).max(0.25);
    let single_frame = source.image || duration < 0.17;
    let filters = if single_frame {
        "scale=320:320:force_original_aspect_ratio=decrease".into()
    } else {
        format!(
            "trim=start={:.9},setpts=PTS-STARTPTS,fps={:.9}:round=up,scale=320:320:force_original_aspect_ratio=decrease",
            step / 2.0,
            1.0 / step
        )
    };
    let count = if single_frame { "1" } else { "60" };
    let pictures = if normalized {
        crate::source_color::playback(store, source).await?
    } else {
        path
    };
    let bytes = decode(
        &pictures,
        &[
            "-an",
            "-vf",
            &filters,
            "-frames:v",
            count,
            "-pix_fmt",
            "rgb24",
            "-c:v",
            "png",
            "-f",
            "image2pipe",
        ],
    )
    .await?;
    tokio::task::spawn_blocking(move || {
        let mut previous = Vec::new();
        let mut samples: Vec<Sample> = Vec::new();
        for (i, frame) in png_frames(&bytes).enumerate() {
            let time = if single_frame {
                0.0
            } else {
                (i as f64 + 0.5) * step
            };
            if !single_frame && time >= duration - 0.04 {
                break;
            }
            let Ok(mut sample) = measure(frame, &mut previous, time) else {
                continue;
            };
            let faces = samples.len() % 2 == 0;
            let aesthetics = samples.len() % ((1.0 / step).round() as usize).max(1) == 0;
            if let Some(last) = samples.last() {
                sample.faces = last.faces;
                sample.aesthetic = last.aesthetic;
                sample.utility = last.utility;
            }
            vision(frame, &mut sample, faces, aesthetics);
            samples.push(sample);
        }
        ensure!(
            !samples.is_empty(),
            "No frames were available for Auto Edit."
        );
        let analysis = Analysis {
            duration,
            created,
            samples,
        };
        if let Ok(bytes) = serde_json::to_vec(&analysis) {
            let _ = private_write(&cache, &bytes);
        }
        Ok(analysis)
    })
    .await?
}

#[cfg(target_os = "macos")]
fn vision(bytes: &[u8], sample: &mut Sample, faces: bool, aesthetics: bool) {
    use objc2::{AnyThread, rc::autoreleasepool, runtime::AnyClass};
    use objc2_foundation::{NSArray, NSData, NSDictionary};
    use objc2_vision::{
        VNCalculateImageAestheticsScoresRequest, VNDetectFaceRectanglesRequest,
        VNImageRequestHandler, VNRequest,
    };

    if !faces && !aesthetics {
        return;
    }
    autoreleasepool(|_| {
        let data = NSData::with_bytes(bytes);
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &data,
            &NSDictionary::new(),
        );
        if faces {
            let request = unsafe { VNDetectFaceRectanglesRequest::new() };
            let as_request: &VNRequest = &request;
            if handler
                .performRequests_error(&NSArray::from_slice(&[as_request]))
                .is_ok()
                && let Some(results) = unsafe { request.results() }
            {
                sample.faces = (0..results.count())
                    .filter(|index| {
                        let rect = unsafe { results.objectAtIndex(*index).boundingBox() };
                        rect.size.width * rect.size.height > 0.004
                    })
                    .count();
            }
        }
        if aesthetics && AnyClass::get(c"VNCalculateImageAestheticsScoresRequest").is_some() {
            let request = unsafe { VNCalculateImageAestheticsScoresRequest::new() };
            let as_request: &VNRequest = &request;
            if handler
                .performRequests_error(&NSArray::from_slice(&[as_request]))
                .is_ok()
                && let Some(result) = unsafe { request.results() }.and_then(|r| r.firstObject())
            {
                sample.aesthetic = Some(f64::from(unsafe { result.overallScore() }));
                sample.utility = Some(unsafe { result.isUtility() });
            }
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn vision(_bytes: &[u8], _sample: &mut Sample, _faces: bool, _aesthetics: bool) {}
