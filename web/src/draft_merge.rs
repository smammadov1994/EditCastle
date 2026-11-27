use crate::editor::{Clip, Project, Sequence};
use anyhow::{Context, Result, ensure};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub fn rebase(base: &Project, draft: &mut Project, saved: &Project) -> Result<()> {
    let original = placed_clips(base)?;
    let proposed = placed_clips(draft)?;
    let latest = placed_clips(saved)?;
    let clips = merge_items(&original, &proposed, &latest, |clip| clip.clip.id)
        .context("The draft and saved cut changed the same clip.")?;
    check_overlaps(&proposed, &latest, &clips)?;

    let sequence = merge_sequence(&base.sequence, &draft.sequence, &saved.sequence)?;
    let track_names = sequence
        .tracks
        .iter()
        .map(|track| (track.id, track.name.as_str()))
        .collect::<HashMap<_, _>>();
    draft.timeline = clips
        .into_iter()
        .map(|PlacedClip { mut clip, track }| {
            let name = track_names
                .get(&track)
                .context("An edited clip's track was removed.")?;
            clip.track = Some((*name).into());
            Ok(clip)
        })
        .collect::<Result<_>>()?;
    draft.sequence = sequence;
    draft.soundtrack = choose(&base.soundtrack, &draft.soundtrack, &saved.soundtrack)
        .context("The draft and saved cut changed the soundtrack.")?;
    draft.sequence_offer = choose(
        &base.sequence_offer,
        &draft.sequence_offer,
        &saved.sequence_offer,
    )
    .context("The draft and saved cut changed the sequence format offer.")?;
    Ok(())
}

fn choose<T: Clone + PartialEq>(base: &T, draft: &T, saved: &T) -> Option<T> {
    if draft == base {
        Some(saved.clone())
    } else if saved == base || saved == draft {
        Some(draft.clone())
    } else {
        None
    }
}

fn merge_items<T: Clone + PartialEq>(
    base: &[T],
    draft: &[T],
    saved: &[T],
    id: impl Fn(&T) -> Uuid,
) -> Option<Vec<T>> {
    let before = base
        .iter()
        .map(|item| (id(item), item))
        .collect::<HashMap<_, _>>();
    let proposed = draft
        .iter()
        .map(|item| (id(item), item))
        .collect::<HashMap<_, _>>();
    let latest = saved
        .iter()
        .map(|item| (id(item), item))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    // Keep the saved order, followed by items introduced by the draft.
    for key in saved
        .iter()
        .chain(draft)
        .map(id)
        .filter(|key| seen.insert(*key))
    {
        let item = choose(&before.get(&key), &proposed.get(&key), &latest.get(&key))?;
        result.extend(item.map(|item| (*item).clone()));
    }
    Some(result)
}

fn merge_sequence(base: &Sequence, draft: &Sequence, saved: &Sequence) -> Result<Sequence> {
    let format = choose(&base.format(), &draft.format(), &saved.format())
        .context("The draft and saved cut changed the sequence format.")?;
    let tracks = merge_items(&base.tracks, &draft.tracks, &saved.tracks, |track| track.id)
        .context("The draft and saved cut changed the same track.")?;
    let ids = tracks.iter().map(|track| track.id).collect::<HashSet<_>>();
    let names = tracks
        .iter()
        .map(|track| track.name.as_str())
        .collect::<HashSet<_>>();
    ensure!(
        names.len() == tracks.len()
            && names.contains("V1")
            && names.contains("A1")
            && tracks
                .iter()
                .all(|track| track.duck_under.is_none_or(|id| ids.contains(&id))),
        "The draft and saved cut made incompatible track changes."
    );
    let (in_point, out_point) = choose(
        &(base.in_point, base.out_point),
        &(draft.in_point, draft.out_point),
        &(saved.in_point, saved.out_point),
    )
    .context("The draft and saved cut changed the same marked range.")?;
    let markers = merge_items(&base.markers, &draft.markers, &saved.markers, |marker| {
        marker.id
    })
    .context("The draft and saved cut changed the same marker.")?;
    ensure!(
        markers.len() <= 500,
        "A project can have up to 500 markers."
    );
    Ok(Sequence {
        width: format.width,
        height: format.height,
        fps: format.fps,
        settings_locked: choose(
            &base.settings_locked,
            &draft.settings_locked,
            &saved.settings_locked,
        )
        .context("The draft and saved cut changed the same sequence settings.")?,
        tracks,
        markers,
        in_point,
        out_point,
        playhead: saved.playhead,
        playing: false,
        captions: choose(&base.captions, &draft.captions, &saved.captions)
            .context("The draft and saved cut changed the caption track.")?,
        render_model: choose(&base.render_model, &draft.render_model, &saved.render_model)
            .context("The draft and saved cut changed the render model.")?,
        luts: merge_items(&base.luts, &draft.luts, &saved.luts, |look| look.id)
            .context("The draft and saved cut changed the same look.")?,
    })
}

#[derive(Clone, PartialEq)]
struct PlacedClip {
    clip: Clip,
    track: Uuid,
}

fn placed_clips(project: &Project) -> Result<Vec<PlacedClip>> {
    let mut ends = HashMap::new();
    project
        .timeline
        .iter()
        .map(|original| {
            let name = original.track();
            let track = project
                .sequence
                .tracks
                .iter()
                .find(|track| track.name == name)
                .context("An edited clip's track was removed.")?
                .id;
            let next = ends.entry(track).or_insert(0_u32);
            let at = original.position.unwrap_or(*next);
            *next = (*next).max(at.saturating_add(original.duration()));
            let mut clip = original.clone();
            clip.position = Some(at);
            // Track IDs stay stable when deleting a lane renumbers the ones that follow it.
            clip.track = None;
            Ok(PlacedClip { clip, track })
        })
        .collect()
}

fn check_overlaps(draft: &[PlacedClip], saved: &[PlacedClip], merged: &[PlacedClip]) -> Result<()> {
    let draft = draft
        .iter()
        .map(|item| (item.clip.id, item))
        .collect::<HashMap<_, _>>();
    let saved = saved
        .iter()
        .map(|item| (item.clip.id, item))
        .collect::<HashMap<_, _>>();
    for (index, left) in merged.iter().enumerate() {
        for right in &merged[index + 1..] {
            if overlaps(left, right) {
                let ids = (left.clip.id, right.clip.id);
                ensure!(
                    overlap_in(&draft, ids) || overlap_in(&saved, ids),
                    "The draft and saved cut overlap on the same track."
                );
            }
        }
    }
    Ok(())
}

fn overlap_in(clips: &HashMap<Uuid, &PlacedClip>, (left, right): (Uuid, Uuid)) -> bool {
    clips
        .get(&left)
        .zip(clips.get(&right))
        .is_some_and(|(a, b)| overlaps(a, b))
}

fn overlaps(left: &PlacedClip, right: &PlacedClip) -> bool {
    let a = left.clip.position.unwrap_or(0);
    let b = right.clip.position.unwrap_or(0);
    left.track == right.track
        && a < b.saturating_add(right.clip.duration())
        && b < a.saturating_add(left.clip.duration())
}
