use crate::{
    clock,
    editor::{Clip, ClipKind, Project, Track},
};
use anyhow::{Context, Result, ensure};
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

fn start(c: &Clip) -> i64 {
    i64::from(c.position.unwrap_or(0))
}

fn end(c: &Clip) -> i64 {
    start(c) + i64::from(c.duration())
}

fn unlocked(project: &Project, name: &str) -> bool {
    project
        .sequence
        .tracks
        .iter()
        .any(|t| t.name == name && !t.locked)
}

pub fn linked(project: &Project, ids: &HashSet<Uuid>) -> HashSet<Uuid> {
    let links: HashSet<_> = project
        .timeline
        .iter()
        .filter(|c| ids.contains(&c.id) && unlocked(project, c.track()))
        .filter_map(|c| c.link)
        .collect();
    project
        .timeline
        .iter()
        .filter(|c| {
            unlocked(project, c.track())
                && (ids.contains(&c.id) || c.link.is_some_and(|id| links.contains(&id)))
        })
        .map(|c| c.id)
        .collect()
}

fn next_track_name(project: &Project, kind: char) -> String {
    let next = project
        .sequence
        .tracks
        .iter()
        .filter_map(|t| t.name.strip_prefix(kind)?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    format!("{kind}{next}")
}

pub fn add_track(project: &mut Project, kind: char) -> String {
    let name = next_track_name(project, kind);
    project.sequence.tracks.push(Track::new(name.clone()));
    name
}

pub fn delete_track(project: &mut Project, id: Uuid) -> Result<Vec<Uuid>> {
    let index = project
        .sequence
        .tracks
        .iter()
        .position(|track| track.id == id)
        .context("That track is no longer in the project.")?;
    let name = project.sequence.tracks[index].name.clone();
    let kind = name.chars().next().context("The track has no kind.")?;
    ensure!(
        project
            .sequence
            .tracks
            .iter()
            .filter(|track| track.name.starts_with(kind))
            .count()
            > 1,
        "Keep at least one track of each kind."
    );
    project.sequence.tracks.remove(index);
    let mut removed = Vec::new();
    project.timeline.retain(|clip| {
        if clip.track() == name {
            removed.push(clip.id);
            return false;
        }
        true
    });
    let mut rename = HashMap::new();
    for (index, track) in project
        .sequence
        .tracks
        .iter_mut()
        .filter(|track| track.name.starts_with(kind))
        .enumerate()
    {
        let new = format!("{kind}{}", index + 1);
        rename.insert(std::mem::replace(&mut track.name, new.clone()), new);
        if track.duck_under == Some(id) {
            track.duck_under = None;
        }
    }
    for clip in &mut project.timeline {
        if let Some(name) = rename.get(clip.track()) {
            clip.track = Some(name.clone());
        }
    }
    Ok(removed)
}

pub fn first_free_video_spot(project: &Project, from: u32, duration: u32) -> Option<(String, u32)> {
    let mut best: Option<(String, u32)> = None;
    for track in project
        .sequence
        .tracks
        .iter()
        .filter(|track| track.name.starts_with('V') && !track.locked)
    {
        let mut clips = project
            .timeline
            .iter()
            .filter(|clip| clip.track() == track.name)
            .collect::<Vec<_>>();
        clips.sort_by_key(|clip| start(clip));
        let mut at = i64::from(from);
        for clip in clips {
            if start(clip) >= at + i64::from(duration) {
                break;
            }
            at = at.max(end(clip));
        }
        if at + i64::from(duration) <= i64::from(clock::MAX_TIME)
            && best
                .as_ref()
                .is_none_or(|(_, first)| at < i64::from(*first))
        {
            best = Some((track.name.clone(), at as u32));
        }
    }
    best
}

fn copied(mut c: Clip, links: &mut HashMap<Uuid, Uuid>) -> Clip {
    c.id = Uuid::new_v4();
    c.link = c
        .link
        .map(|id| *links.entry(id).or_insert_with(Uuid::new_v4));
    c
}

pub fn copy_relative(project: &Project, ids: &HashSet<Uuid>) -> Result<Vec<Clip>> {
    let mut clips = project
        .timeline
        .iter()
        .filter(|c| ids.contains(&c.id))
        .cloned()
        .collect::<Vec<_>>();
    for c in &mut clips {
        c.position = project.placement(c.id);
    }
    let first = clips
        .iter()
        .map(start)
        .min()
        .context("Select unlocked clips to copy.")?;
    for c in &mut clips {
        c.position = Some((start(c) - first) as u32);
    }
    Ok(clips)
}

pub fn paste(
    project: &mut Project,
    copies: Vec<Clip>,
    original_tracks: &HashMap<String, Uuid>,
    at: u32,
    seek: bool,
) -> Result<HashSet<Uuid>> {
    ensure!(!copies.is_empty(), "Copy clips before pasting.");
    ensure!(
        copies
            .iter()
            .all(|c| end(c) + i64::from(at) <= i64::from(clock::MAX_TIME)),
        "The pasted clips would exceed the timeline."
    );
    let mut lanes = HashMap::new();
    let mut links = HashMap::new();
    let mut ids = HashSet::new();
    let last = copies.iter().map(end).max().unwrap_or(0);
    for c in copies {
        let track = c.track().to_owned();
        let kind = if c.kind == ClipKind::Audio { 'A' } else { 'V' };
        let target = lanes.entry((track.clone(), kind)).or_insert_with(|| {
            let original = project.sequence.tracks.iter().find(|lane| {
                lane.name.starts_with(kind)
                    && !lane.locked
                    && original_tracks
                        .get(&track)
                        .map_or(lane.name == track, |id| lane.id == *id)
            });
            if let Some(lane) = original {
                return lane.name.clone();
            }
            if let Some(t) = project
                .sequence
                .tracks
                .iter()
                .find(|t| t.name.starts_with(kind) && !t.locked)
            {
                return t.name.clone();
            }
            add_track(project, kind)
        });
        let mut c = copied(c, &mut links);
        c.position = Some(at + start(&c) as u32);
        c.track = Some(target.clone());
        ids.insert(c.id);
        project.timeline.push(c);
    }
    project.overwrite(&ids);
    if seek {
        project.sequence.playhead = at + last as u32;
    }
    Ok(ids)
}

pub fn move_group(
    project: &mut Project,
    ids: &HashSet<Uuid>,
    anchor: &Clip,
    at: u32,
    destination: Option<&str>,
    duplicate: bool,
) -> Result<HashSet<Uuid>> {
    let moving = project
        .timeline
        .iter()
        .filter(|c| ids.contains(&c.id))
        .cloned()
        .collect::<Vec<_>>();
    let earliest = moving
        .iter()
        .map(start)
        .min()
        .context("Select unlocked clips to move.")?;
    let latest = moving.iter().map(end).max().unwrap();
    let delta = (i64::from(at) - start(anchor)).clamp(
        -earliest,
        (i64::from(clock::MAX_TIME) - latest).max(-earliest),
    );
    let anchor_kind = anchor.track().chars().next().unwrap();
    let lanes = |kind| {
        project
            .sequence
            .tracks
            .iter()
            .filter(|t| t.name.starts_with(kind))
            .collect::<Vec<_>>()
    };
    let anchor_lanes = lanes(anchor_kind);
    let lane_index =
        |kind, name: &str| lanes(kind).iter().position(|t| t.name == name).unwrap() as isize;
    let indices = moving
        .iter()
        .filter(|c| c.track().starts_with(anchor_kind))
        .map(|c| lane_index(anchor_kind, c.track()))
        .collect::<Vec<_>>();
    let offset = destination
        .map_or(0, |name| {
            let to = if name == "new" {
                anchor_lanes.len() as isize
            } else {
                lane_index(anchor_kind, name)
            };
            to - lane_index(anchor_kind, anchor.track())
        })
        .clamp(
            -indices.iter().min().unwrap(),
            anchor_lanes.len() as isize - indices.iter().max().unwrap(),
        );
    let offset = if indices.iter().any(|index| {
        anchor_lanes
            .get((index + offset) as usize)
            .is_some_and(|t| t.locked)
    }) {
        0
    } else {
        offset
    };
    let partner_links = moving
        .iter()
        .filter(|c| c.track().starts_with(anchor_kind))
        .filter_map(|c| c.link)
        .collect::<HashSet<_>>();
    let mut new_lanes = HashSet::new();
    let mut placements = HashMap::new();
    let mut claimed = Vec::<(String, i64, i64)>::new();
    for c in &moving {
        let kind = c.track().chars().next().unwrap();
        let kind_lanes = lanes(kind);
        let index = (lane_index(kind, c.track()) + offset).max(0) as usize;
        let to = kind_lanes.get(index);
        let at = start(c) + delta;
        let stop = end(c) + delta;
        let occupied = |name: &str| {
            project.timeline.iter().any(|other| {
                (duplicate || !ids.contains(&other.id))
                    && other.track() == name
                    && start(other) < stop
                    && end(other) > at
            }) || claimed
                .iter()
                .any(|(track, lo, hi)| track == name && *lo < stop && *hi > at)
                || moving.iter().any(|other| {
                    other.id != c.id
                        && other.track() == name
                        && start(other) < end(c)
                        && end(other) > start(c)
                })
        };
        let mut name = c.track().to_owned();
        if offset != 0
            && (kind == anchor_kind || c.link.is_some_and(|id| partner_links.contains(&id)))
        {
            if let Some(track) = to {
                if !track.locked && (kind == anchor_kind || !occupied(&track.name)) {
                    name = track.name.clone();
                }
            } else {
                let next = next_track_name(project, kind);
                if kind == anchor_kind || !occupied(&next) {
                    new_lanes.insert(kind);
                    name = next;
                }
            }
        }
        claimed.push((name.clone(), at, stop));
        placements.insert(c.id, (name, at as u32));
    }
    for kind in ['V', 'A'] {
        if new_lanes.contains(&kind) {
            add_track(project, kind);
        }
    }
    let mut result = ids.clone();
    if duplicate {
        let mut links = HashMap::new();
        result.clear();
        for c in moving {
            let (name, at) = placements.remove(&c.id).unwrap();
            let mut c = copied(c, &mut links);
            c.track = Some(name);
            c.position = Some(at);
            result.insert(c.id);
            project.timeline.push(c);
        }
    } else {
        for c in &mut project.timeline {
            if let Some((name, at)) = placements.remove(&c.id) {
                c.track = Some(name);
                c.position = Some(at);
            }
        }
    }
    project.overwrite(&result);
    Ok(result)
}

pub fn split(project: &mut Project, ids: &HashSet<Uuid>, at: u32) -> Result<HashSet<Uuid>> {
    let half_frame = i64::from(project.frame_step().div_ceil(2));
    let can_split =
        |c: &Clip| i64::from(at) - start(c) >= half_frame && end(c) - i64::from(at) >= half_frame;
    let chosen = project
        .timeline
        .iter()
        .filter(|c| can_split(c) && ids.contains(&c.id))
        .map(|c| c.id)
        .collect::<HashSet<_>>();
    let chosen = if chosen.is_empty() {
        project
            .timeline
            .iter()
            .filter(|c| can_split(c))
            .map(|c| c.id)
            .collect()
    } else {
        chosen
    };
    let chosen = linked(project, &chosen);
    let result = split_exact(project, &chosen, at);
    ensure!(
        !result.is_empty(),
        "Move the playhead inside an unlocked clip to split it."
    );
    Ok(result)
}

fn split_exact(project: &mut Project, ids: &HashSet<Uuid>, at: u32) -> HashSet<Uuid> {
    let half = i64::from(project.frame_step().div_ceil(2));
    let can_split = |c: &Clip| i64::from(at) - start(c) >= half && end(c) - i64::from(at) >= half;
    let mut links = HashMap::new();
    let mut tails = Vec::new();
    let mut result = HashSet::new();
    for c in project
        .timeline
        .iter_mut()
        .filter(|c| ids.contains(&c.id) && can_split(c))
    {
        let mut tail = copied(c.clone(), &mut links);
        c.effects.fade_out_frames = 0;
        tail.effects.fade_in_frames = 0;
        tail.fade_in = false;
        c.trim_tail(at);
        tail.trim_head(at);
        result.extend([c.id, tail.id]);
        tails.push(tail);
    }
    project.timeline.extend(tails);
    result
}

pub fn insert_freeze(
    project: &mut Project,
    clip: &Clip,
    at: u32,
    hold: u32,
) -> Result<HashSet<Uuid>> {
    ensure!(
        clip.kind == ClipKind::Video && !clip.effects.freeze,
        "Choose a moving video clip to freeze."
    );
    let at_tick = i64::from(at);
    ensure!(
        at_tick >= start(clip) && at_tick < end(clip),
        "Move the playhead inside the video to freeze a frame."
    );
    ensure!(
        at.saturating_add(hold) <= clock::MAX_TIME,
        "The freeze frame would exceed the timeline."
    );
    let ids = linked(project, &HashSet::from([clip.id]));
    let tracks = project
        .timeline
        .iter()
        .filter(|c| ids.contains(&c.id))
        .map(|c| c.track().to_owned())
        .collect::<HashSet<_>>();
    let mut changed = split_exact(project, &ids, at);
    changed.extend(&ids);
    let following = project
        .timeline
        .iter()
        .filter(|c| tracks.contains(c.track()) && start(c) >= at_tick)
        .map(|c| c.id)
        .collect::<HashSet<_>>();
    let mut shifted = linked(project, &following);
    for c in &mut project.timeline {
        if shifted.contains(&c.id) {
            ensure!(
                end(c) + i64::from(hold) <= i64::from(clock::MAX_TIME),
                "The freeze frame would move a clip beyond the timeline."
            );
            c.position = Some(start(c) as u32 + hold);
        }
    }
    let mut frozen = Clip::synthetic(ClipKind::Video, at, hold, clip.track().to_owned());
    frozen.source = clip.source;
    frozen.start = clip.visible_source_at(at - clip.position.unwrap_or(0));
    frozen.end = frozen.start + hold;
    frozen.effects.freeze = true;
    frozen.effects.grade = clip.effects.grade.clone();
    frozen.effects.transform = clip.effects.transform.clone();
    frozen.fit_transform(clip.duration(), 0);
    frozen.audio_detached = true;
    shifted.insert(frozen.id);
    project.timeline.push(frozen);
    project.overwrite(&shifted);
    changed.extend(shifted);
    Ok(changed)
}

pub fn gap(project: &Project, track: &str, at: u32) -> Option<(u32, u32)> {
    let at = i64::from(at);
    let mut before = 0;
    let mut after = None;
    for clip in project.timeline.iter().filter(|clip| clip.track() == track) {
        if start(clip) <= at && end(clip) > at {
            return None;
        }
        if end(clip) <= at {
            before = before.max(end(clip));
        }
        if start(clip) > at {
            after = Some(after.map_or(start(clip), |next: i64| next.min(start(clip))));
        }
    }
    let after = after?;
    (after > before).then_some((before as u32, after as u32))
}

fn shift_fits(project: &Project, movers: &HashSet<Uuid>, shift: i64) -> bool {
    let mut tracks: HashMap<&str, Vec<(i64, i64, bool)>> = HashMap::new();
    for clip in &project.timeline {
        let moving = movers.contains(&clip.id);
        let offset = if moving { shift } else { 0 };
        if moving && (start(clip) + offset < 0 || end(clip) + offset > i64::from(clock::MAX_TIME)) {
            return false;
        }
        tracks.entry(clip.track()).or_default().push((
            start(clip) + offset,
            end(clip) + offset,
            moving,
        ));
    }
    for track in tracks.values_mut() {
        track.sort_by_key(|&(start, _, _)| start);
        if track
            .windows(2)
            .any(|pair| (pair[0].2 || pair[1].2) && pair[0].1 > pair[1].0)
        {
            return false;
        }
    }
    true
}

pub fn shift_following(project: &mut Project, track: &str, edge: u32, shift: i64) -> Result<()> {
    if shift == 0 {
        return Ok(());
    }
    let ids = project
        .timeline
        .iter()
        .filter(|item| item.track() == track && start(item) >= i64::from(edge))
        .map(|item| item.id)
        .collect::<HashSet<_>>();
    let links = project
        .timeline
        .iter()
        .filter(|item| ids.contains(&item.id))
        .filter_map(|item| item.link)
        .collect::<HashSet<_>>();
    let movers = project
        .timeline
        .iter()
        .filter(|item| {
            ids.contains(&item.id) || item.link.is_some_and(|link| links.contains(&link))
        })
        .map(|item| {
            ensure!(
                unlocked(project, item.track()),
                "Unlock the linked track to make room for the generated shots."
            );
            Ok(item.id)
        })
        .collect::<Result<HashSet<_>>>()?;
    ensure!(
        shift_fits(project, &movers, shift),
        "There is no room to move the following clips with their linked audio."
    );
    for clip in project
        .timeline
        .iter_mut()
        .filter(|clip| movers.contains(&clip.id))
    {
        clip.position = Some((start(clip) + shift) as u32);
    }
    Ok(())
}

fn pull_left(project: &mut Project, track: &str, edge: u32, distance: u32) -> (u32, HashSet<Uuid>) {
    if distance == 0 || !unlocked(project, track) {
        return (0, HashSet::new());
    }
    let ids = project
        .timeline
        .iter()
        .filter(|clip| clip.track() == track && start(clip) >= i64::from(edge))
        .map(|clip| clip.id)
        .collect::<HashSet<_>>();
    let movers = linked(project, &ids);
    if movers.is_empty() {
        return (0, movers);
    }
    let shift = if shift_fits(project, &movers, -i64::from(distance)) {
        distance
    } else {
        // The whole jump is allowed if it fits. If blocked, find the greatest safe shift
        // by searching from the original arrangement, as the native gap command does.
        let (mut fits, mut blocked) = (0, distance);
        while blocked - fits > 1 {
            let mid = fits + (blocked - fits) / 2;
            if shift_fits(project, &movers, -i64::from(mid)) {
                fits = mid;
            } else {
                blocked = mid;
            }
        }
        let snapped = ((fits as u64 + u64::from(project.frame_step() / 2))
            / u64::from(project.frame_step()))
            * u64::from(project.frame_step());
        if snapped <= u64::from(fits) && shift_fits(project, &movers, -(snapped as i64)) {
            snapped as u32
        } else {
            fits
        }
    };
    if shift == 0 {
        return (0, HashSet::new());
    }
    for clip in project
        .timeline
        .iter_mut()
        .filter(|clip| movers.contains(&clip.id))
    {
        clip.position = Some(start(clip) as u32 - shift);
    }
    (shift, movers)
}

pub fn delete_gap(project: &mut Project, track: &str, at: u32) -> Result<(u32, HashSet<Uuid>)> {
    let (first, last) =
        gap(project, track, at).context("Choose an empty gap before a clip on this track.")?;
    Ok(pull_left(project, track, last, last - first))
}

pub fn close_all_gaps(project: &mut Project, tracks: &[String]) -> HashSet<Uuid> {
    let mut updates = HashSet::new();
    for name in tracks {
        for _ in 0..500 {
            let mut clips = project
                .timeline
                .iter()
                .filter(|clip| clip.track() == name)
                .collect::<Vec<_>>();
            clips.sort_by_key(|clip| start(clip));
            let Some(pair) = clips.windows(2).find(|pair| start(pair[1]) > end(pair[0])) else {
                break;
            };
            let edge = start(pair[1]) as u32;
            let (shift, moved) = pull_left(project, name, edge, edge - end(pair[0]) as u32);
            if shift == 0 {
                break;
            }
            updates.extend(moved);
        }
    }
    updates
}

pub fn ripple_playhead(
    project: &mut Project,
    ids: &HashSet<Uuid>,
    head: bool,
    at: u32,
) -> Result<(u32, u32)> {
    let under = project
        .timeline
        .iter()
        .filter(|c| {
            start(c) < i64::from(at) && end(c) > i64::from(at) && unlocked(project, c.track())
        })
        .collect::<Vec<_>>();
    let selected = under
        .iter()
        .filter(|c| ids.contains(&c.id))
        .map(|c| c.id)
        .collect::<HashSet<_>>();
    let tracks = if selected.is_empty() {
        project
            .sequence
            .tracks
            .iter()
            .filter(|t| !t.locked)
            .map(|t| t.name.clone())
            .collect()
    } else {
        let selected = linked(project, &selected);
        project
            .timeline
            .iter()
            .filter(|c| selected.contains(&c.id))
            .map(|c| c.track().to_owned())
            .collect::<HashSet<_>>()
    };
    let mut edges = under.into_iter().filter(|c| tracks.contains(c.track()));
    let (first, last) = if head {
        let edge = edges
            .map(start)
            .max()
            .context("Move the playhead inside an unlocked clip to ripple trim.")?;
        (edge as u32, at)
    } else {
        let edge = edges
            .by_ref()
            .map(end)
            .min()
            .context("Move the playhead inside an unlocked clip to ripple trim.")?;
        (at, edge as u32)
    };
    let half = project.frame_step().div_ceil(2);
    let gap = last - first;
    ensure!(
        gap >= half,
        "The ripple trim range is shorter than half a frame."
    );
    for at in [first, last] {
        let ids = project
            .timeline
            .iter()
            .filter(|c| tracks.contains(c.track()))
            .map(|c| c.id)
            .collect();
        split_exact(project, &ids, at);
    }
    project.timeline.retain(|c| {
        !tracks.contains(c.track())
            || start(c) < i64::from(first.saturating_sub(half))
            || end(c) > i64::from(last + half)
    });
    for c in project
        .timeline
        .iter_mut()
        .filter(|c| tracks.contains(c.track()) && start(c) >= i64::from(last.saturating_sub(half)))
    {
        c.position = Some((start(c) as u32).saturating_sub(gap));
    }
    project.sequence.playhead = first;
    Ok((first, last))
}

pub fn trim(
    project: &mut Project,
    ids: &HashSet<Uuid>,
    anchor: &Clip,
    left: bool,
    tick: i64,
    sticky: bool,
) -> Result<HashSet<Uuid>> {
    let selected = project
        .timeline
        .iter()
        .filter(|c| ids.contains(&c.id))
        .collect::<Vec<_>>();
    let other = project
        .timeline
        .iter()
        .filter(|c| !ids.contains(&c.id))
        .collect::<Vec<_>>();
    let step = i64::from(project.frame_step());
    let touch = step / 2;
    let ripple = sticky
        && (!left
            || selected.iter().any(|c| {
                start(c) <= touch
                    || other
                        .iter()
                        .any(|p| p.track() == c.track() && (end(p) - start(c)).abs() <= touch)
            }));
    let mut lo = -i64::from(clock::MAX_TIME);
    let mut hi = i64::from(clock::MAX_TIME);
    for c in selected {
        let duration = i64::from(c.duration());
        let source = project.source(c.source).filter(|_| !c.effects.freeze);
        let source_bounds = source.map(|source| {
            let (head, tail) = if c.effects.reverse {
                (source.duration, 0)
            } else {
                (0, source.duration)
            };
            (
                c.elapsed_at_source(head),
                c.elapsed_at_source(tail) - duration,
            )
        });
        let (low, high) = if left {
            let previous = other
                .iter()
                .filter(|p| p.track() == c.track() && end(p) <= start(c))
                .map(|p| end(p))
                .max()
                .unwrap_or(0);
            let low = source_bounds.map_or(end(c) - i64::from(clock::MAX_TIME), |(head, _)| head);
            let limit = if ripple {
                end(c) - i64::from(clock::MAX_TIME)
            } else {
                previous - start(c)
            };
            (low.max(limit), (duration - step).max(0))
        } else {
            let next = other
                .iter()
                .filter(|p| p.track() == c.track() && start(p) >= end(c))
                .map(|p| start(p))
                .min()
                .unwrap_or(i64::from(clock::MAX_TIME));
            let low = (step - duration).min(0);
            let high = source_bounds.map_or(i64::from(clock::MAX_TIME) - end(c), |(_, tail)| tail);
            (
                low,
                high.min(if ripple {
                    i64::from(clock::MAX_TIME) - end(c)
                } else {
                    next - end(c)
                }),
            )
        };
        lo = lo.max(low.min(0));
        hi = hi.min(high.max(0));
    }
    let edge = if left { start(anchor) } else { end(anchor) };
    let requested = (tick - edge).clamp(lo, hi);
    let attempt = |delta: i64| {
        let mut edits = HashMap::new();
        for c in project.timeline.iter().filter(|c| ids.contains(&c.id)) {
            let mut trimmed = c.clone();
            if left {
                if c.effects.freeze {
                    trimmed.end = c
                        .start
                        .checked_add((i64::from(c.duration()) - delta) as u32)?;
                } else if c.effects.reverse {
                    trimmed.end = c.source_at(delta).max(c.start + 1);
                } else {
                    trimmed.start = c.source_at(delta).min(c.end - 1);
                }
                if !ripple {
                    trimmed.position = Some((start(c) + delta) as u32);
                }
                trimmed.clamp_fades();
                trimmed.fit_transform(c.duration(), delta);
            } else {
                trimmed.trim_tail((end(c) + delta) as u32);
            }
            if end(&trimmed) > i64::from(clock::MAX_TIME) {
                return None;
            }
            edits.insert(c.id, trimmed);
        }
        if ripple {
            carry_after(project, &mut edits)?;
        }
        edits_fit(project, &edits).then_some(edits)
    };
    let mut delta = requested;
    let edits = if let Some(edits) = attempt(delta) {
        edits
    } else {
        let mut lower = 0;
        let mut upper = delta.abs();
        while upper - lower > 1 {
            let test = (lower + upper) / 2;
            if attempt(test * delta.signum()).is_some() {
                lower = test;
            } else {
                upper = test;
            }
        }
        delta = lower * delta.signum();
        attempt(delta).context("Those clip edges cannot move together.")?
    };
    let mut result = HashSet::new();
    // A limit can leave the selection unchanged.
    if delta == 0 {
        return Ok(result);
    }
    let mut edits = edits;
    for c in &mut project.timeline {
        if let Some(after) = edits.remove(&c.id) {
            result.insert(c.id);
            *c = after;
        }
    }
    Ok(result)
}

fn carry_after(project: &Project, edits: &mut HashMap<Uuid, Clip>) -> Option<()> {
    let touch = i64::from(project.frame_step()) / 2;
    let mut queue = VecDeque::new();
    let mut other = Vec::new();
    for clip in &project.timeline {
        if let Some(changed) = edits.get(&clip.id) {
            queue.push_back((clip.track(), end(clip), end(changed)));
        } else {
            other.push(clip);
        }
    }
    while let Some((track, before, after)) = queue.pop_front() {
        if before == after {
            continue;
        }
        let next = other
            .iter()
            .copied()
            .filter(|c| c.track() == track && start(c) >= before - touch)
            .min_by_key(|c| start(c));
        let Some(next) = next else { continue };
        if edits.contains_key(&next.id)
            || ((start(next) - before).abs() > touch && start(next) >= after)
        {
            continue;
        }
        let shift = after - start(next);
        let group = linked(project, &HashSet::from([next.id]));
        for c in other.iter().copied().filter(|c| group.contains(&c.id)) {
            if edits.contains_key(&c.id) {
                continue;
            }
            let at = start(c) + shift;
            if at < 0 || end(c) + shift > i64::from(clock::MAX_TIME) {
                return None;
            }
            let mut carried = c.clone();
            carried.position = Some(at as u32);
            queue.push_back((c.track(), end(c), end(&carried)));
            edits.insert(c.id, carried);
        }
    }
    Some(())
}

fn edits_fit(project: &Project, edits: &HashMap<Uuid, Clip>) -> bool {
    let mut tracks = HashMap::<&str, Vec<&Clip>>::new();
    for clip in &project.timeline {
        let clip = edits.get(&clip.id).unwrap_or(clip);
        tracks.entry(clip.track()).or_default().push(clip);
    }
    tracks.values_mut().all(|clips| {
        clips.sort_by_key(|c| start(c));
        !clips.windows(2).any(|pair| {
            end(pair[0]) > start(pair[1])
                && (edits.contains_key(&pair[0].id) || edits.contains_key(&pair[1].id))
        })
    })
}

pub fn retime(
    project: &mut Project,
    ids: &HashSet<Uuid>,
    sticky: bool,
    change: impl FnMut(&mut Clip),
) -> Result<()> {
    if !sticky {
        project.retime(ids, change);
        return Ok(());
    }
    let mut changed = project.clone();
    changed.retime(ids, change);
    let mut edits = changed
        .timeline
        .into_iter()
        .filter(|clip| ids.contains(&clip.id))
        .map(|clip| (clip.id, clip))
        .collect();
    carry_after(project, &mut edits).context("These clip edges cannot move together.")?;
    ensure!(
        edits_fit(project, &edits),
        "Sticky edges would overlap an existing clip. Adjust the speed or turn off Sticky."
    );
    for clip in &mut project.timeline {
        if let Some(edited) = edits.remove(&clip.id) {
            *clip = edited;
        }
    }
    Ok(())
}
