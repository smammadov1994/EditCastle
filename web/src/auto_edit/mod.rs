mod analysis;
mod effects;
mod music;
pub mod planner;
mod recipe;
mod requested;

use crate::{
    clock,
    editor::{Clip, ClipKind, Project, Source, Store},
    timeline,
};
use anyhow::{Context, Result, ensure};
use music::Music;
use planner::{Decision, Footage};
use recipe::{Music as MusicRequest, Pace, Recipe};
pub use requested::from_prompt;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub struct Stage {
    pub project: Project,
    pub action: &'static str,
    pub activity: &'static str,
}

pub struct Plan {
    pub stages: Vec<Stage>,
    pub summary: String,
}

impl Plan {
    fn stage(&mut self, project: &Project, action: &'static str, activity: &'static str) {
        self.stages.push(Stage {
            project: project.clone(),
            action,
            activity,
        });
    }
}

pub async fn plan(store: &Store, project: Project, progress: impl FnMut(String)) -> Result<Plan> {
    build_cut(store, project, &Recipe::default(), progress).await
}

pub async fn analyze_media(
    store: &Store,
    source: &Source,
    length: f64,
) -> Result<serde_json::Value> {
    let rounded = |value: f64| (value * 1000.0).round() / 1000.0;
    if !source.video {
        let track = music::analyze(store, source)
            .await?
            .context("Could not find a steady beat in this audio.")?;
        let bars = track
            .beats
            .iter()
            .enumerate()
            .filter(|(index, _)| index % 4 == track.bar_phase % 4)
            .take(80)
            .map(|(_, &at)| rounded(at))
            .collect::<Vec<_>>();
        return Ok(json!({
            "bpm":rounded(track.bpm), "beat_length":rounded(track.period()),
            "beat_count":track.beats.len(), "first_beat":rounded(track.beats.first().copied().unwrap_or(0.0)),
            "bar_starts":bars,
        }));
    }
    ensure!(length > 0.0, "Use a positive moment length.");
    let decoded = analysis::footage(store, source).await?;
    let duration = decoded.duration.min(clock::seconds(source.duration));
    let moments = planner::best_moments(&decoded, duration, length)
        .into_iter()
        .map(|(start, end, score, faces)| {
            json!({
                "start":rounded(start), "end":rounded(end), "score":rounded(score), "faces":faces,
            })
        })
        .collect::<Vec<_>>();
    let brightness = decoded
        .samples
        .iter()
        .map(|sample| sample.luma)
        .sum::<f64>()
        / decoded.samples.len().max(1) as f64;
    let mut result = json!({
        "duration":rounded(duration), "best_moments":moments,
        "average_brightness":rounded(brightness),
        "has_faces":decoded.samples.iter().any(|sample| sample.faces > 0),
    });
    if let Some(created) = decoded.created {
        result["filmed"] = json!(created);
    }
    if decoded
        .samples
        .iter()
        .any(|sample| sample.utility == Some(true))
    {
        result["looks_like_a_screenshot_or_document"] = json!(true);
    }
    Ok(result)
}

async fn build_cut(
    store: &Store,
    mut project: Project,
    recipe: &Recipe,
    mut progress: impl FnMut(String),
) -> Result<Plan> {
    let used: HashSet<_> = project
        .timeline
        .iter()
        .filter(|clip| is_picture(clip))
        .map(|clip| clip.source)
        .collect();
    let refining = !used.is_empty() && !recipe.whole_library;
    let visuals: Vec<_> = project
        .sources
        .iter()
        .filter(|asset| {
            asset.video
                && (asset.image || asset.duration >= clock::TICKS_PER_SECOND)
                && (!refining || used.contains(&asset.id))
                && !project.library.offline(asset.id)
                && (recipe.keep.is_empty() || recipe.keep.contains(&asset.id))
                && !recipe.drop.contains(&asset.id)
        })
        .collect();
    ensure!(
        !visuals.is_empty(),
        "Add usable video or photos to Media before running Auto Edit."
    );
    let mut footage = Vec::new();
    for (i, asset) in visuals.iter().enumerate() {
        progress(format!(
            "Analyzing {} · {} of {}",
            asset.title,
            i + 1,
            visuals.len()
        ));
        if let Ok(analysis) = analysis::footage(store, asset).await {
            footage.push(Footage {
                asset: (**asset).clone(),
                analysis,
            });
        }
    }
    ensure!(
        !footage.is_empty(),
        "Auto Edit could not decode any of the selected footage."
    );
    let song = if recipe.music == MusicRequest::Off {
        None
    } else if refining {
        song_clip(&project).and_then(|clip| project.source(clip.source))
    } else {
        project
            .sources
            .iter()
            .filter(|asset| song_source(asset) && !project.library.offline(asset.id))
            .max_by_key(|asset| asset.duration)
    };
    let music = if let Some(asset) = song {
        progress(format!("Finding the beat · {}", asset.title));
        music::analyze(store, asset)
            .await
            .unwrap_or(None)
            .map(|music| (asset.id, music))
    } else {
        None
    };
    project.undo_steps.clear();
    project.redo_steps.clear();
    project.messages.clear();
    progress(if refining {
        "Choosing trims within the current cut".into()
    } else {
        "Choosing the strongest moments for the cut".into()
    });
    let plan = if refining {
        improve(
            &project,
            &footage,
            music.as_ref().map(|(_, song)| song),
            recipe,
        )?
    } else {
        let decision = planner::decide(
            &footage,
            music.as_ref().map(|(asset, music)| (*asset, music)),
            recipe,
        )
        .context("Auto Edit could not find a usable shot. Add some longer footage or photos.")?;
        assemble(project, decision)?
    };
    if let Some(last) = plan.stages.last() {
        last.project
            .validate_timeline(&last.project.timeline, last.project.soundtrack)?;
    }
    Ok(plan)
}

fn is_picture(clip: &Clip) -> bool {
    matches!(clip.kind, ClipKind::Video | ClipKind::Image)
}

fn song_source(asset: &Source) -> bool {
    !asset.video && asset.audio && asset.duration >= 8 * clock::TICKS_PER_SECOND
}

fn song_clip(project: &Project) -> Option<&Clip> {
    project
        .timeline
        .iter()
        .filter(|clip| {
            clip.kind == ClipKind::Audio
                && clip.link.is_none()
                && project.source(clip.source).is_some_and(song_source)
                && !project.library.offline(clip.source)
        })
        .max_by_key(|clip| clip.duration())
}

fn add_tracks(project: &mut Project, kind: char, count: usize) -> Vec<String> {
    let mut tracks: Vec<_> = project
        .sequence
        .tracks
        .iter()
        .filter(|track| track.name.starts_with(kind))
        .map(|track| track.name.clone())
        .collect();
    while tracks.len() < count {
        tracks.push(timeline::add_track(project, kind));
    }
    tracks
}

fn snap(time: f64, step: u32) -> u32 {
    clock::snap_seconds(time, step)
}

struct Random(u64);
impl Random {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1_u64 << 53) as f64
    }
}

fn assemble(mut draft: Project, decision: Decision) -> Result<Plan> {
    let step = draft.frame_step();
    let mut rng = Random(
        ((decision.shots.len() * 7919 + draft.sources.len()) as u64)
            .wrapping_add(0x9E3779B97F4A7C15),
    );
    let mut times = Vec::new();
    let mut elapsed = 0.0;
    for shot in &decision.shots {
        let start = snap(elapsed, step);
        elapsed += shot.duration;
        times.push((start, snap(elapsed, step).saturating_sub(start).max(step)));
    }
    let mut result = Plan {
        stages: Vec::new(),
        summary: format!(
            "Cut {} shots in {:.1} seconds{}.",
            decision.shots.len(),
            clock::seconds(snap(elapsed, step)),
            decision
                .bpm
                .map(|bpm| format!(", cut to the beat at {:.0} BPM", bpm))
                .unwrap_or_default()
        ),
    };
    draft.timeline.clear();
    draft.soundtrack = None;
    draft.sequence.in_point = None;
    draft.sequence.out_point = None;
    let picture = add_tracks(&mut draft, 'V', 2);
    let sound = add_tracks(&mut draft, 'A', if decision.song.is_some() { 3 } else { 2 });
    for track in &mut draft.sequence.tracks {
        track.locked = false;
    }
    result.stage(&draft, "edit", "Laying out the rough cut");
    if let Some(music) = decision.song {
        let asset = draft.source(music.asset).context("The song is missing.")?;
        let mut clip = Clip::from_source(asset);
        clip.track = Some(sound[2].clone());
        clip.position = Some(0);
        clip.start = clock::ticks(music.source);
        clip.end = clip
            .start
            .saturating_add(snap(music.duration, step))
            .min(asset.duration);
        clip.effects.fade_in_frames = clock::ticks(0.3);
        clip.effects.fade_out_frames = clock::ticks(music.fade.min(music.duration / 3.0));
        clip.effects.gain_db = -2.0;
        clip.clamp_fades();
        if clip.end > clip.start {
            draft.timeline.push(clip);
            result.stage(&draft, "add_clip", "Placing the song under the cut");
        }
    }
    struct Rough {
        video: Clip,
        audio: Option<Clip>,
        head: u32,
        tail: u32,
        length: u32,
    }
    let mut rough = Vec::new();
    for (shot, &(_, length)) in decision.shots.iter().zip(&times) {
        let asset = draft
            .source(shot.asset)
            .context("The footage is missing.")?;
        let source = if asset.image {
            0
        } else {
            clock::ticks(shot.source).min(asset.duration.saturating_sub(length))
        };
        let length = if asset.image {
            length
        } else {
            length.min(asset.duration - source)
        };
        let floor = |n: f64| (clock::ticks(n) / step) * step;
        let mut head = floor(0.35 + 0.45 * rng.next());
        let mut tail = floor(0.35 + 0.50 * rng.next());
        if !asset.image {
            head = head.min(source / step * step);
            tail = tail.min(asset.duration.saturating_sub(source + length) / step * step);
        }
        let mut video = Clip::from_source(asset);
        video.track = Some(picture[1].clone());
        video.start = source - head.min(source);
        video.end = video.start + head + length + tail;
        let audio = asset.audio.then(|| video.link_audio(sound[1].clone()));
        rough.push(Rough {
            video,
            audio,
            head,
            tail,
            length,
        });
    }
    let mut order: Vec<_> = (0..rough.len()).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, (rng.next() * (i + 1) as f64) as usize);
    }
    let mut head_at = HashMap::new();
    let mut tail_at = HashMap::new();
    let mut travel = HashMap::new();
    let mut at = 0;
    for index in order {
        let shot = &mut rough[index];
        let visible = at + shot.head;
        for clip in std::iter::once(&mut shot.video).chain(shot.audio.iter_mut()) {
            clip.position = Some(at);
            draft.timeline.push(clip.clone());
            head_at.insert(clip.id, visible);
            tail_at.insert(clip.id, visible + shot.length);
            let track = if clip.kind == ClipKind::Audio {
                &sound[0]
            } else {
                &picture[0]
            };
            travel.insert(clip.id, (times[index].0, track.clone()));
        }
        at += shot.head + shot.length + shot.tail;
    }
    result.stage(
        &draft,
        "add_clip",
        "Placing the selected moments on the rough track",
    );
    for clip in &mut draft.timeline {
        if let Some(&at) = head_at.get(&clip.id)
            && clip.position != Some(at)
        {
            clip.trim_head(at);
        }
    }
    result.stage(&draft, "trim_clips", "Trimming the heads of the shots");
    for clip in &mut draft.timeline {
        if let Some(&at) = tail_at.get(&clip.id)
            && clip.position.unwrap_or(0) + clip.duration() != at
        {
            clip.trim_tail(at);
        }
    }
    result.stage(&draft, "trim_clips", "Trimming the ends of the shots");
    for clip in &mut draft.timeline {
        if let Some((at, track)) = travel.get(&clip.id) {
            clip.position = Some(*at);
            clip.track = Some(track.clone());
        }
    }
    result.stage(
        &draft,
        "move_clips",
        "Moving the trimmed shots into sequence",
    );
    let index = rough
        .iter()
        .enumerate()
        .flat_map(|(i, shot)| {
            std::iter::once((shot.video.id, i)).chain(shot.audio.iter().map(move |c| (c.id, i)))
        })
        .collect::<HashMap<_, _>>();
    for clip in &mut draft.timeline {
        let Some(&i) = index.get(&clip.id) else {
            continue;
        };
        let shot = &decision.shots[i];
        if clip.kind == ClipKind::Audio {
            clip.effects.gain_db = shot.gain_db;
            if i % 2 == 0 {
                clip.effects.fade_in_frames = clock::ticks(0.2);
            } else {
                clip.effects.fade_out_frames = clock::ticks(0.25);
            }
        } else {
            if i + 1 == rough.len() {
                clip.effects.fade_out_frames = clock::ticks(0.6);
            }
            if shot.dissolve {
                clip.effects.transition = Some("dissolve".into());
                clip.effects.transition_frames = clock::ticks(0.5);
            }
            if shot.exposure.abs() >= 0.2 {
                clip.effects.grade = json!({"exposure":shot.exposure});
            }
        }
        clip.clamp_fades();
    }
    result.stage(
        &draft,
        "edit",
        "Balancing picture and sound and finishing the cut",
    );
    Ok(result)
}

fn overlap(a: &Clip, b: &Clip) -> bool {
    a.source == b.source
        && a.kind == b.kind
        && (a.kind == ClipKind::Image
            || a.end.min(b.end).saturating_sub(a.start.max(b.start)) as f64
                > 0.5 * f64::from((a.end - a.start).min(b.end - b.start)))
}

fn unlocked(project: &Project, track: &str) -> bool {
    project
        .sequence
        .tracks
        .iter()
        .any(|lane| lane.name == track && !lane.locked)
}

fn improve(
    project: &Project,
    footage: &[Footage],
    music: Option<&Music>,
    recipe: &Recipe,
) -> Result<Plan> {
    let mut draft = project.clone();
    draft.materialize_positions();
    let track = draft
        .sequence
        .tracks
        .iter()
        .filter(|track| track.name.starts_with('V'))
        .find(|track| {
            draft
                .timeline
                .iter()
                .any(|clip| clip.track() == track.name && is_picture(clip))
        })
        .context("There are no picture clips on the timeline.")?
        .name
        .clone();
    ensure!(
        unlocked(&draft, &track),
        "Unlock the main picture track before refining the cut."
    );
    let step = draft.frame_step();
    let mut shots: Vec<_> = draft
        .timeline
        .iter()
        .filter(|clip| clip.track() == track && is_picture(clip))
        .cloned()
        .collect();
    shots.sort_by_key(|clip| clip.position);
    let start = shots[0].position.unwrap_or(0);
    let prior_end = shots
        .iter()
        .map(|s| s.position.unwrap_or(0) + s.duration())
        .max()
        .unwrap_or(start);
    let lookup: HashMap<_, _> = footage.iter().map(|f| (f.asset.id, &f.analysis)).collect();
    let score = planner::scorer(
        shots
            .iter()
            .filter_map(|clip| lookup.get(&clip.source))
            .flat_map(|analysis| analysis.samples.iter().copied())
            .collect(),
    );
    let shot_score = |clip: &Clip| {
        let samples: Vec<_> = lookup
            .get(&clip.source)
            .into_iter()
            .flat_map(|a| a.samples.iter())
            .filter(|s| {
                clip.kind == ClipKind::Image
                    || (clock::seconds(clip.start)..=clock::seconds(clip.end)).contains(&s.time)
            })
            .copied()
            .map(&score)
            .collect();
        if samples.is_empty() {
            0.5
        } else {
            samples.iter().sum::<f64>() / samples.len() as f64
        }
    };
    let mut drops = HashSet::new();
    let mut kept = shots;
    let mut index = if recipe.general || !recipe.has_settings() {
        0
    } else {
        kept.len()
    };
    while index < kept.len() {
        let mut later = index + 1;
        while later < kept.len() {
            if !overlap(&kept[index], &kept[later]) {
                later += 1;
                continue;
            }
            let repeated = kept.remove(later);
            if shot_score(&repeated) > shot_score(&kept[index]) {
                drops.insert(std::mem::replace(&mut kept[index], repeated).id);
            } else {
                drops.insert(repeated.id);
            }
        }
        index += 1;
    }
    let repeats = drops.len();
    if kept.len() > 4 && (recipe.general || !recipe.has_settings()) {
        let min = planner::percentile(kept.iter().map(&shot_score).collect(), 0.15) - 0.05;
        let mut weak: Vec<_> = kept.iter().filter(|s| shot_score(s) < min).collect();
        weak.sort_by(|a, b| shot_score(a).total_cmp(&shot_score(b)));
        drops.extend(
            weak.into_iter()
                .take((kept.len() / 4).min(kept.len() - 4))
                .map(|c| c.id),
        );
    }
    kept.retain(|clip| !drops.contains(&clip.id));
    kept.sort_by_key(|clip| clip.position);
    let song = if recipe.music == MusicRequest::Off {
        None
    } else {
        song_clip(&draft).cloned()
    };
    let beat = music.zip(song.as_ref()).filter(|(m, _)| m.beats.len() > 8);
    let mut result = Plan {
        stages: Vec::new(),
        summary: String::new(),
    };
    if !drops.is_empty() {
        let ids = timeline::linked(&draft, &drops);
        draft.timeline.retain(|clip| !ids.contains(&clip.id));
        result.stage(&draft, "delete_clips", "Removing repeated and weak shots");
    }
    let pace = |clip: &Clip| {
        let duration = clock::seconds(clip.duration());
        match recipe.pace {
            Pace::Fast if duration > 2.0 => (duration * 0.5).clamp(1.2, 2.5),
            Pace::Calm if duration > 6.0 => (duration * 0.8).max(4.0),
            Pace::Unchanged if duration > 3.5 => (duration * 0.6).clamp(2.5, 4.5).min(duration),
            _ => duration,
        }
    };
    let paced = kept.iter().map(&pace).sum::<f64>().max(0.01);
    let existing = kept
        .iter()
        .map(|c| clock::seconds(c.duration()))
        .sum::<f64>()
        .max(0.01);
    let mut positions = HashMap::new();
    let mut cursor = start;
    let mut trimmed = false;
    for (i, shot) in kept.iter().enumerate() {
        let duration = clock::seconds(shot.duration());
        let paced_length = pace(shot);
        let wanted = match recipe.seconds {
            Some(target) if target < paced => {
                (paced_length * target / paced).max(0.2).min(duration)
            }
            Some(target) if target <= existing => {
                paced_length
                    + (duration - paced_length) * (target - paced) / (existing - paced).max(0.01)
            }
            Some(target) => duration * target / existing,
            None => paced_length,
        };
        let last = i + 1 == kept.len();
        let can_grow = recipe.seconds.is_some_and(|target| target > existing);
        let room = if (last || can_grow) && shot.kind == ClipKind::Image {
            3600.0
        } else if (last || can_grow) && !shot.effects.freeze {
            draft.source(shot.source).map_or(duration, |src| {
                let tail = if shot.effects.reverse {
                    0
                } else {
                    src.duration
                };
                shot.elapsed_at_source(tail).max(i64::from(shot.duration())) as f64
                    / f64::from(clock::TICKS_PER_SECOND)
            })
        } else {
            duration
        };
        let from = clock::seconds(cursor);
        let limit = room.min(clock::seconds(clock::MAX_TIME.saturating_sub(cursor)));
        let mut length = wanted.min(limit);
        if let Some((music, song)) = beat {
            let low = from + (2.0 * music.period()).max(0.8) - 1e-6;
            let high = from + limit + 1e-6;
            let mut best = None::<(bool, f64, f64)>;
            for (index, beat) in music.beats.iter().copied().enumerate() {
                let at = clock::seconds(song.position.unwrap_or(0))
                    + (beat - clock::seconds(song.start)) / song.effects.speed;
                if at < low || at > high {
                    continue;
                }
                let bar = last && index % 4 == music.bar_phase;
                let distance = (at - from - wanted).abs();
                if best.is_none_or(|(is_bar, error, _)| {
                    (bar && !is_bar) || (bar == is_bar && distance < error)
                }) {
                    best = Some((bar, distance, at));
                }
            }
            if let Some((_, _, end)) = best {
                length = end - from;
            }
        }
        let old_at = shot.position.unwrap_or(0);
        let length = snap(length, step).max(step.min(shot.duration()));
        let mut head = old_at;
        if shot.kind == ClipKind::Video
            && !shot.effects.freeze
            && length < shot.duration()
            && let Some(analysis) = lookup.get(&shot.source)
        {
            let span = clock::seconds(
                shot.source_at(i64::from(length))
                    .abs_diff(shot.source_at(0)),
            );
            let mut candidate = clock::seconds(shot.start);
            let limit = clock::seconds(shot.end) - span;
            let mut best = None;
            while candidate <= limit + 1e-6 {
                let values = analysis
                    .samples
                    .iter()
                    .filter(|s| s.time >= candidate && s.time <= candidate + span)
                    .copied()
                    .map(&score);
                if let Some(q) = planner::window_score(values)
                    && best.is_none_or(|(_, old)| q > old)
                {
                    best = Some((candidate, q));
                }
                candidate += 0.1;
            }
            if let Some((start, _)) = best {
                let source = clock::ticks(if shot.effects.reverse {
                    start + span
                } else {
                    start
                });
                let offset = shot.elapsed_at_source(source).max(0) as u32;
                head = snap(
                    clock::seconds(old_at + offset.min(shot.duration() - length)),
                    step,
                )
                .clamp(old_at, old_at + shot.duration() - length);
            }
        }
        let targets = timeline::linked(&draft, &HashSet::from([shot.id]));
        for clip in draft
            .timeline
            .iter_mut()
            .filter(|c| targets.contains(&c.id))
        {
            let origin = clip.position.unwrap_or(0);
            if origin == old_at {
                if head > old_at {
                    clip.trim_head(head);
                    trimmed = true;
                }
                if clip.duration() != length {
                    clip.trim_tail(head + length);
                    if let Some(asset) =
                        project.source(clip.source).filter(|_| !clip.effects.freeze)
                    {
                        clip.end = clip.end.min(asset.duration).max(clip.start + 1);
                        clip.clamp_fades();
                    }
                    trimmed = true;
                }
            }
            let position = clip.position.unwrap_or(0);
            positions.insert(
                clip.id,
                (i64::from(position) + i64::from(cursor) - i64::from(head)).max(0) as u32,
            );
        }
        cursor += draft
            .timeline
            .iter()
            .find(|clip| clip.id == shot.id)
            .map_or(length, Clip::duration);
    }
    if trimmed {
        result.stage(
            &draft,
            "trim_clips",
            "Trimming shots to their strongest moments",
        );
    }
    let mut moved = false;
    for clip in &mut draft.timeline {
        if let Some(&at) = positions.get(&clip.id)
            && clip.position != Some(at)
        {
            clip.position = Some(at);
            moved = true;
        }
    }
    if moved {
        result.stage(
            &draft,
            "move_clips",
            "Closing the gaps while preserving shot order",
        );
    }
    let mut titles: Vec<_> = draft
        .timeline
        .iter()
        .filter(|clip| clip.kind == ClipKind::Text)
        .cloned()
        .collect();
    titles.sort_by_key(|clip| {
        draft
            .sequence
            .tracks
            .iter()
            .position(|t| t.name == clip.track())
    });
    let mut duplicate = HashSet::new();
    for (i, clip) in titles
        .iter()
        .enumerate()
        .filter(|(_, clip)| !clip.effects.title.is_empty())
    {
        if duplicate.contains(&clip.id) {
            continue;
        }
        for later in &titles[i + 1..] {
            let a = clip.position.unwrap_or(0);
            let b = later.position.unwrap_or(0);
            if unlocked(project, later.track())
                && clip.effects.title == later.effects.title
                && a + clip.duration() > b
                && b + later.duration() > a
            {
                duplicate.insert(later.id);
            }
        }
    }
    let mut polished = false;
    draft.timeline.retain(|clip| {
        let remove = duplicate.contains(&clip.id)
            || (clip.track() != track
                && clip.track().starts_with('V')
                && unlocked(project, clip.track())
                && clip.position.unwrap_or(0) >= cursor.saturating_sub(clock::ticks(0.2)));
        polished |= remove;
        !remove
    });
    let mut looks = HashMap::new();
    for clip in &kept {
        if let Some(look) = clip.effects.grade["look"].as_str() {
            *looks.entry(look.to_owned()).or_insert(0) += 1;
        }
    }
    let look = looks
        .into_iter()
        .max_by(|(a, ac), (b, bc)| ac.cmp(bc).then_with(|| b.cmp(a)))
        .map(|(look, _)| look);
    let kept_ids: HashSet<_> = kept.iter().map(|clip| clip.id).collect();
    let links: HashSet<_> = kept.iter().filter_map(|clip| clip.link).collect();
    let audio_extremes = draft
        .timeline
        .iter()
        .filter(|c| {
            c.kind == ClipKind::Audio
                && unlocked(project, c.track())
                && c.link.is_some_and(|link| links.contains(&link))
        })
        .fold(
            (None::<(u32, Uuid)>, None::<(u32, Uuid)>),
            |(first, last), clip| {
                let entry = (clip.position.unwrap_or(0), clip.id);
                (
                    Some(first.map_or(entry, |x| x.min(entry))),
                    Some(last.map_or(entry, |x| x.max(entry))),
                )
            },
        );
    for clip in draft
        .timeline
        .iter_mut()
        .filter(|clip| unlocked(project, clip.track()))
    {
        let from = clip.position.unwrap_or(0);
        if (clip.track() != track && clip.track().starts_with('V')
            || song.as_ref().is_some_and(|c| c.id == clip.id))
            && from < cursor
            && from + clip.duration() > cursor
        {
            clip.trim_tail(cursor);
            polished = true;
        }
        if song.as_ref().is_some_and(|c| c.id == clip.id)
            && clip.effects.fade_out_frames < clock::ticks(1.5)
        {
            let fade = clock::ticks((clock::seconds(clip.duration()) * 0.15).clamp(0.5, 2.0))
                .min(clip.duration());
            polished |= clip.effects.fade_out_frames != fade;
            clip.effects.fade_out_frames = fade;
        }
        if clip.kind == ClipKind::Audio && clip.link.is_some_and(|link| links.contains(&link)) {
            if song.is_some() && clip.effects.gain_db > -10.0 {
                clip.effects.gain_db = -16.0;
                polished = true;
            } else if song.is_none() {
                if audio_extremes.0.is_some_and(|(_, id)| id == clip.id)
                    && clip.effects.fade_in_frames < clock::ticks(0.2)
                {
                    clip.effects.fade_in_frames = clock::ticks(0.3).min(clip.duration());
                    polished = true;
                }
                if audio_extremes.1.is_some_and(|(_, id)| id == clip.id)
                    && clip.effects.fade_out_frames < clock::ticks(0.3)
                {
                    clip.effects.fade_out_frames = clock::ticks(0.6).min(clip.duration());
                    polished = true;
                }
            }
        }
        if kept_ids.contains(&clip.id) {
            let mut grade = clip.effects.grade.clone();
            if let Some(name) = &look {
                grade["look"] = json!(name);
            }
            if grade["exposure"].as_f64().unwrap_or(0.0) == 0.0
                && let Some(analysis) = lookup.get(&clip.source)
            {
                let original = kept.iter().find(|c| c.id == clip.id).unwrap();
                let exposure = planner::exposure(
                    analysis
                        .samples
                        .iter()
                        .filter(|s| {
                            clip.kind == ClipKind::Image
                                || (clock::seconds(original.start)..=clock::seconds(original.end))
                                    .contains(&s.time)
                        })
                        .copied(),
                );
                if exposure.abs() >= 0.2 {
                    grade["exposure"] = json!(exposure);
                }
            }
            if grade != clip.effects.grade {
                clip.effects.grade = grade;
                polished = true;
            }
        }
        clip.clamp_fades();
    }
    if polished {
        result.stage(
            &draft,
            "edit",
            "Balancing the edit and finishing titles and sound",
        );
    }
    result.summary = if result.stages.is_empty() {
        "The edit already has a continuous cut.".into()
    } else {
        format!(
            "Refined the existing cut from {:.1} to {:.1} seconds{}.",
            clock::seconds(prior_end - start),
            clock::seconds(cursor - start),
            if repeats > 0 {
                format!(
                    " and removed {repeats} repeated {}",
                    if repeats == 1 { "shot" } else { "shots" }
                )
            } else {
                String::new()
            }
        )
    };
    Ok(result)
}
