use crate::{
    clock,
    editor::{Clip, ClipKind, Project, Source, Store, Track, private_write},
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};
use uuid::Uuid;

pub const WAVEFORM_RATE: u32 = 200;
const SAMPLE: u32 = clock::TICKS_PER_SECOND / WAVEFORM_RATE;
const SCAN: u32 = clock::TICKS_PER_SECOND / 20;
const ATTACK: u32 = 3 * SCAN;
const RELEASE: u32 = 10 * SCAN;

fn linear(db: f64) -> f64 {
    10_f64.powf(db / 20.0)
}

struct Waveform {
    peaks: Vec<f32>,
    rms: Vec<f32>,
}

fn cache_path(store: &Store, source: &Source) -> PathBuf {
    store
        .media(source.storage_id(), source.video)
        .with_extension("waveform-200-v2")
}

fn cached(store: &Store, source: &Source) -> Option<Waveform> {
    let data = std::fs::read(cache_path(store, source)).ok()?;
    let chunks = data.chunks_exact(8);
    if !chunks.remainder().is_empty() {
        return None;
    }
    let (peaks, rms) = chunks
        .map(|bytes| {
            let peak = f32::from_le_bytes(bytes[..4].try_into().unwrap());
            let rms = f32::from_le_bytes(bytes[4..].try_into().unwrap());
            (peak, rms)
        })
        .unzip();
    Some(Waveform { peaks, rms })
}

async fn waveform(store: &Store, source: &Source) -> Result<Waveform> {
    if let Some(saved) = cached(store, source) {
        return Ok(saved);
    }
    let _slot = store.audio_analysis_slot.lock().await;
    if let Some(saved) = cached(store, source) {
        return Ok(saved);
    }
    let original = store.original(source.storage_id());
    let input = if original.is_file() {
        original
    } else {
        store.media(source.storage_id(), source.video)
    };
    // Decode without downmixing so mono and multichannel peaks retain the source amplitude.
    let channels = timeout(
        Duration::from_secs(30),
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file",
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=channels",
                "-of",
                "csv=p=0",
            ])
            .arg(&input)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    let channels: usize = String::from_utf8(channels.stdout)?
        .trim()
        .parse()
        .context("Read the source audio channels.")?;
    ensure!((1..=32).contains(&channels), "Unsupported channel count.");
    let mut decode = Command::new("ffmpeg");
    decode
        .args([
            "-v",
            "error",
            "-nostdin",
            "-protocol_whitelist",
            "file",
            "-i",
        ])
        .arg(input)
        .args([
            "-map",
            "0:a:0",
            "-t",
            &crate::media::seconds(source.duration),
            "-ar",
            "48000",
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    let mut child = decode
        .spawn()
        .context("Decode audio to measure its levels.")?;
    let mut pipe = tokio::io::BufReader::new(child.stdout.take().context("Read decoded audio.")?);
    let measured = timeout(Duration::from_secs(900), async {
        let frame_bytes = channels * 4;
        let mut block = vec![0; (48_000 / WAVEFORM_RATE) as usize * frame_bytes];
        let mut measured = Waveform {
            peaks: vec![],
            rms: vec![],
        };
        loop {
            let mut used = 0;
            while used < block.len() {
                let read = pipe.read(&mut block[used..]).await?;
                if read == 0 {
                    break;
                }
                used += read;
            }
            if used == 0 {
                break;
            }
            let mut peak = 0.0_f32;
            let mut squares = 0.0_f64;
            for frame in block[..used].chunks_exact(frame_bytes) {
                let loudest = frame
                    .chunks_exact(4)
                    .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()).abs())
                    .filter(|value| value.is_finite())
                    .fold(0.0, f32::max);
                peak = peak.max(loudest);
                squares += f64::from(loudest).powi(2);
            }
            measured.peaks.push(peak);
            measured
                .rms
                .push((squares / (used / frame_bytes).max(1) as f64).sqrt() as f32);
        }
        ensure!(
            child.wait().await?.success(),
            "Could not measure this audio."
        );
        Ok::<_, anyhow::Error>(measured)
    })
    .await??;
    let bytes = measured
        .peaks
        .iter()
        .zip(&measured.rms)
        .flat_map(|(peak, rms)| peak.to_le_bytes().into_iter().chain(rms.to_le_bytes()))
        .collect::<Vec<_>>();
    private_write(&cache_path(store, source), &bytes)?;
    Ok(measured)
}

pub async fn waveform_levels(store: &Store, source: &Source) -> Result<Vec<u8>> {
    Ok(waveform(store, source)
        .await?
        .rms
        .iter()
        .flat_map(|level| level.to_le_bytes())
        .collect())
}

fn levels(clip: &Clip, waveform: &Waveform) -> Option<(f64, f64)> {
    let lo = (clip.start / SAMPLE) as usize;
    let hi = ((clip.end / SAMPLE) as usize).min(waveform.peaks.len());
    let samples = waveform.peaks.get(lo..hi)?;
    let (power, count, peak) = samples.iter().filter(|&&peak| peak > 0.003).fold(
        (0.0, 0, 0_f64),
        |(power, count, highest), &peak| {
            let peak = f64::from(peak);
            (power + peak * peak, count + 1, highest.max(peak))
        },
    );
    (count > 0).then(|| {
        (
            10.0 * (power / f64::from(count)).log10(),
            20.0 * peak.log10(),
        )
    })
}

pub async fn even_levels(
    store: &Store,
    project: &mut Project,
    selected: &[Uuid],
    match_output: bool,
) -> Result<Vec<(Uuid, f64)>> {
    let mut found = vec![];
    let mut waves = HashMap::new();
    for clip in project.timeline.iter().filter(|clip| {
        clip.kind == ClipKind::Audio && (selected.is_empty() || selected.contains(&clip.id))
    }) {
        let Some(track) = project
            .sequence
            .tracks
            .iter()
            .find(|track| track.name == clip.track() && !track.locked)
        else {
            continue;
        };
        let Some(source) = project
            .source(clip.source)
            .filter(|source| !project.library.missing.contains_key(&source.id))
        else {
            continue;
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = waves.entry(source.id) {
            entry.insert(waveform(store, source).await?);
        }
        if let Some((level, peak)) = levels(clip, &waves[&source.id]) {
            let track_db = if match_output { track.gain_db } else { 0.0 };
            found.push((
                clip.id,
                level,
                peak,
                track_db,
                level + clip.effects.gain_db + track_db,
            ));
        }
    }
    if found.is_empty() {
        return Ok(vec![]);
    }
    let target = if match_output {
        found.sort_by(|a, b| a.4.total_cmp(&b.4));
        let n = found.len();
        (found[(n - 1) / 2].4 + found[n / 2].4) / 2.0
    } else {
        -20.0
    };
    let mut changes = vec![];
    for (id, level, peak, track, _) in found {
        let gain = target - level - track;
        let gain = if match_output {
            gain.min(-1.0 - peak - track)
        } else {
            gain
        };
        let gain = ((gain * 10.0).round() / 10.0).clamp(-24.0, 12.0);
        if let Some(clip) = project.timeline.iter_mut().find(|clip| clip.id == id)
            && (clip.effects.gain_db - gain).abs() > f64::EPSILON
        {
            clip.effects.gain_db = gain;
            changes.push((id, gain));
        }
    }
    Ok(changes)
}

pub fn audible(project: &Project, track: &Track) -> bool {
    !track.muted
        && (track.solo
            || !project
                .sequence
                .tracks
                .iter()
                .any(|track| track.name.starts_with('A') && track.solo))
}

#[derive(Clone, Serialize)]
pub struct Duck {
    pub low: f64,
    pub spans: Vec<[u32; 2]>,
}

impl Duck {
    pub fn gain(&self, at: u32) -> f64 {
        let next = self.spans.partition_point(|span| span[0] <= at);
        let past = next
            .checked_sub(1)
            .map(|n| {
                let tail = at.saturating_sub(self.spans[n][1]);
                f64::from(tail) / f64::from(RELEASE)
            })
            .unwrap_or(1.0);
        let ahead = self
            .spans
            .get(next)
            .map(|span| f64::from(span[0] - at) / f64::from(ATTACK))
            .unwrap_or(1.0);
        self.low + (1.0 - self.low) * past.min(ahead).min(1.0)
    }

    // A 200 Hz gain envelope is small enough to apply sample-wise to a mix
    // without putting an arbitrarily large expression in FFmpeg's filter graph.
    pub async fn write_control(&self, path: &std::path::Path, duration: u32) -> Result<()> {
        let bytes = (0..=duration.div_ceil(SAMPLE).saturating_add(32))
            .flat_map(|sample| (self.gain(sample.saturating_mul(SAMPLE)) as f32).to_le_bytes())
            .collect::<Vec<_>>();
        tokio::fs::write(path, &bytes).await?;
        Ok(())
    }
}

pub async fn mix(store: &Store, project: &Project) -> Result<BTreeMap<String, Duck>> {
    let mut mix = BTreeMap::new();
    let mut activity = HashMap::new();
    for track in project
        .sequence
        .tracks
        .iter()
        .filter(|track| track.name.starts_with('A') && audible(project, track))
    {
        let Some(voice) = project.sequence.tracks.iter().find(|voice| {
            Some(voice.id) == track.duck_under
                && voice.id != track.id
                && voice.name.starts_with('A')
                && audible(project, voice)
        }) else {
            continue;
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = activity.entry(voice.id) {
            entry.insert(spans(store, project, voice).await?);
        }
        if !activity[&voice.id].is_empty() {
            mix.insert(
                track.name.clone(),
                Duck {
                    low: linear(track.duck_db),
                    spans: activity[&voice.id].clone(),
                },
            );
        }
    }
    Ok(mix)
}

async fn spans(store: &Store, project: &Project, voice: &Track) -> Result<Vec<[u32; 2]>> {
    let mut spans = vec![];
    let mut waves = HashMap::new();
    for clip in project.timeline.iter().filter(|clip| {
        clip.kind == ClipKind::Audio
            && !clip.mute
            && !clip.effects.freeze
            && clip.track() == voice.name
    }) {
        let Some(source) = project
            .source(clip.source)
            .filter(|source| !project.library.missing.contains_key(&source.id))
        else {
            continue;
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = waves.entry(source.id) {
            entry.insert(waveform(store, source).await?);
        }
        let wave = &waves[&source.id].peaks;
        let start = project.placement(clip.id).unwrap_or(0);
        let duration = clip.duration();
        let mut open = None;
        let mut elapsed = 0;
        while elapsed < duration {
            let a = clip.visible_source_at(elapsed);
            let b = clip.visible_source_at(elapsed.saturating_add(SCAN).min(duration));
            let lo = (a.min(b) / SAMPLE) as usize;
            let hi = ((a.max(b) / SAMPLE) as usize + 1).min(wave.len());
            let peak = wave
                .get(lo..hi)
                .unwrap_or_default()
                .iter()
                .copied()
                .fold(0.0, f32::max);
            let active =
                f64::from(peak) * linear(clip.effects.gain_db) * clip.fade_gain(elapsed, duration)
                    > 0.03;
            let at = start.saturating_add(elapsed);
            if active {
                open.get_or_insert(at);
            } else if let Some(from) = open.take() {
                spans.push([from, at]);
            }
            elapsed = elapsed.saturating_add(SCAN);
        }
        if let Some(from) = open {
            spans.push([from, start.saturating_add(duration)]);
        }
    }
    spans.sort_unstable();
    let mut merged: Vec<[u32; 2]> = vec![];
    for span in spans {
        if let Some(last) = merged.last_mut()
            && span[0].saturating_sub(last[1]) < 7 * SCAN
        {
            last[1] = last[1].max(span[1]);
        } else {
            merged.push(span);
        }
    }
    Ok(merged)
}
