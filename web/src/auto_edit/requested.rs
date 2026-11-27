use super::{
    Plan, build_cut, is_picture,
    recipe::{Music, Recipe},
    song_clip, song_source, unlocked,
};
use crate::{
    clock,
    editor::{Clip, ClipKind, Project, Store},
    timeline, tools,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};
use uuid::Uuid;

pub async fn from_prompt(
    store: &Store,
    mut project: Project,
    request: &str,
    at: u32,
    selection: &[Uuid],
    work: &Path,
    mut progress: impl FnMut(String),
) -> Result<Plan> {
    progress("Reading the request on this host".into());
    let mut recipe = Recipe::analyze(store, &project, request, at, selection);
    let empty = || Plan {
        stages: Vec::new(),
        summary: String::new(),
    };
    if recipe.unsupported.contains(&"new footage generation") {
        return Ok(Plan{summary:"Choose ChatGPT or Claude to plan a new render on the studio GPU. Local can edit footage already in Media.".into(),..empty()});
    }
    if recipe.missing_scope {
        return Ok(Plan {
            summary:
                "Select the clips or mark the range you want to edit, then send the request again."
                    .into(),
            ..empty()
        });
    }
    let has_pictures = project.timeline.iter().any(is_picture);
    let has_media = project
        .sources
        .iter()
        .any(|asset| asset.video && !project.library.offline(asset.id));
    let should_assemble = !has_pictures
        && has_media
        && (recipe.needs_cut()
            || !recipe.keep.is_empty()
            || (recipe.has_settings()
                && (recipe.look.is_some()
                    || recipe.frame.is_some()
                    || recipe.speed.is_some()
                    || recipe.music == Music::Add)));
    let mut result = empty();
    if !recipe.needs_cut() && !recipe.has_settings() {
        result.summary = if recipe.unsupported.is_empty() {
            "Try a requested cut length, a look such as black and white, a title in quotes, or the media to keep. Choose ChatGPT or Claude for broader editing requests.".into()
        } else {
            format!(
                "Choose ChatGPT or Claude for {}.",
                recipe.unsupported.join(" and ")
            )
        };
        return Ok(result);
    }
    project.undo_steps.clear();
    project.redo_steps.clear();
    project.messages.clear();
    let mut reports = Vec::new();
    if has_pictures && !recipe.whole_library && filter_media(&mut project, &recipe) {
        result.stage(
            &project,
            "delete_clips",
            "Removing the requested clips and closing their gaps",
        );
        reports.push("Removed the requested clips and closed the gaps".to_owned());
    }
    let pictures_remain = project.timeline.iter().any(is_picture);
    let targeted = !recipe.clips.is_empty() || recipe.span.is_some() || recipe.at.is_some();
    let named = !recipe.keep.is_empty() || !recipe.drop.is_empty();
    let rebuild = should_assemble
        || recipe.whole_library
        || (pictures_remain
            && recipe.needs_cut()
            && !targeted
            && (!named
                || recipe.seconds.is_some()
                || recipe.pace != super::recipe::Pace::Unchanged));
    if rebuild {
        let cut = build_cut(store, project.clone(), &recipe, &mut progress).await?;
        if let Some(stage) = cut.stages.last() {
            project = stage.project.clone();
        }
        result.stages.extend(cut.stages);
        reports.push(cut.summary.trim_end_matches('.').to_owned());
    } else if recipe.needs_cut() && !pictures_remain && !has_media {
        reports.push("Add footage to Media to build a cut".into());
    }
    split_scoped_picture(store, &mut project, &mut recipe, &mut result, work).await?;
    adjustments(
        store,
        &mut project,
        &recipe,
        &mut result,
        &mut reports,
        work,
    )
    .await?;
    if targeted && (recipe.seconds.is_some() || recipe.pace != super::recipe::Pace::Unchanged) {
        reports.push("Choose ChatGPT or Claude to retime only the referenced section".into());
    }
    if !recipe.unsupported.is_empty() {
        reports.push(format!(
            "Choose ChatGPT or Claude for {}",
            recipe.unsupported.join(" and ")
        ));
    }
    result.summary = if reports.is_empty() {
        "There were no matching clips to change.".into()
    } else {
        reports.join(". ") + "."
    };
    Ok(result)
}

fn filter_media(project: &mut Project, recipe: &Recipe) -> bool {
    if recipe.keep.is_empty() && recipe.drop.is_empty() && !recipe.keep_clips && !recipe.drop_clips
    {
        return false;
    }
    let picked = project
        .timeline
        .iter()
        .filter(|clip| {
            unlocked(project, clip.track())
                && (recipe.drop.contains(&clip.source)
                    || recipe.drop_clips && recipe.clips.contains(&clip.id)
                    || ((!recipe.keep.is_empty() || recipe.keep_clips || recipe.clear_rest)
                        && (is_picture(clip) || recipe.clear_rest)
                        && !recipe.keep.contains(&clip.source)
                        && !(recipe.keep_clips && recipe.clips.contains(&clip.id))))
        })
        .map(|clip| clip.id)
        .collect::<HashSet<_>>();
    if picked.is_empty() {
        return false;
    }
    project.materialize_positions();
    let picked = timeline::linked(project, &picked);
    let tracks = project
        .timeline
        .iter()
        .filter(|clip| picked.contains(&clip.id))
        .map(|clip| clip.track().to_owned())
        .collect::<HashSet<_>>();
    // Remove only the space occupied by a removed shot, preserving the gap before the first shot.
    let before = project.timeline.len();
    project.timeline.retain(|clip| !picked.contains(&clip.id));
    let tracks: Vec<_> = tracks.into_iter().collect();
    timeline::close_all_gaps(project, &tracks);
    before != project.timeline.len()
}

fn targets(project: &Project, recipe: &Recipe, kind: impl Fn(&Clip) -> bool) -> Vec<Uuid> {
    let here = recipe
        .span
        .or_else(|| recipe.at.map(|at| (at, at.saturating_add(1))));
    project
        .timeline
        .iter()
        .filter(|clip| {
            if !kind(clip) || !unlocked(project, clip.track()) {
                return false;
            }
            if !recipe.clips.is_empty() && !recipe.clips.contains(&clip.id) {
                return false;
            }
            if let Some((from, to)) = here {
                let start = project.placement(clip.id).unwrap_or(0);
                return start < to && start.saturating_add(clip.duration()) > from;
            }
            true
        })
        .map(|clip| clip.id)
        .collect()
}

async fn edit(
    project: &mut Project,
    store: &Store,
    work: &Path,
    result: &mut Plan,
    name: &'static str,
    activity: &'static str,
    args: Value,
) -> Result<Option<Value>> {
    let response = tools::call(name, &args, project, store, work).await?;
    if !response.changed {
        return Ok(None);
    }
    project.undo_steps.clear();
    project.redo_steps.clear();
    result.stage(project, name, activity);
    Ok(Some(response.value))
}

async fn split_scoped_picture(
    store: &Store,
    project: &mut Project,
    recipe: &mut Recipe,
    result: &mut Plan,
    work: &Path,
) -> Result<()> {
    let Some((from, to)) = recipe
        .span
        .filter(|_| recipe.look.is_some() || recipe.speed.is_some())
    else {
        return Ok(());
    };
    let step = project.frame_step();
    let from = clock::snap_seconds(clock::seconds(from), step);
    let to = clock::snap_seconds(clock::seconds(to), step);
    if from >= to {
        return Ok(());
    }
    recipe.span = Some((from, to));
    let explicit = !recipe.clips.is_empty();
    for at in [from, to] {
        let ids = targets(project, recipe, |clip| {
            is_picture(clip)
                && project
                    .placement(clip.id)
                    .is_some_and(|first| at > first && at < first + clip.duration())
        });
        if ids.is_empty() {
            continue;
        }
        let changed = edit(
            project,
            store,
            work,
            result,
            "split_clips",
            "Marking the boundaries of the requested shot section",
            json!({"clip_ids":ids,"at":clock::seconds(at)}),
        )
        .await?;
        if explicit && let Some(changed) = changed {
            recipe.clips.extend(
                changed["clips"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|clip| clip["id"].as_str()?.parse::<Uuid>().ok()),
            );
        }
    }
    Ok(())
}

async fn ramp_speed(
    store: &Store,
    project: &mut Project,
    recipe: &Recipe,
    result: &mut Plan,
    work: &Path,
) -> Result<usize> {
    let Some(ramp) = &recipe.ramp else {
        return Ok(0);
    };
    let step = project.frame_step();
    let mut pictures = project
        .timeline
        .iter()
        .filter(|clip| {
            clip.kind == ClipKind::Video && !clip.effects.freeze && unlocked(project, clip.track())
        })
        .map(|clip| (project.placement(clip.id).unwrap_or(0), clip.clone()))
        .collect::<Vec<_>>();
    pictures.sort_by_key(|(at, _)| *at);
    if pictures.is_empty() {
        return Ok(0);
    }
    let explicit = targets(project, recipe, |clip| {
        clip.kind == ClipKind::Video && !clip.effects.freeze
    });
    let track = project
        .sequence
        .tracks
        .iter()
        .find(|track| pictures.iter().any(|(_, clip)| clip.track() == track.name))
        .map(|track| track.name.as_str());
    let point = recipe.at.unwrap_or(recipe.playhead);
    let chosen = if !explicit.is_empty()
        && (!ramp.cuts || explicit.len() > 1)
        && (!recipe.clips.is_empty() || recipe.span.is_some())
    {
        pictures
            .into_iter()
            .filter(|(_, clip)| explicit.contains(&clip.id))
            .collect::<Vec<_>>()
    } else if ramp.cuts {
        pictures
            .into_iter()
            .filter(|(_, clip)| Some(clip.track()) == track)
            .collect()
    } else {
        let hit = pictures.iter().position(|(at, clip)| {
            Some(clip.track()) == track && point >= *at && point < at + clip.duration()
        });
        let longest = pictures
            .iter()
            .enumerate()
            .filter(|(_, (_, clip))| Some(clip.track()) == track)
            .max_by_key(|(_, (_, clip))| clip.duration())
            .map(|(i, _)| i);
        hit.or(longest)
            .map(|i| vec![pictures.swap_remove(i)])
            .unwrap_or_default()
    };
    let mut applied = 0;
    for (i, (at, clip)) in chosen.iter().enumerate() {
        let start = clock::seconds(clip.start);
        let length = clock::seconds(clip.end - clip.start);
        if length < 0.04 {
            continue;
        }
        let (from, to, ease, speed, around) = if ramp.cuts {
            let incoming = i.checked_sub(1).is_some_and(|j| {
                let (time, prior) = &chosen[j];
                prior.track() == clip.track() && (time + prior.duration()).abs_diff(*at) < step / 2
            });
            let outgoing = chosen.get(i + 1).is_some_and(|(time, next)| {
                next.track() == clip.track() && time.abs_diff(at + clip.duration()) < step / 2
            });
            if !incoming && !outgoing {
                continue;
            }
            let (source_head, source_tail) = if clip.effects.reverse {
                (outgoing, incoming)
            } else {
                (incoming, outgoing)
            };
            (
                if source_head { 0.25 } else { 0.0 },
                if source_tail { 0.75 } else { 1.0 },
                0.25,
                clip.effects.speed,
                ramp.speed,
            )
        } else {
            let mid = if point > *at && point < at + clip.duration() {
                ((clock::seconds(clip.source_at(i64::from(point - at))) - start) / length)
                    .clamp(0.2, 0.8)
            } else {
                0.5
            };
            (mid - 0.1, mid + 0.1, 0.1, ramp.speed, clip.effects.speed)
        };
        let reply = tools::call(
            "set_speed_ramp",
            &json!({"clip_id":clip.id,"from":start+from*length,"to":start+to*length,
            "speed":speed,"around":around,"ease":ease*length,"coordinate_space":"asset"}),
            project,
            store,
            work,
        )
        .await?;
        if reply.changed {
            applied += 1;
        }
        project.undo_steps.clear();
        project.redo_steps.clear();
    }
    if applied > 0 {
        result.stage(
            project,
            "set_speed_ramp",
            "Easing the requested speed ramps",
        );
    }
    Ok(applied)
}

async fn adjustments(
    store: &Store,
    project: &mut Project,
    recipe: &Recipe,
    result: &mut Plan,
    reports: &mut Vec<String>,
    work: &Path,
) -> Result<()> {
    if recipe.remove_titles {
        let ids = targets(project, recipe, |clip| clip.kind == ClipKind::Text);
        if !ids.is_empty() {
            edit(
                project,
                store,
                work,
                result,
                "delete_clips",
                "Removing the requested titles",
                json!({"clip_ids":ids}),
            )
            .await?;
            reports.push("Removed the requested titles".into());
        }
    }
    if let Some((width, height)) = recipe.frame
        && (project.sequence.width, project.sequence.height) != (width, height)
    {
        edit(
            project,
            store,
            work,
            result,
            "set_sequence",
            "Changing the sequence shape",
            json!({"width":width,"height":height}),
        )
        .await?;
        reports.push(format!("Set the frame to {width} × {height}"));
    }
    if let Some(speed) = recipe.speed {
        let clips = targets(project, recipe, |clip| {
            clip.kind == ClipKind::Video
                && !clip.effects.freeze
                && (clip.effects.speed - speed).abs() > 0.0001
        });
        if let Some(id) = clips.first() {
            edit(
                project,
                store,
                work,
                result,
                "set_speed",
                "Changing clip speed",
                json!({"clip_id":id,"clip_ids":clips,"speed":speed}),
            )
            .await?;
            reports.push(format!(
                "Set {} video {} to {speed}× speed",
                clips.len(),
                if clips.len() == 1 { "clip" } else { "clips" }
            ));
        }
    }
    if recipe.ramp.is_some() {
        let applied = ramp_speed(store, project, recipe, result, work).await?;
        reports.push(if applied == 0 {
            "There was no matching video section to ramp".into()
        } else {
            format!(
                "Added speed ramps to {applied} {}",
                if applied == 1 { "shot" } else { "shots" }
            )
        });
    }
    if let Some(look) = &recipe.look {
        let clips = targets(project, recipe, |clip| {
            is_picture(clip) && clip.effects.grade["look"].as_str().unwrap_or("none") != look
        });
        if !clips.is_empty() {
            edit(
                project,
                store,
                work,
                result,
                "set_grade",
                "Applying the requested look",
                json!({"clip_ids":clips,"look":look}),
            )
            .await?;
            reports.push(if look == "none" {
                "Removed the look from the requested shots".into()
            } else {
                format!(
                    "Applied {look} to {} {}",
                    clips.len(),
                    if clips.len() == 1 { "shot" } else { "shots" }
                )
            });
        }
    }
    if recipe.music == Music::Off {
        let songs = project
            .timeline
            .iter()
            .filter(|clip| {
                clip.kind == ClipKind::Audio
                    && clip.link.is_none()
                    && unlocked(project, clip.track())
                    && project.source(clip.source).is_some_and(song_source)
            })
            .map(|clip| clip.id)
            .collect::<HashSet<_>>();
        let changed = !songs.is_empty() || project.soundtrack.is_some();
        if changed {
            project.materialize_positions();
            project.timeline.retain(|clip| !songs.contains(&clip.id));
            project.soundtrack = None;
            result.stage(project, "delete_clips", "Taking the music out of the cut");
            reports.push("Removed the music".into());
        }
    } else if recipe.music == Music::Add
        && song_clip(project).is_none()
        && project.soundtrack.is_none()
    {
        let song = project
            .sources
            .iter()
            .filter(|asset| song_source(asset) && !project.library.offline(asset.id))
            .max_by_key(|asset| asset.duration)
            .map(|a| a.clone());
        if let Some(song) = song
            && project.duration() > 0
        {
            let length = project.duration().min(song.duration);
            let song_track = project
                .sequence
                .tracks
                .iter()
                .filter(|track| track.name.starts_with('A') && !track.locked)
                .find(|track| project.track_end(&track.name) == 0)
                .map(|track| track.name.clone());
            let song_track = song_track.unwrap_or_else(|| timeline::add_track(project, 'A'));
            let mut clip = Clip::from_source(&song);
            clip.track = Some(song_track);
            clip.position = Some(0);
            clip.end = length;
            clip.effects.gain_db = -2.0;
            clip.effects.fade_in_frames = clock::ticks(0.3);
            clip.effects.fade_out_frames =
                clock::ticks((clock::seconds(length) * 0.15).clamp(0.5, 2.0));
            clip.clamp_fades();
            let audio_ids = project
                .timeline
                .iter()
                .filter(|c| {
                    c.link.is_some() && c.kind == ClipKind::Audio && unlocked(project, c.track())
                })
                .map(|c| c.id)
                .collect::<HashSet<_>>();
            for c in project
                .timeline
                .iter_mut()
                .filter(|clip| audio_ids.contains(&clip.id))
            {
                c.effects.gain_db = c.effects.gain_db.min(-16.0);
            }
            project.timeline.push(clip);
            result.stage(
                project,
                "add_clip",
                "Adding music and preserving the source sound under it",
            );
            reports.push(format!("Added {}", song.title));
        } else {
            reports.push("Add a song to Media to put music under the cut".into());
        }
    }
    if let Some(title) = &recipe.title {
        let cut = clock::seconds(project.duration());
        let length = recipe
            .title_duration
            .unwrap_or((cut * 0.12).clamp(2.0, 3.0).min(cut.max(2.0)));
        let (start, duration) = if let Some((from, to)) = recipe.span {
            (clock::seconds(from), clock::seconds(to - from))
        } else {
            (
                recipe
                    .at
                    .map(clock::seconds)
                    .unwrap_or(if recipe.title_at_end {
                        (cut - length).max(0.0)
                    } else {
                        0.0
                    }),
                length,
            )
        };
        let title = edit(
            project,
            store,
            work,
            result,
            "add_title",
            "Placing the requested title",
            json!({"text":title,"start":start,"duration":duration}),
        )
        .await?
        .context("The title could not be placed.")?;
        if let Some(id) = title["added"]["id"].as_str() {
            edit(
                project,
                store,
                work,
                result,
                "set_title_style",
                "Setting the title size",
                json!({"clip_id":id,"size":130}),
            )
            .await?;
            edit(
                project,
                store,
                work,
                result,
                "set_audio",
                "Fading the title in and out",
                json!({"clip_id":id,"fade_in":0.4,"fade_out":0.5}),
            )
            .await?;
        }
        reports.push(format!("Added the title at {:.2}s", start));
    }
    if recipe.sound_effects {
        let (count, note) = super::effects::place(store, project, recipe).await;
        if count > 0 {
            result.stage(project, "add_clip", "Placing sound effects near the cuts");
        }
        reports.push(note);
    }
    if recipe.level_audio {
        let picked = targets(project, recipe, |clip| {
            clip.kind == ClipKind::Audio || clip.kind == ClipKind::Video
        });
        if !picked.is_empty() {
            if let Some(levels) = edit(
                project,
                store,
                work,
                result,
                "even_out_audio_levels",
                "Matching the audio clip levels",
                json!({"clip_ids":picked,"match_output":true}),
            )
            .await?
            {
                let count = levels["adjusted"].as_array().map_or(0, Vec::len);
                reports.push(format!("Matched the levels of {count} audio clips"));
            }
        }
    }
    if let Some(add) = recipe.captions {
        let scoped = !recipe.clips.is_empty() || recipe.span.is_some() || recipe.at.is_some();
        let args = if scoped {
            let clips = targets(project, recipe, |clip| {
                matches!(clip.kind, ClipKind::Audio | ClipKind::Video)
            });
            if clips.is_empty() {
                reports.push("The requested section has no clip to caption".into());
                return Ok(());
            }
            json!({"clip_ids":clips,"remove":!add})
        } else {
            json!({"default_targets":add,"remove":!add})
        };
        match edit(
            project,
            store,
            work,
            result,
            "set_captions",
            "Updating captions on the cut",
            args,
        )
        .await
        {
            Ok(Some(_)) => reports.push(
                if add {
                    "Added captions"
                } else {
                    "Removed captions"
                }
                .into(),
            ),
            Ok(None) => {}
            Err(error) => reports.push(format!("Captions could not be updated. {error}")),
        }
    }
    project.validate_timeline(&project.timeline, project.soundtrack)?;
    Ok(())
}
