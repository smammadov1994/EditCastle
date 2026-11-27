use crate::{
    clock,
    editor::{Clip, ClipKind, Project, Source, Track},
    provider::Action,
    timeline,
};
use anyhow::{Context, Result, ensure};
use uuid::Uuid;

pub struct ShotClips {
    picture: Clip,
    audio: Option<Clip>,
}

impl ShotClips {
    pub fn new(source: &Source) -> Self {
        let mut picture = Clip::from_source(source);
        let audio = source.audio.then(|| picture.link_audio("A1".into()));
        Self { picture, audio }
    }

    pub fn asset(&self) -> Uuid {
        self.picture.source
    }

    pub fn picture_id(&self) -> Uuid {
        self.picture.id
    }
}

fn lane<'a>(project: &'a Project, clip: &Clip) -> Result<&'a Track> {
    project
        .sequence
        .tracks
        .iter()
        .find(|track| track.name == clip.track())
        .context("The selected clip's track is missing.")
}

pub fn check_target(project: &Project, clip: &Clip, action: &Action) -> Result<()> {
    let track = lane(project, clip)?;
    ensure!(track.name.starts_with('V'), "Select a picture clip.");
    ensure!(!track.locked, "Unlock the selected clip's track first.");
    if matches!(action, Action::Continue) {
        ensure!(
            clip.kind == ClipKind::Video && !track.hidden,
            "Select a visible video clip to continue."
        );
        ensure!(
            !project.library.offline(clip.source),
            "Locate the selected clip's media first."
        );
    }
    Ok(())
}

pub fn verify_target(
    base: &Project,
    saved: &Project,
    original: &Clip,
    action: &Action,
) -> Result<()> {
    let original_track = lane(base, original)?;
    let saved_clip = saved
        .timeline
        .iter()
        .find(|clip| clip.id == original.id)
        .context("The selected clip was removed during rendering. Reviewed shots are in Media.")?;
    let saved_track = lane(saved, saved_clip)?;
    let normalized = |project: &Project, clip: &Clip| {
        let mut placed = clip.clone();
        placed.position = project.placement(clip.id);
        placed.track = None;
        placed
    };
    let source_version = |project: &Project| {
        project
            .source(original.source)
            .map(|source| (source.storage_id(), source.duration))
    };
    ensure!(
        original_track.id == saved_track.id
            && normalized(base, original) == normalized(saved, saved_clip)
            && source_version(base) == source_version(saved)
            && (!matches!(action, Action::Continue)
                || base.sequence.format() == saved.sequence.format()),
        "The selected clip changed during rendering. Reviewed shots are in Media."
    );
    check_target(saved, saved_clip, action)?;
    Ok(())
}

pub fn place(
    project: &mut Project,
    shots: &[ShotClips],
    action: &Action,
    first: Option<Uuid>,
) -> Result<()> {
    let duration = shots
        .iter()
        .try_fold(0_u32, |total, shot| {
            total
                .checked_add(shot.picture.duration())
                .filter(|end| *end <= clock::MAX_TIME)
        })
        .context("The generated shots would exceed the timeline.")?;
    project.materialize_positions();
    let (track, at) = match action {
        Action::Continue | Action::ReplaceSelected => {
            let clip = project
                .timeline
                .iter()
                .find(|clip| Some(clip.id) == first)
                .context("Select a picture clip for the generated shots.")?
                .clone();
            check_target(project, &clip, action)?;
            let track = clip.track().to_owned();
            let start = clip.position.unwrap_or(0);
            let end = start + clip.duration();
            if matches!(action, Action::Continue) {
                timeline::shift_following(project, &track, end, i64::from(duration))?;
                (track, end)
            } else {
                let replaces = |item: &Clip| {
                    item.id == clip.id || clip.link.is_some() && item.link == clip.link
                };
                for item in project.timeline.iter().filter(|item| replaces(item)) {
                    ensure!(
                        !lane(project, item)?.locked,
                        "Unlock the selected clip's linked track first."
                    );
                }
                project.timeline.retain(|item| !replaces(item));
                timeline::shift_following(
                    project,
                    &track,
                    end,
                    i64::from(duration) - i64::from(clip.duration()),
                )?;
                (track, start)
            }
        }
        _ => {
            let step = project.frame_step();
            let at = project.duration().div_ceil(step) * step;
            let track = project
                .sequence
                .tracks
                .iter()
                .find(|track| track.name.starts_with('V') && !track.locked)
                .map(|track| track.name.clone())
                .unwrap_or_else(|| timeline::add_track(project, 'V'));
            (track, at)
        }
    };
    let end = at
        .checked_add(duration)
        .filter(|end| *end <= clock::MAX_TIME)
        .context("The generated shots would exceed the timeline.")?;
    let empty = |project: &Project, lane: &str| {
        project
            .timeline
            .iter()
            .filter(|clip| clip.track() == lane)
            .all(|clip| {
                let start = clip.position.unwrap_or(0);
                start >= end || start + clip.duration() <= at
            })
    };
    ensure!(
        empty(project, &track),
        "There is no room on the selected track for the generated shots."
    );
    let sound_lane = shots.iter().any(|shot| shot.audio.is_some()).then(|| {
        project
            .sequence
            .tracks
            .iter()
            .find(|lane| lane.name.starts_with('A') && !lane.locked && empty(project, &lane.name))
            .map(|lane| lane.name.clone())
            .unwrap_or_else(|| timeline::add_track(project, 'A'))
    });
    let mut start = at;
    for shot in shots {
        let mut picture = shot.picture.clone();
        picture.track = Some(track.clone());
        picture.position = Some(start);
        let position = picture.position;
        start += picture.duration();
        project.timeline.push(picture);
        if let Some(mut audio) = shot.audio.clone() {
            audio.track.clone_from(&sound_lane);
            audio.position = position;
            project.timeline.push(audio);
        }
    }
    project.validate_timeline(&project.timeline, project.soundtrack)
}
