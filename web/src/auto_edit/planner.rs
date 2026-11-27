use super::{
    analysis::{Analysis, Sample},
    music::Music,
    recipe::{Pace, Recipe},
};
use crate::{clock, editor::Source};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
pub(super) struct Footage {
    pub asset: Source,
    pub analysis: Analysis,
}

#[derive(Clone, Serialize)]
pub(super) struct Shot {
    pub asset: Uuid,
    pub source: f64,
    pub duration: f64,
    pub exposure: f64,
    pub gain_db: f64,
    pub dissolve: bool,
}

#[derive(Serialize)]
pub(super) struct Song {
    pub asset: Uuid,
    pub source: f64,
    pub duration: f64,
    pub fade: f64,
}

#[derive(Serialize)]
pub(super) struct Decision {
    pub shots: Vec<Shot>,
    pub song: Option<Song>,
    pub bpm: Option<f64>,
}

pub(super) fn percentile(mut values: Vec<f64>, p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        0.0
    } else {
        values[((values.len() - 1) as f64 * p) as usize]
    }
}

pub(super) fn scorer(samples: Vec<Sample>) -> impl Fn(Sample) -> f64 {
    let sharpness: Vec<_> = samples.iter().map(|s| s.sharpness.ln_1p()).collect();
    let low = percentile(sharpness.clone(), 0.1);
    let high = percentile(sharpness, 0.9).max(low + 1e-6);
    let motion = percentile(samples.iter().map(|s| s.motion).collect(), 0.9).max(0.02);
    let color = percentile(samples.iter().map(|s| s.color).collect(), 0.9).max(0.05);
    move |s| {
        let sharp = ((s.sharpness.ln_1p() - low) / (high - low)).clamp(0.0, 1.0);
        let exposure = 1.0 - ((s.luma - 0.45).abs() / 0.38).min(1.0);
        let mut score = 0.36 * sharp + 0.2 * exposure + 0.14 * (s.color / color).min(1.0);
        if s.faces > 0 {
            score += 0.16;
        }
        score += s.aesthetic.map_or(0.12, |v| 0.13 * (v + 1.0));
        if s.utility == Some(true) {
            score -= 0.35;
        }
        score - ((s.motion / motion - 1.1) * 0.3).clamp(0.0, 0.3)
    }
}

pub(super) fn exposure(samples: impl Iterator<Item = Sample>) -> f64 {
    let (sum, count) = samples.fold((0.0, 0usize), |(sum, n), s| (sum + s.luma, n + 1));
    let mean = if count == 0 { 0.45 } else { sum / count as f64 };
    (((0.45 / mean.max(0.05)).log2() * 0.5).clamp(-0.5, 0.5) * 10.0).round() / 10.0
}

pub(super) fn window_score(scores: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count, min) = scores.fold((0.0, 0, f64::INFINITY), |(sum, n, min), score| {
        (sum + score, n + 1, min.min(score))
    });
    (count > 0).then(|| 0.75 * sum / count as f64 + 0.25 * min)
}

pub(super) fn best_moments(
    analysis: &Analysis,
    limit: f64,
    length: f64,
) -> Vec<(f64, f64, f64, usize)> {
    let end = analysis.duration.min(limit);
    let length = length.min(end);
    let samples = &analysis.samples;
    let score = scorer(samples.clone());
    let scores = samples.iter().copied().map(score).collect::<Vec<_>>();
    let mut windows = Vec::new();
    let mut start = 0.0;
    while start + length <= end + 1e-6 {
        let first = samples.partition_point(|sample| sample.time < start);
        let last = samples.partition_point(|sample| sample.time <= start + length);
        if let Some(score) = window_score(scores[first..last].iter().copied()) {
            let faces = samples[first..last]
                .iter()
                .map(|sample| sample.faces)
                .max()
                .unwrap_or(0);
            windows.push((start, start + length, score, faces));
        }
        start += 0.25;
    }
    windows.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut chosen = Vec::<(f64, f64, f64, usize)>::new();
    for window in windows {
        if chosen
            .iter()
            .all(|other| window.1 <= other.0 || window.0 >= other.1)
        {
            chosen.push(window);
            if chosen.len() == 5 {
                break;
            }
        }
    }
    chosen
}

struct Rated<'a> {
    footage: &'a Footage,
    scores: Vec<f64>,
    picked: Vec<(f64, f64)>,
}

impl Rated<'_> {
    fn limit(&self) -> f64 {
        self.footage
            .analysis
            .duration
            .min(clock::seconds(self.footage.asset.duration))
    }

    fn window(&self, length: f64, gap: f64, moments: &[f64]) -> Option<(f64, f64)> {
        if self.footage.asset.image {
            return self.picked.is_empty().then(|| (0.0, self.scores[0]));
        }
        let limit = self.limit();
        if limit < length + 0.1 {
            return None;
        }
        let mut start = 0.05;
        let mut best = None;
        let mut pointed = None;
        let samples = &self.footage.analysis.samples;
        while start + length <= limit - 0.05 {
            let end = start + length;
            if self
                .picked
                .iter()
                .all(|&(a, b)| end + gap <= a || start >= b + gap)
            {
                let first = samples.partition_point(|s| s.time < start);
                let last = samples.partition_point(|s| s.time <= end);
                if let Some(score) = window_score(self.scores[first..last].iter().copied()) {
                    if best.is_none_or(|(_, old)| score > old) {
                        best = Some((start, score));
                    }
                    let margin = (length / 4.0).min(0.3);
                    if moments
                        .iter()
                        .any(|time| *time >= start + margin && *time <= end - margin)
                        && pointed.is_none_or(|(_, old)| score > old)
                    {
                        pointed = Some((start, score));
                    }
                }
            }
            start += 0.25;
        }
        pointed.or(best)
    }
}

pub(super) fn decide(
    footage: &[Footage],
    music: Option<(Uuid, &Music)>,
    recipe: &Recipe,
) -> Option<Decision> {
    let score = scorer(
        footage
            .iter()
            .flat_map(|v| v.analysis.samples.iter().copied())
            .collect(),
    );
    let mut sources: Vec<_> = footage
        .iter()
        .filter(|v| !v.analysis.samples.is_empty())
        .map(|v| Rated {
            footage: v,
            scores: v.analysis.samples.iter().copied().map(&score).collect(),
            picked: Vec::new(),
        })
        .collect();
    sources.sort_by(
        |a, b| match (&a.footage.analysis.created, &b.footage.analysis.created) {
            (Some(a), Some(b)) => a.cmp(b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        },
    );
    let scores: Vec<_> = sources
        .iter()
        .flat_map(|s| s.scores.iter().copied())
        .collect();
    let floor = percentile(scores.clone(), 0.30);
    let lower = percentile(scores, 0.12);
    let mut period = music.map(|(_, m)| m.period());
    let scale = match recipe.pace {
        Pace::Fast => 0.55,
        Pace::Calm => 1.7,
        Pace::Unchanged => 1.0,
    };
    let mut long = period.map_or(4, |p| {
        [2, 4, 8, 16]
            .into_iter()
            .find(|&n| f64::from(n) * p >= 1.5 * scale)
            .unwrap_or(16)
    });
    let mut short = if period.is_some_and(|p| f64::from(long / 2) * p >= 0.8) {
        long / 2
    } else {
        long
    };
    let wanted = if recipe.every {
        Some(recipe.shots.unwrap_or(0).max(sources.len()).max(1))
    } else {
        recipe.shots
    };
    let per_shot = wanted.map(|count| {
        let normal = match recipe.pace {
            Pace::Fast => 0.9,
            Pace::Calm => 2.5,
            Pace::Unchanged => 1.5,
        };
        (recipe.seconds.unwrap_or(count as f64 * normal) / count as f64).max(if recipe.every {
            0.2
        } else {
            0.35
        })
    });
    let sub = usize::from(
        recipe.every
            && period
                .zip(per_shot)
                .is_some_and(|(p, length)| length < p * 0.9),
    ) + 1;
    period = period.map(|p| p / sub as f64);
    if let Some((p, length)) = period.zip(per_shot) {
        long = ((length / p).floor() as i32).max(1);
        short = long;
    }
    let good = sources
        .iter()
        .map(|s| {
            let step = if s.scores.len() > 1 {
                s.footage.analysis.duration / s.scores.len() as f64
            } else {
                2.5
            };
            s.scores.iter().filter(|&&q| q >= floor).count() as f64 * step
        })
        .sum::<f64>();
    let target = recipe
        .seconds
        .or(per_shot
            .zip(wanted)
            .map(|(length, count)| length * count as f64))
        .unwrap_or((good * 0.5).clamp(12.0, 40.0));
    let target = music.map_or(target, |(_, m)| target.min(m.duration - 2.0));
    let mut shots: Vec<Shot> = Vec::new();
    let mut shot_beats = Vec::<usize>::new();
    let mut total = 0.0;
    let mut round = 0;
    while total < target {
        let before = shots.len();
        let alternatives = sources.len() > 1;
        for s in &mut sources {
            if total >= target && !(recipe.every && round == 0) {
                break;
            }
            if round > 0 && s.limit() < 12.0 {
                continue;
            }
            let id = s.footage.asset.id;
            if alternatives && shots.last().is_some_and(|shot| shot.asset == id) {
                continue;
            }
            let i = shots.len() % 6;
            let mut beats = if [true, false, false, true, true, false][i] {
                long
            } else {
                short
            };
            let rhythm = per_shot.map_or([2.4, 1.8, 2.1, 2.8, 1.7, 2.2][i] * scale, |length| {
                length * [1.08, 0.94, 1.0, 1.06, 0.92, 1.0][i]
            });
            let mut length = period.map_or(rhythm, |p| f64::from(beats) * p);
            let moments = recipe.moments.get(&id).map_or(&[][..], Vec::as_slice);
            let gap = if round > 0 { 4.0 } else { 0.3 };
            let mut found = s.window(length + 0.08, gap, moments);
            if found.is_none() && (beats != short || period.is_none()) {
                beats = short;
                length = period.map_or(per_shot.unwrap_or(1.6).min(1.6), |p| f64::from(short) * p);
                found = s.window(length + 0.08, gap, moments);
            }
            let Some((source, quality)) = found else {
                continue;
            };
            if !(recipe.every && round == 0)
                && quality
                    < if round == 0 || total < 18.0 {
                        lower
                    } else {
                        floor
                    }
            {
                continue;
            }
            s.picked.push((source, source + length));
            let luma = exposure(
                s.footage
                    .analysis
                    .samples
                    .iter()
                    .filter(|v| v.time >= source && v.time <= source + length)
                    .copied(),
            );
            shots.push(Shot {
                asset: id,
                source,
                duration: length,
                exposure: luma,
                gain_db: 0.0,
                dissolve: false,
            });
            shot_beats.push(beats as usize);
            total += length;
        }
        if shots.len() == before || recipe.every {
            break;
        }
        round += 1;
    }
    if shots.is_empty() {
        return None;
    }
    let lookup: std::collections::HashMap<_, _> = sources
        .into_iter()
        .map(|s| (s.footage.asset.id, s))
        .collect();
    let unit = period.unwrap_or(0.25);
    loop {
        let before = total;
        for i in 0..shots.len() {
            if total + unit > target + 0.05 {
                break;
            }
            let s = &shots[i];
            let file = &lookup[&s.asset];
            let end = s.source + s.duration + unit;
            if file.footage.asset.image || end > file.limit() - 0.05 {
                continue;
            }
            if shots.iter().enumerate().any(|(j, other)| {
                i != j
                    && s.asset == other.asset
                    && end + 0.1 > other.source
                    && other.source + other.duration > s.source
            }) {
                continue;
            }
            shots[i].duration += unit;
            shot_beats[i] += 1;
            total += unit;
        }
        if total == before {
            break;
        }
    }
    if recipe.every
        && let Some(target) = recipe.seconds
        && total > target + 0.15
    {
        let last = shots.last_mut()?;
        if let Some(period) = period {
            let beats = shot_beats.last_mut()?;
            while total > target + 0.15 && *beats > 1 {
                *beats -= 1;
                last.duration -= period;
                total -= period;
            }
        } else {
            last.duration -= (total - target).min(last.duration - 0.3).max(0.0);
        }
    }
    let mut decision = Decision {
        shots,
        song: None,
        bpm: None,
    };
    if let Some((song, m)) = music {
        let bar = 4 * sub;
        let phase = m.bar_phase * sub;
        let grid = if sub == 2 {
            let mut expanded = Vec::with_capacity(m.beats.len() * 2);
            for i in 0..m.beats.len() {
                expanded.push(m.beats[i]);
                if let Some(next) = m.beats.get(i + 1) {
                    expanded.push((m.beats[i] + next) / 2.0);
                }
            }
            std::borrow::Cow::Owned(expanded)
        } else {
            std::borrow::Cow::Borrowed(m.beats.as_slice())
        };
        let energy = if sub == 2 {
            std::borrow::Cow::Owned(m.energy.iter().flat_map(|e| [*e, *e]).collect::<Vec<_>>())
        } else {
            std::borrow::Cow::Borrowed(m.energy.as_slice())
        };
        let last = decision.shots.len() - 1;
        let extra = if recipe.every && recipe.seconds.is_some() {
            0
        } else {
            (bar - shot_beats.iter().sum::<usize>() % bar) % bar
        };
        if extra > 0 {
            let s = &decision.shots[last];
            let old_span = s.duration;
            let span = old_span + extra as f64 * m.period() / sub as f64;
            let file = &lookup[&s.asset];
            let fits = |start: f64| {
                file.footage.asset.image
                    || (start >= 0.0
                        && start + span <= file.limit() - 0.05
                        && decision.shots[..last].iter().all(|o| {
                            o.asset != s.asset
                                || start + span + 0.1 <= o.source
                                || start >= o.source + o.duration + 0.1
                        }))
            };
            if let Some(at) = [s.source, s.source - (span - old_span)]
                .into_iter()
                .find(|&start| fits(start))
            {
                shot_beats[last] += extra;
                decision.shots[last].source = at;
            } else if shot_beats[last] > bar - extra {
                shot_beats[last] -= bar - extra;
            } else if shot_beats[last] == bar - extra && last > 0 && !recipe.every {
                shot_beats.pop();
                decision.shots.pop();
            }
        }
        let mut count = shot_beats.iter().sum::<usize>();
        while count + 2 >= grid.len() && shot_beats.len() > 1 {
            count -= shot_beats.pop().unwrap();
            decision.shots.pop();
        }
        let mut best = (phase, f64::NEG_INFINITY);
        for b in (phase..grid.len()).step_by(bar) {
            if b + count + 1 >= grid.len() || b + count > energy.len() {
                break;
            }
            let avg = |values: &[f64]| {
                f64::from(values.iter().map(|&value| value as f32).sum::<f32>())
                    / values.len().max(1) as f64
            };
            let mean = avg(&energy[b..b + count]);
            let before = if b >= 8 * sub {
                avg(&energy[b - 8 * sub..b])
            } else {
                mean
            };
            let after = avg(&energy[b..(b + 8 * sub).min(energy.len())]);
            let quality = mean + 0.5 * (after - before).max(0.0)
                - if b == phase && b < bar { 0.05 } else { 0.0 };
            if quality > best.1 {
                best = (b, quality);
            }
        }
        if best.0 + count < grid.len() {
            let begin = best.0;
            let mut at = begin;
            for (i, (shot, beats)) in decision.shots.iter_mut().zip(shot_beats).enumerate() {
                let next = at + beats;
                shot.duration = grid[next] - grid[at];
                shot.dissolve = sub == 1 && i > 0 && (at - begin) % 16 == 0;
                if lookup[&shot.asset].footage.asset.audio {
                    shot.gain_db = -16.0;
                }
                at = next;
            }
            let tail = (m.duration - grid[at]).clamp(0.0, 1.2);
            decision.song = Some(Song {
                asset: song,
                source: grid[begin],
                duration: grid[at] - grid[begin] + tail,
                fade: tail + 0.6,
            });
            decision.bpm = Some(m.bpm);
        }
    }
    for (i, s) in decision.shots.iter_mut().enumerate() {
        let file = &lookup[&s.asset];
        if !file.footage.asset.image {
            s.source = s.source.min(file.limit() - s.duration - 0.02).max(0.0);
        }
        if decision.song.is_none() {
            s.dissolve = i > 0 && i % 4 == 0;
        }
    }
    Some(decision)
}

#[derive(Deserialize)]
struct Fixture {
    footage: Vec<Footage>,
    song: Option<Uuid>,
    music: Option<Music>,
    recipe: Option<FixtureRecipe>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct FixtureRecipe {
    seconds: Option<f64>,
    shots: Option<usize>,
    every: bool,
    pace: Pace,
    moments: std::collections::HashMap<Uuid, Vec<f64>>,
}

pub(crate) fn replay(case: &str) -> anyhow::Result<String> {
    let Ok(case) = serde_json::from_str::<Fixture>(case) else {
        return Ok("invalid Auto Edit fixture".into());
    };
    let fixture = case.recipe.unwrap_or_default();
    let recipe = Recipe {
        seconds: fixture
            .seconds
            .filter(|&value| (0.2..=600.0).contains(&value)),
        shots: fixture.shots.filter(|value| (2..=200).contains(value)),
        every: fixture.every,
        pace: fixture.pace,
        moments: fixture.moments,
        ..Recipe::default()
    };
    let decision = decide(&case.footage, case.song.zip(case.music.as_ref()), &recipe);
    // Display rounded decisions like the native diagnostic, independent of JSON key order.
    let Some(decision) = decision else {
        return Ok("no cut".into());
    };
    let mut text = format!("{} shots", decision.shots.len());
    for s in decision.shots {
        use std::fmt::Write;
        write!(
            text,
            "\n{} {:.3} {:.3} {:.1} {:.0} {}",
            s.asset, s.source, s.duration, s.exposure, s.gain_db, s.dissolve
        )?;
    }
    if let Some(s) = decision.song {
        use std::fmt::Write;
        write!(
            text,
            "\n{} {:.3} {:.3} {:.3}",
            s.asset, s.source, s.duration, s.fade
        )?;
    }
    Ok(text)
}
