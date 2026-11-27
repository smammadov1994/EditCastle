use super::{analysis, is_picture, planner, recipe::Recipe, unlocked};
use crate::{
    clock,
    editor::{Clip, ClipKind, Project, Source, Store},
    timeline,
};
use std::collections::HashMap;
use uuid::Uuid;

fn named(source: &Source, keywords: &[&str]) -> bool {
    let name = source.title.to_lowercase();
    keywords.iter().any(|word| name.contains(word))
}

pub(super) async fn place(
    store: &Store,
    project: &mut Project,
    recipe: &Recipe,
) -> (usize, String) {
    let effects = project
        .sources
        .iter()
        .filter(|source| {
            source.audio
                && !source.video
                && (clock::ticks(0.05)..clock::ticks(10.0)).contains(&source.duration)
                && !project.library.offline(source.id)
        })
        .cloned()
        .collect::<Vec<_>>();
    let leads = effects
        .iter()
        .filter(|source| {
            named(
                source,
                &[
                    "whoosh",
                    "swoosh",
                    "swish",
                    "woosh",
                    "transition",
                    "riser",
                    "sweep",
                ],
            )
        })
        .collect::<Vec<_>>();
    let hits = effects
        .iter()
        .filter(|source| {
            named(
                source,
                &["hit", "impact", "boom", "thud", "punch", "slam", "drop"],
            )
        })
        .collect::<Vec<_>>();
    let pool = if !leads.is_empty() {
        leads.clone()
    } else if recipe.whoosh {
        vec![]
    } else {
        effects.iter().collect::<Vec<_>>()
    };
    if pool.is_empty() {
        return (
            0,
            if recipe.whoosh {
                "Add a whoosh sound file to Media to place it on the cuts"
            } else {
                "Add short sound effects to Media first"
            }
            .into(),
        );
    }
    let track = project.sequence.tracks.iter().find(|track| {
        track.name.starts_with('V')
            && project
                .timeline
                .iter()
                .any(|clip| clip.track() == track.name && is_picture(clip))
    });
    let Some(track) = track else {
        return (
            0,
            "Add picture clips to the cut before placing sound effects".into(),
        );
    };
    let mut pictures = project
        .timeline
        .iter()
        .filter(|clip| clip.track() == track.name && is_picture(clip))
        .map(|clip| (project.placement(clip.id).unwrap_or(0), clip))
        .collect::<Vec<_>>();
    pictures.sort_by_key(|(at, _)| *at);
    let first = pictures[0].0;
    let end = pictures
        .iter()
        .map(|(at, clip)| at + clip.duration())
        .max()
        .unwrap_or(first);
    let moments = if recipe.judge_effects {
        strong_moments(store, project, &pictures).await
    } else {
        let mut cuts = pictures
            .iter()
            .skip(1)
            .map(|(at, _)| (*at, false))
            .collect::<Vec<_>>();
        if !recipe.whoosh && !hits.is_empty() {
            cuts.insert(0, (first, true));
        }
        if cuts.is_empty() {
            cuts.push((first, false));
        }
        cuts
    };
    if moments.is_empty() {
        return (
            0,
            "The picture and motion analysis found no strong moment for a sound effect".into(),
        );
    }
    let mut spots = moments
        .into_iter()
        .take(8)
        .enumerate()
        .map(|(i, (at, impact))| {
            let source = if impact && !recipe.whoosh {
                hits.first().copied().unwrap_or(pool[i % pool.len()])
            } else {
                pool[i % pool.len()]
            };
            let lead = leads.iter().any(|candidate| candidate.id == source.id);
            let start = at
                .saturating_sub(if lead {
                    (source.duration as f64 * 0.6)
                        .round()
                        .min(f64::from(clock::ticks(0.5))) as u32
                } else {
                    clock::ticks(0.03)
                })
                .max(first);
            let stop = end.min(start + source.duration);
            (start, stop, source)
        })
        .collect::<Vec<_>>();
    spots.retain(|(a, b, _)| b > a);
    spots.sort_by_key(|(a, _, _)| *a);
    if spots.is_empty() {
        return (
            0,
            "The current cut has no room for the sound effects".into(),
        );
    }
    project.materialize_positions();
    let first = spots[0].0;
    let end = spots
        .iter()
        .map(|(_, stop, _)| *stop)
        .max()
        .unwrap_or(first);
    let available = project
        .sequence
        .tracks
        .iter()
        .filter(|track| track.name.starts_with('A') && unlocked(project, &track.name))
        .find(|track| {
            !project.timeline.iter().any(|clip| {
                let at = clip.position.unwrap_or(0);
                clip.track() == track.name && at < end && at + clip.duration() > first
            })
        })
        .map(|track| track.name.clone());
    let effect_ids = effects
        .iter()
        .map(|source| source.id)
        .collect::<std::collections::HashSet<Uuid>>();
    let mut live = Vec::new();
    let mut last = first;
    let step = project.frame_step();
    for (start, stop, source) in spots {
        if project.timeline.iter().any(|clip| {
            let at = clip.position.unwrap_or(0);
            effect_ids.contains(&clip.source) && at < stop && at + clip.duration() > start
        }) {
            continue;
        }
        let start = clock::snap_seconds(clock::seconds(start.max(last)), step);
        let duration = clock::snap_seconds(clock::seconds(stop.saturating_sub(start)), step)
            .min(source.duration);
        if duration <= clock::ticks(0.1) {
            continue;
        }
        let mut clip = Clip::from_source(source);
        clip.position = Some(start);
        clip.end = duration;
        clip.effects.gain_db = -6.0;
        clip.effects.fade_out_frames = clock::ticks(0.15).min(duration / 3);
        live.push(clip);
        last = start + duration;
    }
    let count = live.len();
    if count == 0 {
        return (
            0,
            "The cuts already have effects or leave too little room for another one".into(),
        );
    }
    let track = available.unwrap_or_else(|| timeline::add_track(project, 'A'));
    for clip in &mut live {
        clip.track = Some(track.clone());
    }
    project.timeline.extend(live);
    (
        count,
        format!(
            "Placed {count} sound {} around the edit points",
            if count == 1 { "effect" } else { "effects" }
        ),
    )
}

async fn strong_moments(
    store: &Store,
    project: &Project,
    pictures: &[(u32, &Clip)],
) -> Vec<(u32, bool)> {
    let mut cache = HashMap::new();
    for (_, clip) in pictures {
        if cache.contains_key(&clip.source) {
            continue;
        }
        if let Some(asset) = project.source(clip.source)
            && let Ok(reading) = analysis::footage(store, asset).await
        {
            cache.insert(clip.source, reading);
        }
    }
    let samples = |clip: &Clip| {
        cache
            .get(&clip.source)
            .into_iter()
            .flat_map(|reading| reading.samples.iter())
            .filter(|s| {
                clip.kind == ClipKind::Image
                    || (clock::seconds(clip.start)..=clock::seconds(clip.end)).contains(&s.time)
            })
            .copied()
            .collect::<Vec<_>>()
    };
    let samples_at_edges = pictures
        .iter()
        .map(|(_, clip)| samples(clip))
        .collect::<Vec<_>>();
    let mut judged = Vec::new();
    let avg = |samples: &[analysis::Sample]| {
        (!samples.is_empty()).then(|| {
            let n = samples.len() as f64;
            (
                samples.iter().map(|s| s.luma).sum::<f64>() / n,
                samples.iter().map(|s| s.color).sum::<f64>() / n,
            )
        })
    };
    for (i, pair) in samples_at_edges.windows(2).enumerate() {
        let (last, next) = (&pair[0], &pair[1]);
        if let (Some((l0, c0)), Some((l1, c1))) = (
            avg(&last[last.len().saturating_sub(2)..]),
            avg(&next[..next.len().min(2)]),
        ) {
            let score = 3.0 * (l0 - l1).abs()
                + 1.5 * (c0 - c1).abs()
                + if pictures[i].1.source != pictures[i + 1].1.source {
                    0.1
                } else {
                    0.0
                };
            if score >= 0.5 {
                judged.push((pictures[i + 1].0, false, score));
            }
        }
    }
    let motion = samples_at_edges
        .iter()
        .flatten()
        .map(|s| s.motion)
        .collect::<Vec<_>>();
    if motion.len() >= 8 {
        let bar = planner::percentile(motion.clone(), 0.95)
            .max(planner::percentile(motion, 0.5) * 3.0)
            .max(0.02);
        for ((start, clip), samples) in pictures.iter().zip(samples_at_edges) {
            for sample in samples.iter().filter(|s| s.motion >= bar) {
                let at =
                    start.saturating_add(
                        clip.elapsed_at_source(clock::ticks(sample.time)).max(0) as u32
                    );
                if judged
                    .iter()
                    .all(|(other, _, _)| at.abs_diff(*other) >= clock::ticks(1.5))
                {
                    judged.push((at, true, 0.5 + 0.1 * sample.motion / bar));
                }
            }
        }
    }
    judged.sort_by(|a, b| b.2.total_cmp(&a.2));
    judged.into_iter().map(|(at, hit, _)| (at, hit)).collect()
}
