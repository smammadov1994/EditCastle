use super::analysis::{decode, original};
use crate::editor::{Source, Store, private_write};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::f64::consts::TAU;

const RATE: usize = 22_050;
const N: usize = 1024;
const HOP: usize = N / 2;

#[derive(Deserialize, Serialize)]
pub(super) struct Music {
    pub duration: f64,
    pub bpm: f64,
    pub beats: Vec<f64>,
    pub bar_phase: usize,
    pub energy: Vec<f64>,
}

impl Music {
    pub fn period(&self) -> f64 {
        60.0 / self.bpm
    }
}

pub(super) async fn analyze(store: &Store, source: &Source) -> Result<Option<Music>> {
    let cache = store
        .media(source.storage_id(), false)
        .with_extension("auto-edit-beats-v1.json");
    if let Ok(data) = tokio::fs::read(&cache).await
        && let Ok(saved) = serde_json::from_slice(&data)
    {
        return Ok(saved);
    }
    let bytes = decode(
        &original(store, source),
        &["-vn", "-ac", "1", "-ar", "22050", "-f", "f32le"],
    )
    .await?;
    tokio::task::spawn_blocking(move || {
        let result = beats(&bytes);
        if let Ok(data) = serde_json::to_vec(&result) {
            let _ = private_write(&cache, &data);
        }
        Ok(result)
    })
    .await?
}

fn detrend(values: &[f64], radius: usize) -> Vec<f64> {
    let mut prefix = vec![0.0];
    for &value in values {
        prefix.push(prefix.last().unwrap() + value);
    }
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let first = i.saturating_sub(radius);
            let last = (i + radius + 1).min(values.len());
            (v - (prefix[last] - prefix[first]) / (last - first) as f64).max(0.0)
        })
        .collect()
}

fn beats(bytes: &[u8]) -> Option<Music> {
    let count = bytes.len() / 4;
    if count <= RATE * 6 {
        return None;
    }
    let fps = RATE as f64 / HOP as f64;
    let roots: Vec<_> = (0..N / 2)
        .map(|k| {
            let (sin, cos) = (-TAU * k as f64 / N as f64).sin_cos();
            (cos, sin)
        })
        .collect();
    let window: Vec<_> = (0..N)
        .map(|i| 0.5 - 0.5 * (TAU * i as f64 / N as f64).cos())
        .collect();
    let frames = (count - N) / HOP;
    let mut flux = Vec::with_capacity(frames);
    let mut bass = Vec::with_capacity(frames);
    let mut rms = Vec::with_capacity(frames);
    let mut prior = [0.0; N / 2];
    let mut signal = [(0.0, 0.0); N];
    let low_bins = (150 * N / RATE).max(2);
    for frame in 0..frames {
        let raw = &bytes[frame * HOP * 4..(frame * HOP + N) * 4];
        let mut square = 0.0;
        for (i, byte) in raw.chunks_exact(4).enumerate() {
            let amplitude = f64::from(f32::from_le_bytes(byte.try_into().unwrap())) * window[i];
            square += amplitude * amplitude;
            signal[i.reverse_bits() >> (usize::BITS - N.trailing_zeros())] = (amplitude, 0.0);
        }
        rms.push((square / N as f64).sqrt());
        let mut group = 2;
        while group <= N {
            for block in signal.chunks_exact_mut(group) {
                let (first, last) = block.split_at_mut(group / 2);
                for (k, (left, right)) in first.iter_mut().zip(last).enumerate() {
                    let (r, i) = roots[k * N / group];
                    let x = r * right.0 - i * right.1;
                    let y = r * right.1 + i * right.0;
                    *right = (left.0 - x, left.1 - y);
                    *left = (left.0 + x, left.1 + y);
                }
            }
            group *= 2;
        }
        let mut change = 0.0;
        let mut low = 0.0;
        for k in 1..N / 2 {
            let strength = (200.0 * signal[k].0.hypot(signal[k].1)).ln_1p();
            let delta = (strength - prior[k]).max(0.0);
            change += delta;
            if k < low_bins {
                low += delta;
            }
            prior[k] = strength;
        }
        flux.push(change);
        bass.push(low);
    }
    let radius = (fps * 0.25).max(1.0) as usize;
    let flux = detrend(&flux, radius);
    let bass = detrend(&bass, radius);
    let top = |values: &[f64]| values.iter().copied().fold(0.0, f64::max);
    if top(&flux) < 1e-6 {
        return None;
    }
    let mut best = (120.0, f64::NEG_INFINITY);
    for candidate in (700..=1800).step_by(2) {
        let bpm = f64::from(candidate) / 10.0;
        let lag = 60.0 / bpm * fps;
        let whole = lag as usize;
        let part = lag.fract();
        let extent = flux.len().saturating_sub(whole + 1);
        if extent == 0 {
            continue;
        }
        let correlation = (0..extent)
            .map(|i| flux[i] * (flux[i + whole] * (1.0 - part) + flux[i + whole + 1] * part))
            .sum::<f64>()
            / extent as f64;
        let octave = (bpm / 120.0).log2();
        let score = correlation * (-0.5 * octave * octave / 0.81).exp();
        if score > best.1 {
            best = (bpm, score);
        }
    }
    let period = 60.0 / best.0 * fps;
    let strongest = top(&flux).max(1e-6);
    let deepest = top(&bass).max(1e-6);
    let pulse: Vec<_> = flux
        .iter()
        .zip(&bass)
        .map(|(a, b)| a / strongest + b / deepest)
        .collect();
    let mut best_phase = (0.0, f64::NEG_INFINITY);
    for p in 0..(period * 2.0).ceil() as usize {
        let phase = p as f64 / 2.0;
        let mut t = phase;
        let mut score = 0.0;
        while (t as usize) < pulse.len() {
            score += pulse[t as usize];
            t += period;
        }
        if score > best_phase.1 {
            best_phase = (phase, score);
        }
    }
    let mut grid = vec![best_phase.0];
    let mut t = best_phase.0;
    loop {
        let predicted = t + period;
        if predicted as usize >= pulse.len() - 1 {
            break;
        }
        let first = (predicted - period * 0.08) as usize;
        let last = ((predicted + period * 0.08) as usize).min(pulse.len() - 1);
        let mut peak = (predicted, 0.0);
        for (offset, &strength) in pulse[first..=last].iter().enumerate() {
            if strength > peak.1 {
                peak = ((first + offset) as f64, strength);
            }
        }
        t = (predicted + peak.0) / 2.0;
        grid.push(t);
    }
    if grid.len() > 8 {
        let n = grid.len() as f64;
        let mean_i = (n - 1.0) / 2.0;
        let mean_t = grid.iter().sum::<f64>() / n;
        let numerator = grid
            .iter()
            .enumerate()
            .map(|(i, t)| (i as f64 - mean_i) * (t - mean_t))
            .sum::<f64>();
        let denominator = (0..grid.len())
            .map(|i| (i as f64 - mean_i).powi(2))
            .sum::<f64>();
        let slope = numerator / denominator;
        let fitted: Vec<_> = (0..grid.len())
            .map(|i| mean_t + slope * (i as f64 - mean_i))
            .collect();
        let residual = (grid
            .iter()
            .zip(&fitted)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f64>()
            / n)
            .sqrt()
            / fps;
        grid = if residual < 0.03 {
            fitted.into_iter().filter(|&v| v >= 0.0).collect()
        } else {
            (0..grid.len())
                .map(|i| {
                    let neighbors = &grid[i.saturating_sub(1)..(i + 2).min(grid.len())];
                    neighbors.iter().sum::<f64>() / neighbors.len() as f64
                })
                .collect()
        };
    }
    if grid.len() < 2 {
        return None;
    }
    let bpm = 60.0 / ((grid.last()? - grid.first()?) / (grid.len() - 1) as f64 / fps);
    let mut bar = (0, f64::NEG_INFINITY);
    for phase in 0..4 {
        let hits = grid
            .iter()
            .skip(phase)
            .step_by(4)
            .map(|frame| {
                let index = (frame.round() as usize).min(bass.len() - 1);
                bass[index] + 0.25 * flux[index]
            })
            .sum::<f64>();
        if hits > bar.1 {
            bar = (phase, hits);
        }
    }
    let mut energy: Vec<_> = grid
        .iter()
        .enumerate()
        .map(|(i, &frame)| {
            let start = (frame as usize).min(rms.len());
            let end = (grid.get(i + 1).copied().unwrap_or(frame + period) as usize).min(rms.len());
            if end > start {
                rms[start..end].iter().sum::<f64>() / (end - start) as f64
            } else {
                0.0
            }
        })
        .collect();
    let loudest = top(&energy);
    if loudest > 0.0 {
        for value in &mut energy {
            *value /= loudest;
        }
    }
    let beats = grid
        .iter()
        .map(|frame| frame / fps + (N / 2) as f64 / RATE as f64)
        .collect();
    Some(Music {
        duration: count as f64 / RATE as f64,
        bpm,
        beats,
        bar_phase: bar.0,
        energy,
    })
}
