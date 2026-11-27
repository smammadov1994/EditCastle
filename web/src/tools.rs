use crate::{captions, clock};
use crate::{
    editor::{Clip, ClipKind, Marker, Project, Source, Store, Track},
    media, speech,
    timeline::{self, add_track},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
use uuid::Uuid;

const IMPLEMENTED: &[&str] = &[
    "get_project",
    "get_transcript",
    "set_captions",
    "get_captions",
    "set_speed_ramp",
    "auto_edit",
    "list_media",
    "remove_media",
    "note_media",
    "look_at_media",
    "analyze_media",
    "get_frame",
    "search_media",
    "add_clip",
    "place_media",
    "add_title",
    "add_solid",
    "set_solid_color",
    "move_clip",
    "move_clips",
    "copy_clips",
    "paste_clips",
    "duplicate_clips",
    "trim_clips",
    "ripple_trim",
    "split_clips",
    "trim_clip",
    "split_clip",
    "delete_clips",
    "delete_gap",
    "close_all_gaps",
    "duplicate_clip",
    "detach_audio",
    "freeze_frame",
    "reverse_clip",
    "set_speed",
    "set_audio",
    "even_out_audio_levels",
    "set_transition",
    "set_grade",
    "list_looks",
    "set_transform",
    "set_title_style",
    "add_track",
    "delete_track",
    "set_track",
    "set_sequence",
    "match_sequence_to_clip",
    "playback",
    "history",
    "save_project",
    "set_marker",
    "delete_markers",
    "set_in_out",
];

const DRAFT_VIDEO: &[&str] = &[
    "render_video_shot",
    "review_video_shot",
    "decide_video_shot",
];

pub fn native_definitions() -> Vec<Value> {
    let mut all: Vec<Value> = serde_json::from_str(include_str!("../parity/native-tools.json"))
        .expect("native tool definitions");
    all.push(json!({
        "name":"remove_media",
        "description":"Remove project media and the timeline clips that use it. Original files stay in place and the edit is undoable. Use preview to see the impact before applying. Give asset IDs from list_media or project folder IDs and paths.",
        "inputSchema":{"type":"object","properties":{
            "asset_ids":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
            "folders":{"type":"array","items":{"type":"object","properties":{
                "folder":{"type":"string"},"path":{"type":"string"}
            },"required":["folder"],"additionalProperties":false}},
            "preview":{"type":"boolean"},
            "revision":{"type":"integer","minimum":0},"library_revision":{"type":"integer","minimum":0}
        },"additionalProperties":false}
    }));
    all.push(json!({
        "name":"place_media",
        "description":"Place full media sources end to end as one overwrite edit, with linked video and sound on paired tracks. Use asset IDs in the desired order. Use a track label such as V2 or A2, a track ID, or new_video or new_audio to add a track.",
        "inputSchema":{"type":"object","properties":{
            "asset_ids":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":500},
            "start":{"type":"number","description":"Exact start on the timeline in seconds. Omit to use the playhead."},
            "track":{"type":"string","description":"Target picture or sound track. Omit to pair V1 and A1."}
        },"required":["asset_ids"],"additionalProperties":false}
    }));
    all.push(json!({
        "name":"delete_track",
        "description":"Delete one track and the clips on it in one undoable edit. Keep at least one audio and one video track. Other tracks of the same kind are renumbered.",
        "inputSchema":{"type":"object","properties":{"track":{"type":"string","description":"Track ID from get_project, or its current label such as V2."}},"required":["track"],"additionalProperties":false}
    }));
    all.push(json!({
        "name":"delete_gap",
        "description":"Close the empty gap containing a timeline time. Later clips and unlocked linked partners slide left as far as the cut allows.",
        "inputSchema":{"type":"object","properties":{"track":{"type":"string","description":"Track label or ID from get_project."},
            "at":{"type":"number","description":"Timeline time in seconds inside the gap, before a clip."}},"required":["track","at"]}
    }));
    all.push(json!({
        "name":"close_all_gaps",
        "description":"Close gaps between clips without moving the first clip. Choose a track, use the tracks of clip_ids, or omit both to use all unlocked tracks.",
        "inputSchema":{"type":"object","properties":{"track":{"type":"string","description":"Optional track label or ID."},
            "clip_ids":{"type":"array","items":{"type":"string"},"maxItems":500}}}
    }));
    all.push(json!({
        "name":"match_sequence_to_clip",
        "description":"Set sequence width, height and frame rate to the original video. Give a video clip_id from get_project or a video asset_id from list_media.",
        "inputSchema":{"type":"object","properties":{"clip_id":{"type":"string"},"asset_id":{"type":"string"}},"additionalProperties":false}
    }));
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "set_track") {
        tool["inputSchema"]["properties"]["track"]["description"] =
            json!("Track ID from get_project, or its current label such as A1.");
        tool["inputSchema"]["properties"]["duck_under"] = json!({"type":["string","null"],"description":"Audio track that triggers ducking, such as A1. Null turns ducking off."});
        tool["inputSchema"]["properties"]["duck_db"] =
            json!({"type":"number","description":"Gain reduction in dB, usually -12."});
    }
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "detach_audio") {
        tool["inputSchema"]["properties"]["clip_ids"] = json!({"type":"array","items":{"type":"string"},"maxItems":500,
            "description":"Detach sound from all these clips when clip_id is omitted."});
        tool["inputSchema"]["required"] = json!([]);
    }
    for tool in all
        .iter_mut()
        .filter(|tool| matches!(tool["name"].as_str(), Some("add_title" | "add_solid")))
    {
        tool["inputSchema"]["properties"]["track"]["description"] = json!(
            "Video track label or ID, or new_video to add a track and place the graphic in one edit."
        );
        tool["inputSchema"]["properties"]["first_free"] = json!({
            "type":"boolean",
            "description":"When no track is specified, place the graphic at the first free video spot at or after start, preserving the cut."
        });
    }
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "add_clip") {
        tool["inputSchema"]["properties"]["first_free_track"] = json!({
            "type":"boolean",
            "description":"When no track is specified, use an unlocked track that fits at start or the playhead. Create a track if needed to preserve the cut."
        });
    }
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "set_audio") {
        tool["inputSchema"]["properties"]["clip_ids"] = json!({"type":"array","items":{"type":"string"},
            "description":"Apply the same settings to these clips in one edit when clip_id is omitted."});
        tool["inputSchema"]["required"] = json!([]);
        for field in ["fade_in_curve", "fade_out_curve"] {
            tool["inputSchema"]["properties"][field] = json!({
                "type":"string","enum":["linear","easeIn","easeOut","smooth"],
                "description":"Shape of the fade. easeIn is cubic, easeOut is inverse cubic, and smooth is an S-curve."
            });
        }
    }
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "export") {
        tool["description"] = json!(
            "Queue a video or audio export. Follow export_status for progress. The file is available in the studio's export queue when finished."
        );
        tool["inputSchema"]["properties"]["remember_name"] = json!({"type":"boolean",
            "description":"Remember this filename as the project's default after queueing the export."});
    }
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "trim_clip") {
        for field in ["source_start", "source_end"] {
            tool["inputSchema"]["properties"][field] = json!({"type":"number","description":"Seconds in the original file, regardless of clip direction. Give both source_start and source_end when trimming by source time."});
        }
    }
    for tool in all
        .iter_mut()
        .filter(|tool| matches!(tool["name"].as_str(), Some("set_speed" | "set_speed_ramp")))
    {
        tool["inputSchema"]["properties"]["clip_ids"] = json!({"type":"array","items":{"type":"string"},
            "description":"Apply the change to these clips and their unlocked linked partners as one edit."});
        tool["inputSchema"]["properties"]["sticky"] = json!({"type":"boolean",
            "description":"Keep touching clips and their linked partners attached to changing clip edges."});
    }
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "set_speed") {
        tool["description"] = json!(
            "Change playback speed while keeping the source range. Supply clip_id or clip_ids, then speed or reset."
        );
        tool["inputSchema"]["properties"]["reset"] = json!({"type":"boolean",
            "description":"Restore normal speed, remove speed ramps and reset the gain of selected audio clips to 0 dB."});
        tool["inputSchema"]["required"] = json!([]);
    }
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "set_speed_ramp") {
        for field in ["from", "to"] {
            tool["inputSchema"]["properties"][field]["description"] = json!(
                "Seconds on the clip's source clock from get_project. For reversed clips, this clock counts from the file's end. Choose coordinate_space 'asset' for timestamps from look_at_media or analyze_media."
            );
        }
        tool["inputSchema"]["properties"]["coordinate_space"] = json!({"type":"string","enum":["playback","asset"],
            "description":"Defaults to the clip's playback-oriented source clock. Use asset for forward timestamps in the original file."});
        tool["inputSchema"]["properties"]["points"] = json!({"type":"array","minItems":4,"maxItems":4,
            "items":{"type":"number","minimum":0,"maximum":1},
            "description":"Optional fractional positions of the two easing intervals. Supply four ordered points across each clip's source range, from 0 to 1. Overrides from, to and ease."});
        for (field, description) in [
            ("start_speed", "Speed before the ramp. Defaults to around."),
            ("end_speed", "Speed after the ramp. Defaults to around."),
        ] {
            tool["inputSchema"]["properties"][field] =
                json!({"type":"number","description":description});
        }
    }
    all.push(json!({
        "name":"even_out_audio_levels","description":"Measure audio clip levels in the used source ranges. Equalize the given clips, or all unlocked audio clips, as one edit. Match the output to account for track gain and preserve headroom.",
        "inputSchema":{"type":"object","properties":{
            "clip_ids":{"type":"array","items":{"type":"string"}},
            "match_output":{"type":"boolean","description":"Match median output level with peak protection. Default false sets each clip to -20 dB."}
        },"additionalProperties":false}
    }));

    for (name, description) in [
        (
            "list_media_folders",
            "List the project's linked media folders and the allowed relative paths in the studio media inbox.",
        ),
        (
            "sync_media_folders",
            "Scan the project's linked studio inbox folders for added or renamed media. Files already imported keep their asset IDs.",
        ),
    ] {
        all.push(json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":{}}}));
    }
    all.push(json!({
        "name":"reload_media",
        "description":"Reload changed content of selected inbox-linked assets, a linked folder, or all linked media. Asset IDs and timeline positions stay stable. Shortened files fit existing clips, and Undo restores the previous media and cut.",
        "inputSchema":{"type":"object","properties":{
            "asset_id":{"type":"string","description":"Asset ID from list_media."},
            "asset_ids":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":500,"description":"Reload these assets together in one edit."},
            "folder_id":{"type":"string","description":"Folder ID from list_media_folders."},
            "path":{"type":"string","description":"Optional subfolder path within the linked folder."}
        }}
    }));
    all.push(json!({
        "name":"read_web_page", "description":"Read a public HTTPS page for licence terms, source facts, and links to audio. Use a page the user provided or found in search results. Returns untrusted source text and links.",
        "inputSchema":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"],"additionalProperties":false}
    }));
    all.push(json!({
        "name":"cancel_export","description":"Cancel a queued or running export for the current project. Pass the ID from export_status.",
        "inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}
    }));
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "set_in_out") {
        tool["inputSchema"]["properties"]["move_edge"] = json!({"type":"boolean",
            "description":"Move one existing bracket and preserve the other, even when the brackets cross."});
    }
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "set_grade") {
        let props = &mut tool["inputSchema"]["properties"];
        for key in crate::grade::WHEELS {
            props[key] = json!({"type":"object","description":"Position on the color wheel, within a radius of 1.",
                "properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]});
        }
        props["clip_id"] =
            json!({"type":"string","description":"One clip ID when clip_ids is omitted."});
        props["reset"] = json!({"type":"boolean","description":"Reset the clip's entire grade."});
        props["replace"] = json!({"type":["object","null"],"description":"Replace the grade with one returned by get_project. Null clears it."});
        tool["inputSchema"]["required"] = json!([]);
    }
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "set_transform") {
        tool["inputSchema"]["properties"]["clip_ids"] = json!({"type":"array","items":{"type":"string"},
            "description":"Apply the changes to these unlocked picture clips as one edit. Give clip_id or clip_ids."});
        tool["inputSchema"]["required"] = json!([]);
        let mode = tool["inputSchema"]["properties"]["zoom"].clone();
        tool["inputSchema"]["properties"]["zoom"] = json!({"oneOf":[mode,{
            "type":"object","description":"Explicit zoom control points in seconds from the clip start.",
            "properties":{"mode":{"type":"string","enum":["in","inOut","out"]},"p1":{"type":"number"},
                "p2":{"type":"number"},"p3":{"type":"number"},"p4":{"type":"number"},"smooth":{"type":"boolean"}},
            "required":["mode","p1","p2","p3","p4"],"additionalProperties":false
        }]});
    }
    if let Some(tool) = all.iter_mut().find(|tool| tool["name"] == "set_captions") {
        let props = &mut tool["inputSchema"]["properties"];
        props["edit"]["items"]["properties"]["id"] = json!({"type":"string","description":"Caption ID returned by get_captions. Use id or at to identify it."});
        props["edit"]["items"]["required"] = json!(["text"]);
        props["default_targets"] = json!({"type":"boolean","description":"Add captions to voices on the timeline even if the caption track already exists."});
        props["outline"] =
            json!({"type":"boolean","description":"Draw an outline around the caption text."});
        for key in ["text_color", "highlight_color", "outline_color"] {
            props[key] =
                json!({"type":"string","description":"Caption color, in #RRGGBB or #RRGGBBAA."});
        }
    }
    all.push(json!({
        "name":"get_captions","description":"Read the caption track and its timeline blocks, with stable IDs and word timings. Pass the ID to set_captions to correct or omit a caption.",
        "inputSchema":{"type":"object","properties":{
            "from":{"type":"number","description":"Start reading at this timeline time in seconds."},
            "to":{"type":"number","description":"Stop reading at this timeline time in seconds."}
        }}
    }));
    let clip_ids = json!({"type":"array","items":{"type":"string"},"maxItems":500});
    let time = json!({"type":"number","minimum":0,"maximum":7200});
    for (name, description, properties, required) in [
        (
            "move_clips",
            "Move the given unlocked clips and their linked partners together. Preserve time offsets and relative tracks. Start is where the anchor clip should land. Set duplicate to drag copies.",
            json!({"clip_id":{"type":"string"},"clip_ids":clip_ids,"start":time,"track":{"type":"string","description":"Destination track such as V2, or new for a new track."},"duplicate":{"type":"boolean"}}),
            json!(["clip_id", "clip_ids", "start"]),
        ),
        (
            "trim_clips",
            "Trim the same edge by a shared amount across unlocked selected clips and linked partners. Give the absolute timeline time for the anchor edge. Sticky trims carry touching footage.",
            json!({"clip_id":{"type":"string"},"clip_ids":clip_ids,"at":{"type":"number","minimum":-7200,"maximum":7200},"side":{"type":"string","enum":["start","end"]},"sticky":{"type":"boolean"},"revision":{"type":"integer","description":"Optional timeline revision for an interactive trim."}}),
            json!(["clip_id", "clip_ids", "at", "side"]),
        ),
        (
            "ripple_trim",
            "Remove the range from the nearest cut to the playhead and close the gap. Choose start or end. Use the tracks under selected clips, or every unlocked track if none is selected there.",
            json!({"clip_ids":clip_ids,"at":time,"side":{"type":"string","enum":["start","end"]}}),
            json!(["at", "side"]),
        ),
        (
            "split_clips",
            "Split unlocked selected clips and linked partners crossing the edit point. If none are selected there, split all unlocked clips crossing the point.",
            json!({"clip_ids":clip_ids,"at":time}),
            json!(["at"]),
        ),
        (
            "copy_clips",
            "Read a clipboard snapshot of the unlocked clips and linked partners, with relative positions. Pass the clipboard to paste_clips.",
            json!({"clip_ids":clip_ids}),
            json!(["clip_ids"]),
        ),
        (
            "paste_clips",
            "Paste the clip snapshot returned in copy_clips.clipboard into the same project. By default paste at the sequence playhead.",
            json!({"clipboard":{"type":"object"},"start":time}),
            json!(["clipboard"]),
        ),
        (
            "duplicate_clips",
            "Duplicate selected unlocked clips and their links as a group, after the last selected clip by default.",
            json!({"clip_ids":clip_ids,"start":time}),
            json!(["clip_ids"]),
        ),
    ] {
        all.push(json!({"name":name,"description":description,"inputSchema":{
            "type":"object","properties":properties,"required":required,"additionalProperties":false
        }}));
    }
    all
}

pub fn definitions(generation: bool) -> Vec<Value> {
    native_definitions()
        .into_iter()
        .filter(|tool| {
            let name = tool["name"].as_str().unwrap_or("");
            (IMPLEMENTED.contains(&name)
                || generation
                    && (DRAFT_VIDEO.contains(&name)
                        || [
                            "import_media",
                            "download_media",
                            "read_web_page",
                            "list_media_folders",
                            "sync_media_folders",
                        ]
                        .contains(&name)))
                && !(generation && matches!(name, "save_project" | "auto_edit"))
        })
        .map(|mut tool| {
            if tool["name"] == "auto_edit" {
                tool["description"] = json!("Build a first cut from the best moments in Media, or refine the current cut using its existing shots. Auto Edit trims and moves footage, cuts to the beat when it finds music, and commits as one Undo step.");
            }
            tool
        })
        .collect()
}

pub fn label(name: &str) -> &'static str {
    match name {
        "get_project" => "Reading the timeline",
        "get_captions" => "Reading the captions",
        "set_captions" => "Editing the captions",
        "freeze_frame" => "Inserting a freeze frame",
        "set_grade" => "Grading the picture",
        "set_transform" => "Reframing the picture",
        "set_transition" => "Editing a transition at the cut",
        "set_marker" | "delete_markers" => "Editing timeline markers",
        "set_in_out" => "Setting the marked timeline range",
        "set_audio" => "Adjusting gain and fades",
        "set_speed" | "set_speed_ramp" => "Adjusting clip playback speed",
        "add_track" => "Adding a timeline track",
        "set_track" => "Adjusting a timeline track",
        "delete_track" => "Removing a timeline track and its clips",
        "list_media" | "search_media" | "list_media_folders" => "Checking available media",
        "remove_media" => "Removing media and its timeline uses",
        "read_web_page" => "Reading a public source page",
        "download_media" => "Importing licensed media",
        "sync_media_folders" => "Scanning linked media folders",
        "look_at_media" | "analyze_media" => "Reviewing source footage",
        "get_frame" => "Checking a frame in the draft",
        "add_clip" => "Placing footage in the draft",
        "place_media" => "Arranging media on the timeline",
        "even_out_audio_levels" => "Measuring and matching audio levels",
        "move_clip" | "move_clips" => "Moving clips in the draft",
        "trim_clip" | "trim_clips" => "Trimming clips in the draft",
        "split_clip" | "split_clips" => "Splitting clips in the draft",
        "ripple_trim" => "Removing a range and closing the gap",
        "copy_clips" => "Copying clips from the draft",
        "paste_clips" | "duplicate_clips" => "Placing copies in the draft",
        "delete_clips" => "Removing clips in the draft",
        "delete_gap" | "close_all_gaps" => "Closing timeline gaps",
        "match_sequence_to_clip" => "Matching sequence to source video",
        "save_project" => "Saving the project",
        "render_video_shot" => "Preparing a video render",
        "review_video_shot" => "Checking rendered footage",
        "decide_video_shot" => "Reviewing the take",
        _ => "Updating the draft",
    }
}

pub struct Reply {
    pub value: Value,
    pub images: Vec<String>,
    pub changed: bool,
}

impl Reply {
    pub fn data(value: Value, changed: bool) -> Self {
        Self {
            value,
            images: vec![],
            changed,
        }
    }
}

fn seconds(frame: u32) -> f64 {
    (clock::seconds(frame) * 1000.0).round() / 1000.0
}

fn num(args: &Value, key: &str) -> Result<f64> {
    args[key]
        .as_f64()
        .with_context(|| format!("{key} is needed, in seconds."))
}

fn code(args: &Value, key: &str) -> Result<Uuid> {
    args[key]
        .as_str()
        .and_then(|text| text.parse().ok())
        .with_context(|| format!("{key} needs a valid ID."))
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    let value = args[key].as_str().unwrap_or_default().trim();
    ensure!(
        !value.is_empty() && value.len() <= 3000,
        "Pass a nonempty {key}."
    );
    Ok(value)
}

fn arg_time(args: &Value, key: &str, default: u32, step: u32) -> u32 {
    args[key]
        .as_f64()
        .map(|time| clock::snap_seconds(time, step))
        .unwrap_or(default)
}

fn asset<'a>(args: &Value, project: &'a Project) -> Result<&'a Source> {
    let raw = text(args, "asset_id")?;
    project
        .sources
        .iter()
        .find(|asset| raw == asset.title || raw.parse::<Uuid>().ok() == Some(asset.id))
        .context("No matching media. See list_media.")
}

fn track<'a>(
    name: &str,
    project: &'a Project,
    audio: Option<bool>,
    unlocked: bool,
) -> Result<&'a Track> {
    let id = name.trim().parse::<Uuid>().ok();
    let track = project
        .sequence
        .tracks
        .iter()
        .find(|track| Some(track.id) == id || track.name.eq_ignore_ascii_case(name.trim()))
        .with_context(|| format!("There's no {name}. Add a track first."))?;
    ensure!(
        audio.is_none_or(|a| a == track.name.starts_with('A')),
        "That track is the wrong kind."
    );
    ensure!(!unlocked || !track.locked, "That track is locked.");
    Ok(track)
}

fn place_media(args: &Value, project: &mut Project) -> Result<Value> {
    let ids = serde_json::from_value::<Vec<Uuid>>(args["asset_ids"].clone())
        .context("Pass asset_ids from list_media in placement order.")?;
    ensure!(
        (1..=500).contains(&ids.len()),
        "Place between 1 and 500 sources."
    );
    let sources = ids
        .into_iter()
        .map(|id| {
            project
                .source(id)
                .cloned()
                .context("A source is not in this project.")
        })
        .collect::<Result<Vec<_>>>()?;
    let video = project
        .sequence
        .tracks
        .iter()
        .filter(|track| track.name.starts_with('V'))
        .collect::<Vec<_>>();
    let audio = project
        .sequence
        .tracks
        .iter()
        .filter(|track| track.name.starts_with('A'))
        .collect::<Vec<_>>();
    let destination = args["track"].as_str();
    let index = match destination {
        Some("new_video") => video.len(),
        Some("new_audio") => audio.len(),
        Some(name) => {
            let target = track(name, project, None, false)?;
            let group = if target.name.starts_with('A') {
                &audio
            } else {
                &video
            };
            group
                .iter()
                .position(|track| track.id == target.id)
                .expect("existing target track")
        }
        None => 0,
    };
    let opened = |tracks: &[&Track], new| -> Option<String> {
        if new {
            return None;
        }
        let target = *tracks.get(index.min(tracks.len().saturating_sub(1)))?;
        if target.locked {
            tracks
                .iter()
                .find(|track| !track.locked)
                .map(|track| track.name.clone())
        } else {
            Some(target.name.clone())
        }
    };
    let video_track = opened(&video, destination == Some("new_video"));
    let audio_track = opened(&audio, destination == Some("new_audio"));
    let video_track = if sources.iter().any(|source| source.video) {
        video_track.or_else(|| Some(add_track(project, 'V')))
    } else {
        None
    };
    let audio_track = if sources.iter().any(|source| source.audio || !source.video) {
        audio_track.or_else(|| Some(add_track(project, 'A')))
    } else {
        None
    };
    let mut at = project.sequence.playhead;
    if let Some(start) = args["start"].as_f64() {
        ensure!(
            (0.0..=7200.0).contains(&start),
            "Choose a start on the timeline."
        );
        at = clock::ticks(start);
    }
    let step = project.frame_step();
    let mut placed = Vec::new();
    for source in sources {
        let mut clip = Clip::from_source(&source);
        clip.track = if source.video {
            video_track.clone()
        } else {
            audio_track.clone()
        };
        clip.position = Some(at);
        if !source.image {
            clip.end = ((source.duration / step).max(1) * step).min(source.duration);
        }
        at += clip.duration();
        ensure!(
            at <= clock::MAX_TIME,
            "The batch would run past the end of the timeline."
        );
        let companion = if source.video && source.audio {
            Some(clip.link_audio(audio_track.clone().expect("linked audio track")))
        } else {
            None
        };
        placed.push(clip);
        placed.extend(companion);
    }
    let ids = placed.iter().map(|clip| clip.id).collect::<HashSet<_>>();
    project.timeline.extend(placed);
    project.overwrite(&ids);
    Ok(json!({"added":summary(project,&ids)}))
}

fn clip(args: &Value, project: &Project) -> Result<Clip> {
    let id = code(args, "clip_id")?;
    let clip = project
        .timeline
        .iter()
        .find(|clip| clip.id == id)
        .context("No matching clip. See get_project.")?;
    track(clip.track(), project, None, true)?;
    Ok(clip.clone())
}

fn related(clip: &Clip, project: &Project) -> HashSet<Uuid> {
    project
        .timeline
        .iter()
        .filter(|other| {
            (other.id == clip.id || clip.link.is_some() && other.link == clip.link)
                && track(other.track(), project, None, true).is_ok()
        })
        .map(|clip| clip.id)
        .collect()
}

pub(crate) fn project_json(project: &Project) -> Value {
    let seq = &project.sequence;
    let tracks = seq.tracks.iter().rev().filter(|track| track.name.starts_with('V'))
        .chain(seq.tracks.iter().filter(|track| track.name.starts_with('A')))
        .map(|track| {
            if track.name.starts_with('V') {
                json!({"name":track.name,"id":track.id,"kind":"video","hidden":track.hidden,"locked":track.locked})
            } else {
                json!({"name":track.name,"id":track.id,"kind":"audio","muted":track.muted,"solo":track.solo,"locked":track.locked,"gain_db":track.gain_db,"duck_under":track.duck_under.and_then(|id| seq.tracks.iter().find(|other| other.id == id)).map(|other| &other.name),"duck_db":track.duck_db})
            }
        }).collect::<Vec<_>>();
    let transitions = crate::transition::layout(project);
    let cuts=transitions.cuts.iter().map(|cut| json!({
        "previous":cut.previous,"incoming":cut.incoming,"at":seconds(cut.at),
        "kind":cut.kind,"requested":seconds(cut.requested),"from_head":cut.from_head,
        "span":cut.span.map(|span| json!({"start":seconds(span.start),"end":seconds(span.end)}))
    })).collect::<Vec<_>>();
    let mut markers = seq.markers.iter().collect::<Vec<_>>();
    markers.sort_by_key(|marker| marker.frame);
    let mut clips = project.timeline.iter().collect::<Vec<_>>();
    clips.sort_by_key(|clip| (clip.track(), project.placement(clip.id).unwrap_or(0)));
    json!({
        "name":project.title, "export_name":project.export_name,
        "sequence":{"width":seq.width,"height":seq.height,"fps":seq.fps,"duration":seconds(project.duration())},
        "playhead":seconds(seq.playhead), "playing":seq.playing, "tracks":tracks, "cuts":cuts,
        "clips":clips.into_iter().map(|clip| clip_json(clip,project)).collect::<Vec<_>>(),
        "markers":markers.into_iter().map(|m|json!({"id":m.id,"time":seconds(m.frame),"name":m.name,"color":m.color})).collect::<Vec<_>>(),
        "in_out":project.play_range().map(|(start,end)|json!({"in":seconds(start),"out":seconds(end)})),
        "range_points":{"in":seq.in_point.map(seconds),"out":seq.out_point.map(seconds)},
        "captions":seq.captions.as_ref().map(|track| json!({"hidden":track.hidden,"size":track.size,"position":track.position,"highlight":track.highlight,"outline":track.outline,"clip_ids":project.timeline.iter().filter(|c| c.captioned).map(|c|c.id).collect::<Vec<_>>()})),
    })
}

pub(crate) fn asset_json(asset: &Source) -> Value {
    json!({"id": asset.id,"name":asset.title,"kind":if asset.image {"image"} else if asset.video { "video" } else { "audio" },
           "duration":seconds(asset.duration),"has_audio":asset.audio,"notes":asset.notes})
}

fn clip_json(clip: &Clip, project: &Project) -> Value {
    let start = project.placement(clip.id).unwrap_or(0);
    let fx = &clip.effects;
    let mut result = json!({
        "id":clip.id,"name":clip.name(project),"kind":clip.kind,"track":clip.track(),
        "start":seconds(start),"end":seconds(start.saturating_add(clip.duration())),"duration":seconds(clip.duration())
    });
    let backwards = fx.reverse && !fx.freeze;
    let length = project
        .source(clip.source)
        .map_or(clip.end, |source| source.duration);
    if matches!(
        clip.kind,
        ClipKind::Audio | ClipKind::Video | ClipKind::Image
    ) {
        result["asset_id"] = json!(clip.source);
        let (head, tail) = clip.playback_source_bounds(length);
        result["source_start"] = json!(seconds(head));
        if backwards {
            result["source_end"] = json!(seconds(tail));
            result["asset_source_start"] = json!(seconds(clip.start));
            result["asset_source_end"] = json!(seconds(clip.end));
        }
    }
    if clip.kind == ClipKind::Text {
        result["text"] = json!(fx.title);
        result["style"] = json!({
            "font":fx.transform["font"].as_str().filter(|name|!name.is_empty()).unwrap_or("System"),
            "size":fx.transform["size"].as_f64().unwrap_or(110.0),
            "bold":fx.transform["bold"].as_bool().unwrap_or(true),
            "color":fx.title_color(),
            "x":fx.transform["x"].as_f64().unwrap_or(0.5),
            "y":fx.transform["y"].as_f64().unwrap_or(0.5)
        });
    }
    if clip.kind == ClipKind::Solid {
        result["color"] = json!(fx.color);
    }
    if fx.speed != 1.0 {
        result["speed"] = json!(fx.speed);
    }
    if let Some(ramp) = crate::retime::Ramp::read(&fx.ramp) {
        result["speed_ramp"] = if backwards {
            ramp.mirrored(clock::seconds(length)).value()
        } else {
            ramp.value()
        };
    }
    if fx.reverse {
        result["reversed"] = json!(true);
    }
    if fx.freeze {
        result["freeze_frame"] = json!(true);
    }
    if clip.kind == ClipKind::Audio || fx.gain_db != 0.0 {
        result["gain_db"] = json!(fx.gain_db);
    }
    if fx.fade_in_frames > 0 {
        result["fade_in"] = json!(seconds(fx.fade_in_frames));
        result["fade_in_curve"] = json!(fx.fade_in_curve);
    }
    if fx.fade_out_frames > 0 {
        result["fade_out"] = json!(seconds(fx.fade_out_frames));
        result["fade_out_curve"] = json!(fx.fade_out_curve);
    }
    if let Some(kind) = &fx.transition {
        result["transition_in"] = json!({"kind":kind,"duration":seconds(fx.transition_frames)});
    }
    if let Some(link) = clip.link
        && let Some(other) = project
            .timeline
            .iter()
            .find(|other| other.link == Some(link) && other.id != clip.id)
    {
        result["linked_to"] = json!(other.id);
    }
    if !fx.grade.is_null() {
        result["grade"] = fx.grade.clone();
    }
    if matches!(
        clip.kind,
        ClipKind::Video | ClipKind::Image | ClipKind::Solid
    ) {
        let transform =
            crate::transform::Transform::read(&fx.transform, clock::seconds(clip.duration()))
                .value();
        if !transform.is_null() {
            result["transform"] = transform;
        }
    }
    result
}

fn available_track(
    project: &Project,
    prefix: char,
    skip_first: bool,
    from: u32,
    duration: u32,
) -> Option<String> {
    project
        .sequence
        .tracks
        .iter()
        .filter(|track| track.name.starts_with(prefix))
        .skip(usize::from(skip_first))
        .filter(|track| !track.locked)
        .find(|track| {
            !project
                .timeline
                .iter()
                .filter(|clip| clip.track() == track.name)
                .any(|clip| {
                    let at = project.placement(clip.id).unwrap_or(0);
                    at < from.saturating_add(duration) && at.saturating_add(clip.duration()) > from
                })
        })
        .map(|track| track.name.clone())
}

fn color(args: &Value) -> Result<String> {
    let color = text(args, "color")?.trim_start_matches('#');
    ensure!(
        color.len() == 6 && color.bytes().all(|c| c.is_ascii_hexdigit()),
        "Colours are hex, like #FF4A1C."
    );
    Ok(format!("#{color}"))
}

fn ids(args: &Value, key: &str) -> Result<HashSet<Uuid>> {
    args[key]
        .as_array()
        .with_context(|| format!("{key} is needed."))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .and_then(|id| id.parse().ok())
                .context("Use IDs returned by get_project.")
        })
        .collect()
}

pub async fn call(
    name: &str,
    args: &Value,
    project: &mut Project,
    store: &Store,
    work: &Path,
) -> Result<Reply> {
    match name {
        "get_project" => return Ok(Reply::data(project_json(project), false)),
        "playback" => {
            let command = crate::playback::Command::from_tool(args, project)?;
            if let Some(at) = command.at {
                project.sequence.playhead = at;
            }
            return Ok(Reply::data(
                json!({
                    "playhead":seconds(project.sequence.playhead),
                    "playing":false,
                    "message":"Playback is off in the draft. Use get_frame to see the cut."
                }),
                false,
            ));
        }
        "remove_media" => {
            let selection: crate::library::MediaRemoval =
                serde_json::from_value(args.clone()).context("Choose project media or folders.")?;
            let preview = args["preview"] == true;
            if !preview {
                for (field, expected) in [
                    ("revision", project.revision),
                    ("library_revision", project.library_revision),
                ] {
                    if let Some(submitted) = args.get(field) {
                        ensure!(
                            submitted.as_u64() == Some(expected),
                            "The cut or media library changed. Review the removal again."
                        );
                    }
                }
            }
            let plan = crate::library::plan_removal(project, selection)?;
            let result = plan.public(project);
            if !preview {
                plan.apply(project);
            }
            return Ok(Reply::data(result, !preview));
        }
        "copy_clips" => {
            let targets = selection(args, project)?;
            let clips = timeline::copy_relative(project, &targets)?;
            let track_ids = project
                .sequence
                .tracks
                .iter()
                .filter(|track| clips.iter().any(|clip| clip.track() == track.name))
                .map(|track| (track.name.as_str(), track.id))
                .collect::<HashMap<_, _>>();
            return Ok(Reply::data(
                json!({"clipboard":{"project":project.id,"clips":clips,"track_ids":track_ids}}),
                false,
            ));
        }
        "list_media" => {
            let summaries = crate::search::summaries(store, &project.sources);
            let pictures = media::source_pictures(store, &project.sources).await;
            let media = project
                .sources
                .iter()
                .zip(summaries)
                .map(|(source, labels)| {
                    let mut item = asset_json(source);
                    if let Some(picture) = pictures.get(&source.id) {
                        item["width"] = json!(picture.width);
                        item["height"] = json!(picture.height);
                        if let Some(fps) = picture.fps {
                            item["fps"] = json!((fps * 1000.0).round() / 1000.0);
                        }
                    }
                    if let Some(folder) = project.library.display_folder(source.id) {
                        item["folder"] = json!(folder);
                    }
                    if let Some((folder, path)) = project.library.locate(source.id) {
                        item["folder_id"] = json!(folder.id);
                        item["relative_path"] = json!(path);
                    }
                    if project.library.can_reload(source.id) {
                        item["reloadable"] = json!(true);
                    }
                    if project.library.offline(source.id) {
                        item["offline"] = json!(true);
                    }
                    if let Some(name) = project.library.missing.get(&source.id) {
                        item["missing_file"] = json!(name);
                    }
                    if let Some(labels) = labels.filter(|labels| !labels.is_empty()) {
                        item["shows"] = json!(labels);
                    }
                    if source.audio
                        && let Some(transcript) = speech::cached(store, source)
                    {
                        item["speech"] = json!(!transcript.words.is_empty());
                        if !transcript.words.is_empty() {
                            let text = transcript
                                .words
                                .iter()
                                .map(|word| word.text.as_str())
                                .collect::<Vec<_>>()
                                .join(" ");
                            let mut excerpt = text.chars().take(120).collect::<String>();
                            if excerpt.len() < text.len() {
                                excerpt.push('…');
                            }
                            item["says"] = json!(excerpt);
                        }
                    }
                    item
                })
                .collect::<Vec<_>>();
            return Ok(Reply::data(json!(media), false));
        }
        "search_media" => {
            let shows = args["shows"].as_str().unwrap_or_default();
            let said = args["said"].as_str().unwrap_or_default();
            ensure!(
                shows.chars().count() <= 300 && said.chars().count() <= 300,
                "Search can use up to 300 characters per field."
            );
            let query = crate::search::Query::agent(shows, said);
            ensure!(!query.is_empty(), "Give shows and/or said.");
            return Ok(Reply::data(
                crate::search::find(store, &project.sources, &query),
                false,
            ));
        }
        "list_looks" => {
            let mut choices = crate::grade::LOOKS
                .iter()
                .map(|name| json!(name))
                .collect::<Vec<_>>();
            choices.extend(
                project
                    .sequence
                    .luts
                    .iter()
                    .map(|lut| json!({"id":format!("lut:{}",lut.id),"name":lut.name})),
            );
            return Ok(Reply::data(json!(choices), false));
        }
        "get_transcript" => {
            let item = asset(args, project)?;
            let result = speech::transcribe(store, item).await;
            return Ok(Reply::data(
                speech::response(result, item, args["words"] == true),
                false,
            ));
        }
        "get_captions" => {
            let from = clock::ticks(args["from"].as_f64().unwrap_or(0.0));
            let to = clock::ticks(args["to"].as_f64().unwrap_or(7200.0));
            return Ok(Reply::data(caption_result(project, store, from, to), false));
        }
        "set_captions" => return caption_tools(args, project, store).await,
        "match_sequence_to_clip" => {
            let id = match (args["clip_id"].is_string(), args["asset_id"].is_string()) {
                (true, false) => {
                    let clip = clip(args, project)?;
                    ensure!(clip.kind == ClipKind::Video, "Choose a video clip.");
                    clip.source
                }
                (false, true) => code(args, "asset_id")?,
                _ => bail!("Choose exactly one clip_id or asset_id."),
            };
            let source = project
                .source(id)
                .filter(|source| source.video && !source.image)
                .context("Choose an imported or generated video.")?;
            let format = crate::media::picture_format(store, source).await?;
            let changed = project.sequence.format() != format
                || project.sequence.offers_source_format()
                || project.pending_sequence_offer().is_some();
            if changed {
                let before = project.snapshot();
                project.sequence.set_format(format);
                project.lock_sequence_settings();
                project.record(before);
            }
            return Ok(Reply::data(json!(format), changed));
        }
        "even_out_audio_levels" => {
            let selected = args["clip_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|id| {
                    id.as_str()
                        .context("Clip IDs must be strings.")?
                        .parse::<Uuid>()
                        .context("Invalid clip ID.")
                })
                .collect::<Result<Vec<_>>>()?;
            let before = project.snapshot();
            let changes =
                crate::audio::even_levels(store, project, &selected, args["match_output"] == true)
                    .await?;
            if !changes.is_empty() {
                project.record(before);
            }
            return Ok(Reply::data(
                json!({"adjusted":changes.iter().map(|(id, gain)| json!({"clip_id":id,"gain_db":gain})).collect::<Vec<_>>()}),
                !changes.is_empty(),
            ));
        }
        "analyze_media" => {
            let source = asset(args, project)?;
            let length = args["moment_length"].as_f64().unwrap_or(2.0);
            return Ok(Reply::data(
                crate::auto_edit::analyze_media(store, source, length).await?,
                false,
            ));
        }
        "look_at_media" | "get_frame" => {
            return Box::pin(inspect(name, args, project, store, work)).await;
        }
        "history" => {
            let action = text(args, "action")?;
            ensure!(
                ["undo", "redo"].contains(&action),
                "History action must be undo or redo."
            );
            project.history(action == "undo")?;
            return Ok(Reply::data(project_json(project), true));
        }
        _ => {}
    }
    let before = project.snapshot();
    let original = project.clone();
    project.materialize_positions();
    match edit(name, args, project) {
        Ok(value) => {
            if matches!(name, "trim_clips" | "delete_gap" | "close_all_gaps")
                && value["clips"].as_array().is_some_and(Vec::is_empty)
            {
                *project = original;
                return Ok(Reply::data(value, false));
            }
            if let Err(error) = project.validate_timeline(&project.timeline, project.soundtrack) {
                *project = original;
                return Err(error);
            }
            project.record(before);
            Ok(Reply::data(value, true))
        }
        Err(error) => {
            *project = original;
            Err(error)
        }
    }
}

fn selection(args: &Value, project: &Project) -> Result<HashSet<Uuid>> {
    let ids = ids(args, "clip_ids")?;
    ensure!(ids.len() <= 500, "Select up to 500 clips.");
    ensure!(
        ids.iter()
            .all(|id| project.timeline.iter().any(|c| c.id == *id)),
        "A selected clip is no longer on the timeline."
    );
    Ok(timeline::linked(project, &ids))
}

struct TrimPlan {
    anchor: Uuid,
    affected: HashSet<Uuid>,
    selected: HashSet<Uuid>,
    left: bool,
}

fn trim_request(args: &Value, project: &mut Project) -> Result<TrimPlan> {
    if let Some(revision) = args["revision"].as_u64() {
        ensure!(
            project.revision == revision,
            "The timeline changed while trimming."
        );
    }
    let anchor = clip(args, project)?;
    let selected = selection(args, project)?;
    ensure!(
        selected.contains(&anchor.id),
        "Include the anchor in clip_ids."
    );
    let left = match text(args, "side")? {
        "start" => true,
        "end" => false,
        _ => bail!("Choose the start or end edge."),
    };
    let step = project.frame_step();
    let frames = (num(args, "at")?.clamp(-7200.0, 7200.0) * f64::from(clock::TICKS_PER_SECOND)
        / f64::from(step))
    .round() as i64;
    let affected = timeline::trim(
        project,
        &selected,
        &anchor,
        left,
        frames * i64::from(step),
        args["sticky"] == true,
    )?;
    Ok(TrimPlan {
        anchor: anchor.id,
        affected,
        selected,
        left,
    })
}

pub fn trim_preview(args: &Value, project: &mut Project) -> Result<Value> {
    project.materialize_positions();
    let plan = trim_request(args, project)?;
    let anchor = project
        .timeline
        .iter()
        .find(|c| c.id == plan.anchor)
        .unwrap();
    let picture = if anchor.kind == ClipKind::Video {
        Some(anchor)
    } else {
        anchor.link.and_then(|link| {
            project
                .timeline
                .iter()
                .find(|c| c.kind == ClipKind::Video && c.link == Some(link))
        })
    };
    let frame = picture.map(|c| {
        let elapsed = if plan.left {
            0
        } else {
            c.duration().saturating_sub(project.frame_step())
        };
        json!({"id":c.source,"at":c.visible_source_at(elapsed)})
    });
    let clips = project
        .timeline
        .iter()
        .filter(|c| plan.selected.contains(&c.id) || plan.affected.contains(&c.id))
        .map(|c| {
            let mut shape = json!({"id":c.id,"track":c.track(),"start":c.position.unwrap_or(0),"duration":c.duration()});
            if c.kind == ClipKind::Audio {
                shape["audio_window"] = json!({"start":c.start,"end":c.end,"source_clock":c.source_clock()});
            }
            shape
        })
        .collect::<Vec<_>>();
    Ok(json!({"clips":clips,"frame":frame}))
}

pub fn retime_preview(args: &Value, project: &mut Project) -> Result<Value> {
    project.materialize_positions();
    let before = project
        .timeline
        .iter()
        .map(|clip| {
            (
                clip.id,
                (
                    clip.position.unwrap_or(0),
                    clip.duration(),
                    clip.effects.ramp.clone(),
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    let anchor = code(args, "clip_id")?;
    edit("set_speed_ramp", args, project)?;
    project.validate_timeline(&project.timeline, project.soundtrack)?;
    let clips = project
        .timeline
        .iter()
        .filter_map(|clip| {
            let start = clip.position.unwrap_or(0);
            let duration = clip.duration();
            let (previous_start, previous_duration, previous_ramp) = before.get(&clip.id)?;
            let ramp_changed = &clip.effects.ramp != previous_ramp;
            (*previous_start != start || *previous_duration != duration || ramp_changed).then(
                || {
                    let mut result = json!({"id":clip.id,"start":start,"duration":duration});
                    if ramp_changed {
                        result["source_clock"] = json!(clip.source_clock());
                    }
                    result
                },
            )
        })
        .collect::<Vec<_>>();
    let focus = args["focus"].as_f64().and_then(|fraction| {
        let anchor = project.timeline.iter().find(|clip| clip.id == anchor)?;
        let fraction = fraction.clamp(0.0, 1.0);
        let offset = if anchor.effects.reverse {
            1.0 - fraction
        } else {
            fraction
        };
        let source = anchor.start + (f64::from(anchor.end - anchor.start) * offset).round() as u32;
        let elapsed = anchor.elapsed_at_source(source).max(0) as u32;
        Some(
            anchor
                .position
                .unwrap_or(0)
                .saturating_add(elapsed.min(anchor.duration())),
        )
    });
    let cuts = crate::transition::layout(project)
        .cuts
        .into_iter()
        .filter(|cut| cut.span.is_some())
        .collect::<Vec<_>>();
    Ok(json!({"clips":clips,"cuts":cuts,"focus":focus,"duration":project.duration()}))
}

fn audio_settings(clip: &mut Clip, args: &Value, step: u32) -> Result<()> {
    let duration = clip.duration();
    let fx = &mut clip.effects;
    if let Some(value) = args["gain_db"].as_f64() {
        fx.gain_db = value.clamp(-60.0, 12.0);
    }
    if let Some(curve) = args.get("fade_in_curve") {
        fx.fade_in_curve =
            serde_json::from_value(curve.clone()).context("Invalid fade-in curve.")?;
    }
    if let Some(curve) = args.get("fade_out_curve") {
        fx.fade_out_curve =
            serde_json::from_value(curve.clone()).context("Invalid fade-out curve.")?;
    }
    let head = arg_time(args, "fade_in", fx.fade_in_frames, step).min(duration);
    if args.get("fade_in").is_some() {
        clip.fade_in = false;
    }
    let effective_head = if clip.fade_in {
        head.max(clock::ticks(1.0 / 3.0)).min(duration)
    } else {
        head
    };
    let tail = arg_time(args, "fade_out", fx.fade_out_frames, step).min(duration);
    if args.get("fade_out").is_some() && args.get("fade_in").is_none() {
        fx.fade_out_frames = tail.min(duration - effective_head);
    } else if args.get("fade_in").is_some() && args.get("fade_out").is_none() {
        fx.fade_in_frames = head.min(duration - tail);
    } else {
        fx.fade_in_frames = head;
        fx.fade_out_frames = tail.min(duration - effective_head);
    }
    Ok(())
}

fn accepts_transform(clip: &Clip) -> bool {
    matches!(
        clip.kind,
        ClipKind::Video | ClipKind::Image | ClipKind::Solid
    )
}

fn update_transform(clip: &mut Clip, args: &Value) -> Result<()> {
    ensure!(
        accepts_transform(clip),
        "Choose a video, still image, or solid. Titles are positioned with set_title_style."
    );
    let duration = clock::seconds(clip.duration());
    let transform = &mut clip.effects.transform;
    *transform = crate::transform::Transform::read(transform, duration)
        .updated(args, duration)?
        .value();
    Ok(())
}

fn edit(name: &str, args: &Value, project: &mut Project) -> Result<Value> {
    let step = project.frame_step();
    let snap = |time| clock::snap_seconds(time, step);

    match name {
        "move_clips" => {
            let anchor = clip(args, project)?;
            let targets = selection(args, project)?;
            ensure!(
                targets.contains(&anchor.id),
                "Include the anchor in clip_ids."
            );
            let at = snap(num(args, "start")?);
            let destination = args["track"]
                .as_str()
                .map(|name| {
                    if name == "new" {
                        return Ok(name.to_owned());
                    }
                    track(name, project, Some(anchor.kind == ClipKind::Audio), true)
                        .map(|track| track.name.clone())
                })
                .transpose()?;
            let affected = timeline::move_group(
                project,
                &targets,
                &anchor,
                at,
                destination.as_deref(),
                args["duplicate"] == true,
            )?;
            return Ok(json!({"clips":summary(project,&affected)}));
        }
        "trim_clips" => {
            let plan = trim_request(args, project)?;
            return Ok(json!({"clips":summary(project,&plan.affected)}));
        }
        "ripple_trim" => {
            let targets = if args.get("clip_ids").is_some() {
                selection(args, project)?
            } else {
                HashSet::new()
            };
            let head = match text(args, "side")? {
                "start" => true,
                "end" => false,
                _ => bail!("Choose the start or end edge."),
            };
            let (first, last) =
                timeline::ripple_playhead(project, &targets, head, snap(num(args, "at")?))?;
            return Ok(json!({"start":seconds(first),"end":seconds(last)}));
        }
        "split_clips" => {
            let targets = if args.get("clip_ids").is_some() {
                selection(args, project)?
            } else {
                HashSet::new()
            };
            let affected = timeline::split(project, &targets, snap(num(args, "at")?))?;
            return Ok(json!({"clips":summary(project,&affected)}));
        }
        "duplicate_clips" | "paste_clips" => {
            let mut original_tracks = HashMap::new();
            let (copies, default_at) = if name == "duplicate_clips" {
                let targets = selection(args, project)?;
                let end = project
                    .timeline
                    .iter()
                    .filter(|c| targets.contains(&c.id))
                    .map(|c| c.position.unwrap_or(0) + c.duration())
                    .max()
                    .unwrap_or(0);
                (timeline::copy_relative(project, &targets)?, end)
            } else {
                ensure!(
                    code(&args["clipboard"], "project")? == project.id,
                    "Copy clips from this project."
                );
                let copies =
                    serde_json::from_value::<Vec<Clip>>(args["clipboard"]["clips"].clone())
                        .context("Use the clipboard returned by copy_clips.")?;
                ensure!(copies.len() <= 500, "Paste up to 500 clips.");
                if let Some(tracks) = args["clipboard"].get("track_ids") {
                    original_tracks = serde_json::from_value(tracks.clone())
                        .context("Use the track IDs returned by copy_clips.")?;
                }
                (copies, project.sequence.playhead)
            };
            let affected = timeline::paste(
                project,
                copies,
                &original_tracks,
                arg_time(args, "start", default_at, step),
                name == "paste_clips",
            )?;
            return Ok(json!({"clips":summary(project,&affected)}));
        }
        "note_media" => {
            let id = asset(args, project)?.id;
            project
                .sources
                .iter_mut()
                .find(|asset| asset.id == id)
                .unwrap()
                .notes = text(args, "notes")?.into();
            return Ok(json!({"asset_id":id}));
        }
        "place_media" => return place_media(args, project),
        "add_clip" => {
            let asset = asset(args, project)?.clone();
            let mut c = Clip::from_source(&asset);
            if asset.image {
                c.end = arg_time(args, "duration", clock::snap_seconds(5.0, step), step)
                    .clamp(step, clock::snap_seconds(3600.0, step));
            } else {
                c.start = args["source_start"].as_f64().map(clock::ticks).unwrap_or(0);
                ensure!(c.start < c.end, "source_start is past the end of the file.");
                c.end = c
                    .start
                    .saturating_add(arg_time(args, "duration", c.end - c.start, step).max(step))
                    .min(c.end);
            }
            let (lane, at) = if args["first_free_track"] == true && args["track"].as_str().is_none()
            {
                let at = arg_time(args, "start", project.sequence.playhead, step);
                let kind = if asset.video { 'V' } else { 'A' };
                let lane = available_track(project, kind, false, at, c.duration())
                    .unwrap_or_else(|| add_track(project, kind));
                (lane, at)
            } else {
                let lane = track(
                    args["track"]
                        .as_str()
                        .unwrap_or(if asset.video { "V1" } else { "A1" }),
                    project,
                    Some(!asset.video),
                    true,
                )?
                .name
                .clone();
                let at = arg_time(args, "start", project.track_end(&lane), step);
                (lane, at)
            };
            c.track = Some(lane);
            c.position = Some(at);
            let sound = if asset.video && asset.audio {
                let track = available_track(project, 'A', false, at, c.duration())
                    .unwrap_or_else(|| add_track(project, 'A'));
                Some(c.link_audio(track))
            } else {
                None
            };
            let ids = sound
                .iter()
                .map(|a| a.id)
                .chain(std::iter::once(c.id))
                .collect::<HashSet<_>>();
            project.timeline.push(c);
            project.timeline.extend(sound);
            project.overwrite(&ids);
            return Ok(json!({"added":summary(project, &ids)}));
        }
        "add_title" | "add_solid" => {
            let title = name == "add_title";
            let at = arg_time(args, "start", 0, step);
            let minimum = if title {
                clock::snap_seconds(0.2, step)
            } else {
                step
            };
            let duration =
                arg_time(args, "duration", clock::snap_seconds(3.0, step), step).max(minimum);
            let (track, at) = match args["track"].as_str() {
                Some("new_video") => (add_track(project, 'V'), at),
                Some(name) => (track(name, project, Some(false), true)?.name.clone(), at),
                None if args["first_free"] == true => {
                    timeline::first_free_video_spot(project, at, duration)
                        .unwrap_or_else(|| (add_track(project, 'V'), at))
                }
                None => (
                    available_track(project, 'V', title, at, duration)
                        .unwrap_or_else(|| add_track(project, 'V')),
                    at,
                ),
            };
            let mut c = Clip::synthetic(
                if title {
                    ClipKind::Text
                } else {
                    ClipKind::Solid
                },
                at,
                duration,
                track,
            );
            if title {
                c.effects.title = text(args, "text")?.into();
            } else {
                c.effects.color = if args["color"].is_string() {
                    color(args)?
                } else {
                    "#000000".into()
                };
            }
            let id = c.id;
            project.timeline.push(c);
            project.overwrite(&HashSet::from([id]));
            return Ok(
                json!({"added":clip_json(project.timeline.iter().find(|clip| clip.id == id).unwrap(),project)}),
            );
        }
        "add_track" => {
            let name = match text(args, "kind")? {
                "video" => add_track(project, 'V'),
                "audio" => add_track(project, 'A'),
                _ => bail!("Choose video or audio."),
            };
            return Ok(json!({"track":name}));
        }
        "delete_track" => {
            let track = track(text(args, "track")?, project, None, false)?.clone();
            let removed = timeline::delete_track(project, track.id)?;
            return Ok(json!({"track":track.name,"track_id":track.id,"removed_clips":removed}));
        }
        "set_track" => {
            let id = track(text(args, "track")?, project, None, false)?.id;
            let duck = match args.get("duck_under") {
                Some(Value::Null) => Some(None),
                Some(Value::String(voice)) => {
                    let voice = track(voice, project, Some(true), false)?;
                    ensure!(voice.id != id, "A track cannot duck under itself.");
                    Some(Some(voice.id))
                }
                Some(_) => bail!("Choose an audio track to duck under."),
                None => None,
            };
            let track = project
                .sequence
                .tracks
                .iter_mut()
                .find(|track| track.id == id)
                .unwrap();
            if let Some(value) = args["hidden"].as_bool() {
                track.hidden = value;
            }
            if let Some(value) = args["muted"].as_bool() {
                track.muted = value;
            }
            if let Some(value) = args["solo"].as_bool() {
                track.solo = value;
            }
            if let Some(value) = args["locked"].as_bool() {
                track.locked = value;
            }
            if let Some(value) = args["gain_db"].as_f64() {
                track.gain_db = value.clamp(-60.0, 12.0);
            }
            if duck.is_some() || args.get("duck_db").is_some() {
                ensure!(
                    track.name.starts_with('A'),
                    "Ducking applies to audio tracks."
                );
                if let Some(value) = duck {
                    track.duck_under = value;
                }
                if let Some(value) = args["duck_db"].as_f64() {
                    track.duck_db = value.clamp(-60.0, 0.0);
                }
            }
            return Ok(json!(track));
        }
        "detach_audio" => {
            let ids = if args["clip_id"].is_string() {
                let clip = clip(args, project)?;
                timeline::linked(project, &HashSet::from([clip.id]))
            } else {
                selection(args, project)?
            };
            ensure!(
                project
                    .timeline
                    .iter()
                    .any(|clip| ids.contains(&clip.id) && clip.link.is_some()),
                "There is no linked audio in the selection."
            );
            for clip in &mut project.timeline {
                if ids.contains(&clip.id)
                    && clip.link.take().is_some()
                    && clip.kind == ClipKind::Video
                {
                    clip.audio_detached = true;
                }
            }
            return Ok(json!(summary(project, &ids)));
        }
        "set_sequence" => {
            for key in ["width", "height"] {
                if let Some(value) = args[key].as_u64() {
                    ensure!(
                        (128..=7680).contains(&value),
                        "Dimensions must be between 128 and 7680."
                    );
                    if key == "width" {
                        project.sequence.width = value as u32;
                    } else {
                        project.sequence.height = value as u32;
                    }
                }
            }
            if let Some(value) = args["fps"].as_f64() {
                ensure!(
                    clock::EDIT_RATES.contains(&value),
                    "Choose a supported frame rate."
                );
                project.sequence.fps = value;
            }
            if ["width", "height", "fps"]
                .iter()
                .any(|key| args[*key].is_number())
            {
                project.lock_sequence_settings();
            }
            return Ok(project_json(project)["sequence"].clone());
        }
        "set_in_out" => {
            let seq = &mut project.sequence;
            let moved = args["move_edge"] == true;
            if args["clear"] == true {
                seq.in_point = None;
                seq.out_point = None;
            } else {
                let i = args["in"].as_f64().map(snap);
                let o = args["out"].as_f64().map(snap);
                ensure!(i.is_some() || o.is_some(), "Give in, out, or clear.");
                if moved {
                    ensure!(
                        i.is_some() != o.is_some(),
                        "Move exactly one bracket at a time."
                    );
                    ensure!(
                        i.is_none() || seq.in_point.is_some(),
                        "Set an in point first."
                    );
                    ensure!(
                        o.is_none() || seq.out_point.is_some(),
                        "Set an out point first."
                    );
                } else if let (Some(i), Some(o)) = (i, o) {
                    ensure!(o > i, "The out point must follow the in point.");
                }
                if let Some(i) = i {
                    seq.in_point = Some(i);
                    if !moved && seq.out_point.is_some_and(|out| out <= i) {
                        seq.out_point = None;
                    }
                }
                if let Some(o) = o {
                    seq.out_point = Some(o);
                    if !moved && seq.in_point.is_some_and(|first| first >= o) {
                        seq.in_point = None;
                    }
                }
            }
            let points = json!({"in":seq.in_point.map(seconds),"out":seq.out_point.map(seconds)});
            let span = project.play_range();
            return Ok(json!({"in":span.map(|(first,_)|seconds(first)),
                "out":span.map(|(_,last)|seconds(last)),"points":points}));
        }
        "set_marker" => {
            let color = args["color"].as_str();
            if let Some(color) = color {
                ensure!(
                    ["white", "yellow", "green", "blue", "purple", "red"].contains(&color),
                    "Choose white, yellow, green, blue, purple, or red."
                );
            }
            let markers = &mut project.sequence.markers;
            let marker = if args["marker_id"].is_string() {
                let id = code(args, "marker_id")?;
                markers
                    .iter_mut()
                    .find(|marker| marker.id == id)
                    .context("Marker not found.")?
            } else {
                ensure!(markers.len() < 500, "A project can have up to 500 markers.");
                let frame = snap(num(args, "time")?);
                markers.push(Marker {
                    id: Uuid::new_v4(),
                    frame,
                    name: format!("Marker {}", markers.len() + 1),
                    color: "yellow".into(),
                });
                markers.last_mut().unwrap()
            };
            marker.frame = arg_time(args, "time", marker.frame, step);
            if let Some(name) = args["name"].as_str() {
                marker.name = name.chars().take(120).collect();
            }
            if let Some(color) = color {
                marker.color = color.into();
            }
            return Ok(
                json!({"id":marker.id,"time":seconds(marker.frame),"name":marker.name,"color":marker.color}),
            );
        }
        "delete_markers" => {
            if args["all"] == true {
                project.sequence.markers.clear();
            } else {
                let remove = ids(args, "marker_ids")?;
                project
                    .sequence
                    .markers
                    .retain(|marker| !remove.contains(&marker.id));
            }
            return Ok(json!({"remaining":project.sequence.markers.len()}));
        }
        "delete_clips" => {
            let targets = ids(args, "clip_ids")?;
            let remove = project
                .timeline
                .iter()
                .filter(|clip| targets.contains(&clip.id))
                .flat_map(|clip| related(clip, project))
                .collect::<HashSet<_>>();
            let gaps = project
                .timeline
                .iter()
                .filter(|clip| remove.contains(&clip.id))
                .map(|clip| {
                    (
                        clip.track().to_owned(),
                        clip.position.unwrap_or(0),
                        clip.duration(),
                    )
                })
                .collect::<Vec<_>>();
            project.timeline.retain(|clip| !remove.contains(&clip.id));
            if args["ripple"] == true {
                for clip in &mut project.timeline {
                    let at = clip.position.unwrap_or(0);
                    let total: u32 = gaps
                        .iter()
                        .filter(|(track, start, len)| {
                            track == clip.track() && start.saturating_add(*len) <= at
                        })
                        .map(|(_, _, len)| len)
                        .sum();
                    clip.position = Some(at.saturating_sub(total));
                }
            }
            return Ok(json!({"deleted":remove}));
        }
        "delete_gap" => {
            let track = track(text(args, "track")?, project, None, true)?
                .name
                .clone();
            let at = clock::ticks(num(args, "at")?);
            let (shift, changed) = timeline::delete_gap(project, &track, at)?;
            return Ok(
                json!({"track":track,"shift":seconds(shift),"clips":summary(project,&changed)}),
            );
        }
        "close_all_gaps" => {
            let tracks = if args["track"].is_string() {
                vec![
                    track(text(args, "track")?, project, None, true)?
                        .name
                        .clone(),
                ]
            } else {
                let chosen = if args["clip_ids"].is_array() {
                    ids(args, "clip_ids")?
                } else {
                    HashSet::new()
                };
                ensure!(
                    chosen.len() <= 500
                        && chosen
                            .iter()
                            .all(|id| project.timeline.iter().any(|clip| clip.id == *id)),
                    "Choose clips from the current timeline."
                );
                project
                    .sequence
                    .tracks
                    .iter()
                    .filter(|track| {
                        !track.locked
                            && (chosen.is_empty()
                                || project.timeline.iter().any(|clip| {
                                    clip.track() == track.name && chosen.contains(&clip.id)
                                }))
                    })
                    .map(|track| track.name.clone())
                    .collect::<Vec<_>>()
            };
            let changed = timeline::close_all_gaps(project, &tracks);
            return Ok(json!({"tracks":tracks,"clips":summary(project,&changed)}));
        }
        "set_grade" => {
            let chosen = if args["clip_ids"].is_array() {
                ids(args, "clip_ids")?
            } else {
                HashSet::from([code(args, "clip_id")?])
            };
            ensure!(!chosen.is_empty(), "Choose one or more picture clips.");
            ensure!(
                chosen
                    .iter()
                    .all(|id| project.timeline.iter().any(|clip| clip.id == *id)),
                "A selected clip was not found."
            );
            let editable = project
                .timeline
                .iter()
                .filter(|c| {
                    chosen.contains(&c.id)
                        && c.kind != ClipKind::Audio
                        && track(c.track(), project, None, true).is_ok()
                })
                .map(|c| c.id)
                .collect::<HashSet<_>>();
            ensure!(
                !editable.is_empty(),
                "Choose at least one unlocked picture clip."
            );
            for c in project
                .timeline
                .iter_mut()
                .filter(|c| editable.contains(&c.id))
            {
                c.effects.grade =
                    crate::grade::adjusted(&c.effects.grade, args, &project.sequence.luts)?;
            }
            return Ok(json!(summary(project, &editable)));
        }
        _ => {}
    }
    if matches!(name, "set_audio" | "set_transform") && args["clip_ids"].is_array() {
        let ids = ids(args, "clip_ids")?;
        ensure!(
            ids.iter()
                .all(|id| project.timeline.iter().any(|clip| clip.id == *id)),
            "A selected clip was not found."
        );
        let targets = project
            .timeline
            .iter()
            .filter(|clip| {
                ids.contains(&clip.id)
                    && track(clip.track(), project, None, true).is_ok()
                    && (name == "set_audio" || accepts_transform(clip))
            })
            .map(|clip| clip.id)
            .collect::<HashSet<_>>();
        ensure!(
            !targets.is_empty(),
            "Choose at least one unlocked clip that supports these settings."
        );
        for clip in &mut project.timeline {
            if targets.contains(&clip.id) {
                if name == "set_audio" {
                    audio_settings(clip, args, step)?;
                } else {
                    update_transform(clip, args)?;
                }
            }
        }
        return Ok(json!(summary(project, &targets)));
    }
    if name == "set_transition" {
        let current = clip(args, project)?;
        ensure!(
            current.kind != ClipKind::Audio,
            "Choose a picture cut for the transition."
        );
        let kind = args["kind"].as_str().unwrap_or("dissolve");
        if kind != "none" {
            ensure!(
                crate::transition::Kind::read(kind).is_some(),
                "Choose Dissolve, Dip to Black, Wipe, or None."
            );
            ensure!(
                crate::transition::layout(project)
                    .cuts
                    .iter()
                    .any(|cut| cut.incoming == current.id),
                "No picture ends at the start of that clip on its track."
            );
        }
        let clip = project
            .timeline
            .iter_mut()
            .find(|clip| clip.id == current.id)
            .unwrap();
        clip.effects.transition = (kind != "none").then(|| kind.to_owned());
        clip.effects.transition_frames = if kind == "none" {
            0
        } else {
            arg_time(
                args,
                "duration",
                if clip.effects.transition_frames == 0 {
                    clock::ticks(0.5)
                } else {
                    clip.effects.transition_frames
                },
                1,
            )
            .max(clock::ticks(0.04))
        };
        return Ok(json!(summary(project, &HashSet::from([current.id]))));
    }
    if name == "set_speed" {
        return set_playback_speed(project, args);
    }
    let current = clip(args, project)?;
    let ids = if name == "set_speed_ramp" && args["clip_ids"].is_array() {
        let ids = selection(args, project)?;
        ensure!(
            ids.contains(&current.id),
            "Include the ramp's reference clip in clip_ids."
        );
        ids
    } else {
        related(&current, project)
    };
    let mut result_ids = ids.clone();
    match name {
        "move_clip" => {
            let at = snap(num(args, "start")?);
            let destination = args["track"]
                .as_str()
                .map(|name| {
                    track(name, project, Some(current.kind == ClipKind::Audio), true)
                        .map(|t| t.name.clone())
                })
                .transpose()?;
            timeline::move_group(project, &ids, &current, at, destination.as_deref(), false)?;
        }
        "trim_clip" => {
            let clip_pos = current.position.unwrap_or(0);
            let source_times =
                if args.get("source_start").is_some() || args.get("source_end").is_some() {
                    let (Some(lo), Some(hi)) =
                        (args["source_start"].as_f64(), args["source_end"].as_f64())
                    else {
                        bail!("Give both source_start and source_end in the original file.");
                    };
                    let source = project
                        .source(current.source)
                        .filter(|_| !current.effects.freeze)
                        .context("Choose a source-based clip.")?;
                    ensure!(
                        lo >= 0.0
                            && hi > lo
                            && hi <= clock::seconds(source.duration).max(seconds(source.duration)),
                        "Set valid in and out points inside the original file."
                    );
                    let (lo, hi) = (clock::ticks(lo), clock::ticks(hi).min(source.duration));
                    ensure!(hi > lo, "Set a positive source range.");
                    let (head, tail) = if current.effects.reverse {
                        (hi, lo)
                    } else {
                        (lo, hi)
                    };
                    let at = |point| i64::from(clip_pos) + current.elapsed_at_source(point);
                    ensure!(
                        at(head) >= 0 && at(tail) <= i64::from(clock::MAX_TIME),
                        "Those source points would move the edit outside the timeline."
                    );
                    Some((
                        at(head) - i64::from(clip_pos),
                        at(tail) - i64::from(clip_pos),
                    ))
                } else {
                    None
                };
            let mut next = Vec::new();
            for c in project
                .timeline
                .iter_mut()
                .filter(|clip| ids.contains(&clip.id))
            {
                let at = c.position.unwrap_or(0);
                let end = at + c.duration();
                let (head, tail) = if let Some((head, tail)) = source_times {
                    let point = |elapsed: i64| {
                        clock::snap_seconds(
                            clock::seconds(
                                (i64::from(at) + elapsed).clamp(0, i64::from(clock::MAX_TIME))
                                    as u32,
                            ),
                            step,
                        )
                    };
                    (point(head), point(tail))
                } else {
                    (
                        args["start"].as_f64().map(snap).unwrap_or(at),
                        args["end"].as_f64().map(snap).unwrap_or(end),
                    )
                };
                ensure!(head < tail, "That would leave the clip with no length.");
                if head != at {
                    c.trim_head(head);
                }
                if tail != end {
                    c.trim_tail(tail);
                    next.push((c.track().to_owned(), end, tail));
                }
            }
            if args["sticky"] == true {
                for (track, end, to) in next {
                    let mut at = end;
                    let mut target = to;
                    let mut candidates = project
                        .timeline
                        .iter_mut()
                        .filter(|c| c.track() == track && !ids.contains(&c.id))
                        .collect::<Vec<_>>();
                    candidates.sort_by_key(|c| c.position.unwrap_or(0));
                    for c in candidates {
                        let before = c.position.unwrap_or(0);
                        if before < at {
                            continue;
                        }
                        if before > at && before >= target {
                            break;
                        }
                        c.position = Some(target);
                        at = before + c.duration();
                        target += c.duration();
                    }
                }
            }
            project.overwrite(&ids);
        }
        "split_clip" => {
            let at = snap(num(args, "at")?);
            let link = Uuid::new_v4();
            let mut right = Vec::new();
            for c in project
                .timeline
                .iter_mut()
                .filter(|clip| ids.contains(&clip.id))
            {
                let p = c.position.unwrap_or(0);
                ensure!(at > p && at < p + c.duration(), "Split inside the clip.");
                let mut tail = c.clone();
                let source_end = if c.effects.freeze {
                    c.start + at - p
                } else {
                    c.source_at(i64::from(at) - i64::from(p))
                };
                ensure!(
                    c.start < source_end && source_end < c.end,
                    "Split inside the visible clip."
                );
                if c.effects.freeze {
                    tail.end -= source_end - c.start;
                } else if c.effects.reverse {
                    tail.end = source_end;
                } else {
                    tail.start = source_end;
                }
                if c.effects.reverse && !c.effects.freeze {
                    c.start = source_end;
                } else {
                    c.end = source_end;
                }
                c.clamp_fades();
                tail.clamp_fades();
                tail.id = Uuid::new_v4();
                tail.position = Some(at);
                tail.link = c.link.map(|_| link);
                result_ids.insert(tail.id);
                right.push(tail);
            }
            project.timeline.extend(right);
        }
        "duplicate_clip" => {
            let offset = i64::from(arg_time(
                args,
                "start",
                current.position.unwrap_or(0) + current.duration(),
                step,
            )) - i64::from(current.position.unwrap_or(0));
            let link = Uuid::new_v4();
            let copies = project
                .timeline
                .iter()
                .filter(|clip| ids.contains(&clip.id))
                .map(|clip| {
                    let mut copy = clip.clone();
                    copy.id = Uuid::new_v4();
                    copy.position =
                        Some((i64::from(copy.position.unwrap_or(0)) + offset).max(0) as u32);
                    copy.link = copy.link.map(|_| link);
                    result_ids.insert(copy.id);
                    copy
                })
                .collect::<Vec<_>>();
            let winners = copies.iter().map(|clip| clip.id).collect();
            project.timeline.extend(copies);
            project.overwrite(&winners);
        }
        "set_speed_ramp" => speed_ramp(project, args, &current, &ids)?,
        "reverse_clip" => {
            ensure!(
                current.plays_source(),
                "Only source video and audio can change speed or direction."
            );
            let reverse = !current.effects.reverse;
            for clip in project
                .timeline
                .iter_mut()
                .filter(|clip| ids.contains(&clip.id) && !clip.effects.freeze)
            {
                clip.effects.reverse = reverse;
            }
        }
        "freeze_frame" => {
            let at = arg_time(args, "at", project.sequence.playhead, step);
            let hold = arg_time(args, "seconds", clock::ticks(2.0), step).max(step);
            result_ids = timeline::insert_freeze(project, &current, at, hold)?;
        }
        "set_audio" | "set_transform" | "set_title_style" | "set_solid_color" => {
            let c = project
                .timeline
                .iter_mut()
                .find(|clip| clip.id == current.id)
                .unwrap();
            let fx = &mut c.effects;
            match name {
                "set_audio" => audio_settings(c, args, step)?,
                "set_transform" => update_transform(c, args)?,
                "set_title_style" => {
                    ensure!(c.kind == ClipKind::Text, "Choose a title.");
                    let words = args["text"].as_str();
                    let mut style = serde_json::Map::new();
                    if let Some(name) = args["font"].as_str().map(str::trim) {
                        let name = if name.is_empty() { "System" } else { name };
                        let family = media::title_families().iter()
                            .find(|font| font.to_lowercase() == name.to_lowercase())
                            .with_context(|| format!("{name} is unavailable on the studio host. Choose System or an installed font."))?;
                        style.insert("font".into(), json!(family));
                    }
                    if let Some(bold) = args["bold"].as_bool() {
                        style.insert("bold".into(), json!(bold));
                    }
                    for (key, min, max) in [("size", 12.0, 600.0), ("x", 0.0, 1.0), ("y", 0.0, 1.0)]
                    {
                        if let Some(value) = args[key].as_f64() {
                            style.insert(key.into(), json!(value.clamp(min, max)));
                        }
                    }
                    if args.get("color").is_some() {
                        fx.color = color(args)?;
                        style.insert("color".into(), json!(fx.color));
                    }
                    ensure!(
                        words.is_some() || !style.is_empty(),
                        "Give at least one of text, font, size, bold, color, x, y."
                    );
                    if let Some(text) = words {
                        fx.title = text.chars().take(3000).collect();
                    }
                    if !style.is_empty() {
                        if !fx.transform.is_object() {
                            fx.transform = json!({});
                        }
                        fx.transform
                            .as_object_mut()
                            .expect("title style")
                            .extend(style);
                    }
                }
                "set_solid_color" => {
                    ensure!(c.kind == ClipKind::Solid, "Choose a solid clip.");
                    fx.color = color(args)?;
                }
                _ => unreachable!(),
            }
        }
        _ => bail!("This native tool has not been ported yet: {name}"),
    }
    Ok(json!(summary(project, &result_ids)))
}

fn summary(project: &Project, ids: &HashSet<Uuid>) -> Vec<Value> {
    project
        .timeline
        .iter()
        .filter(|c| ids.contains(&c.id))
        .map(|c| clip_json(c, project))
        .collect()
}

async fn inspect(
    name: &str,
    args: &Value,
    p: &Project,
    store: &Store,
    work: &Path,
) -> Result<Reply> {
    if name == "get_frame" {
        let step = p.frame_step();
        let duration = p.duration();
        ensure!(duration > 0, "Add clips before checking a frame.");
        let final_frame = (duration - 1) / step * step;
        let at = arg_time(args, "time", p.sequence.playhead, step).min(final_frame);
        let mut frame_project = p.clone();
        frame_project.sequence.in_point = Some(at);
        frame_project.sequence.out_point = Some(at + step);
        let subdir = work.join(format!("composite-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&subdir).await?;
        let result = async {
            let options = media::ExportOptions {
                height: Some(288),
                include_audio: Some(false),
                range_only: Some(true),
                ..Default::default()
            };
            let rendered = media::export(store, &frame_project, &subdir, &options).await?;
            let file = subdir.join("frame.jpg");
            media::extract(&rendered, 0, &file, true).await?;
            let image = STANDARD.encode(tokio::fs::read(&file).await?);
            let mut reply = Reply::data(json!({"time":seconds(at)}), false);
            reply.images.push(image);
            Ok(reply)
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&subdir).await;
        return result;
    }
    let source = asset(args, p)?;
    ensure!(
        source.video,
        "This is an audio file. Use analyze_media to find its beats."
    );
    let frames = args["frames"].as_i64().unwrap_or(6).clamp(1, 12) as usize;
    let explicit = args["times"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .take(12)
        .collect();
    let sheet = media::source_contact_sheet(store, source, frames, explicit, work).await?;
    let mut reply = Reply::data(
        json!({
            "asset_id":source.id, "name":source.title,"notes":source.notes,
            "times":sheet.times.into_iter().map(seconds).collect::<Vec<_>>(),
            "contact_sheet":{"columns":sheet.columns,"rows":sheet.rows,"order":"left to right, top to bottom"},
        }),
        false,
    );
    reply.images.push(STANDARD.encode(sheet.jpeg));
    Ok(reply)
}

fn caption_result(project: &Project, store: &Store, from: u32, to: u32) -> Value {
    let blocks = captions::blocks(project, store);
    let selected = blocks
        .iter()
        .filter(|block| block.start < to && block.end > from)
        .collect::<Vec<_>>();
    let mut output = json!(project.sequence.captions.clone().unwrap_or_default());
    if let Some(object) = output.as_object_mut() {
        object.remove("edits");
        object.remove("fixes");
    }
    output["clip_ids"] = json!(
        project
            .timeline
            .iter()
            .filter(|c| c.captioned)
            .map(|c| c.id)
            .collect::<Vec<_>>()
    );
    output["count"] = json!(blocks.len());
    output["matched"] = json!(selected.len());
    output["captions"] = json!(selected.into_iter().take(100).map(|line| json!({
        "id":line.id,"clip":line.clip,"start":seconds(line.start),"end":seconds(line.end),"text":line.text,
        "words":line.words.iter().map(|word| json!({"start":seconds(word.start),"end":seconds(word.end),"text":word.text})).collect::<Vec<_>>()
    })).collect::<Vec<_>>());
    output
}

async fn caption_tools(args: &Value, project: &mut Project, store: &Store) -> Result<Reply> {
    let before = project.snapshot();
    let mut changed = project.clone();
    let requested = if args["clip_ids"].is_array() {
        ids(args, "clip_ids")?
    } else {
        HashSet::new()
    };
    ensure!(
        requested
            .iter()
            .all(|id| changed.timeline.iter().any(|clip| clip.id == *id)),
        "A clip was not found."
    );
    let remove = args["remove"] == true;
    if remove {
        let targets = captions::targets(&changed, &requested);
        for clip in &mut changed.timeline {
            if requested.is_empty() || targets.contains(&clip.id) {
                clip.captioned = false;
            }
        }
        let used = changed
            .timeline
            .iter()
            .filter(|c| c.captioned)
            .map(|c| c.source)
            .collect::<HashSet<_>>();
        if used.is_empty() {
            changed.sequence.captions = None;
        } else if let Some(track) = &mut changed.sequence.captions {
            track.fixes.retain(|id, _| used.contains(id));
            track
                .edits
                .retain(|e| e.source.is_some_and(|id| used.contains(&id)));
        }
    } else {
        let need_defaults = changed.sequence.captions.is_none() || args["default_targets"] == true;
        let targets = if requested.is_empty() && need_defaults {
            captions::default_targets(&changed)
        } else {
            captions::targets(&changed, &requested)
        };
        let targets = targets
            .into_iter()
            .filter(|id| {
                changed
                    .timeline
                    .iter()
                    .any(|c| c.id == *id && track(c.track(), &changed, None, true).is_ok())
            })
            .collect::<HashSet<_>>();
        ensure!(
            changed.sequence.captions.is_some() || !targets.is_empty(),
            "Select a clip with speech to add captions."
        );
        ensure!(
            requested.is_empty() || !targets.is_empty(),
            "The selected clips have no unlocked sound to caption."
        );
        let mut transcribed = HashSet::new();
        for clip in changed
            .timeline
            .iter_mut()
            .filter(|c| targets.contains(&c.id))
        {
            clip.captioned = true;
            transcribed.insert(clip.source);
        }
        for id in transcribed {
            if let Some(source) = changed.source(id) {
                speech::transcribe(store, source).await?;
            }
        }
        let shown = if args["edit"].is_array() {
            changed
                .sequence
                .captions
                .get_or_insert_with(Default::default);
            captions::blocks(&changed, store)
        } else {
            vec![]
        };
        let track = changed
            .sequence
            .captions
            .get_or_insert_with(Default::default);
        if let Some(size) = args["size"].as_f64() {
            track.size = size.clamp(28.0, 140.0);
        }
        if let Some(position) = args["position"].as_f64() {
            track.position = position.clamp(0.1, 0.95);
        }
        if let Some(hidden) = args["hidden"].as_bool() {
            track.hidden = hidden;
        }
        if let Some(highlight) = args["highlight"].as_bool() {
            track.highlight = highlight;
        }
        if let Some(outline) = args["outline"].as_bool() {
            track.outline = outline;
        }
        for (key, field) in [
            ("text_color", &mut track.text_color),
            ("highlight_color", &mut track.highlight_color),
            ("outline_color", &mut track.outline_color),
        ] {
            if let Some(value) = args[key].as_str() {
                let hex = value.trim_start_matches('#');
                ensure!(
                    [6, 8].contains(&hex.len()) && hex.bytes().all(|c| c.is_ascii_hexdigit()),
                    "Use a hex caption color, like #FFFFFF."
                );
                *field = format!("#{hex}");
            }
        }
        if let Some(edits) = args["edit"].as_array() {
            ensure!(edits.len() <= 500, "Edit up to 500 captions at once.");
            for edit in edits {
                let block = if let Some(id) = edit["id"].as_str() {
                    shown.iter().find(|b| b.id == id)
                } else {
                    let at = clock::ticks(num(edit, "at")?);
                    shown
                        .iter()
                        .find(|b| at >= b.start && at < b.end)
                        .or_else(|| {
                            shown
                                .iter()
                                .filter(|b| {
                                    at >= b.start.saturating_sub(clock::ticks(0.5))
                                        && at < b.end.saturating_add(clock::ticks(0.5))
                                })
                                .min_by_key(|b| (b.start + (b.end - b.start) / 2).abs_diff(at))
                        })
                }
                .context("That caption is no longer at the requested location.")?;
                let text = edit["text"]
                    .as_str()
                    .context("Supply the caption text.")?
                    .chars()
                    .take(300)
                    .collect::<String>();
                if text.split_whitespace().collect::<Vec<_>>().join(" ") != block.text {
                    captions::apply_fix(track, block, &text);
                }
            }
        }
    }
    changed.record(before);
    let result = caption_result(&changed, store, 0, clock::MAX_TIME);
    *project = changed;
    Ok(Reply::data(result, true))
}

fn set_playback_speed(project: &mut Project, args: &Value) -> Result<Value> {
    let reset = args["reset"] == true;
    let ids = if args["clip_ids"].is_array() {
        selection(args, project)?
    } else {
        let current = clip(args, project)?;
        ensure!(
            current.plays_source() || reset && current.kind == ClipKind::Audio,
            "Choose unlocked source video or audio to change its playback settings."
        );
        related(&current, project)
    };
    ensure!(
        project.timeline.iter().any(|clip| {
            ids.contains(&clip.id) && (clip.plays_source() || reset && clip.kind == ClipKind::Audio)
        }),
        "Choose unlocked source video or audio to change its playback settings."
    );
    let speed = if reset {
        1.0
    } else {
        num(args, "speed")?.clamp(0.05, 100.0)
    };
    timeline::retime(project, &ids, args["sticky"] == true, |clip| {
        clip.effects.speed = speed;
        if reset {
            clip.effects.ramp = Value::Null;
        } else if clip.effects.ramp.is_object() {
            clip.effects.ramp["speed"] = json!(speed);
        }
    })?;
    if reset {
        for clip in project
            .timeline
            .iter_mut()
            .filter(|clip| ids.contains(&clip.id) && clip.kind == ClipKind::Audio)
        {
            clip.effects.gain_db = 0.0;
        }
    }
    Ok(json!(summary(project, &ids)))
}

fn speed_ramp(
    project: &mut Project,
    args: &Value,
    current: &Clip,
    ids: &HashSet<Uuid>,
) -> Result<()> {
    ensure!(
        args["revision"]
            .as_u64()
            .is_none_or(|revision| revision == project.revision)
            && args["library_revision"]
                .as_u64()
                .is_none_or(|revision| revision == project.library_revision),
        "The project changed while adjusting the speed ramp."
    );
    ensure!(
        current.plays_source(),
        "Only source video and audio have a speed ramp."
    );
    let sticky = args["sticky"] == true;
    if args["remove"] == true {
        return timeline::retime(project, ids, sticky, |clip| clip.effects.ramp = Value::Null);
    }
    let speed = num(args, "speed")?.clamp(0.05, 100.0);
    let around = args["around"].as_f64().unwrap_or(1.0).clamp(0.05, 100.0);
    let playback = match args["coordinate_space"].as_str().unwrap_or("playback") {
        "playback" => true,
        "asset" => false,
        _ => bail!("Use playback or asset as the ramp's coordinate_space."),
    };
    let lengths = project
        .sources
        .iter()
        .map(|source| (source.id, source.duration))
        .collect::<HashMap<_, _>>();
    let range = |clip: &Clip| {
        let length = lengths.get(&clip.source).copied().unwrap_or(clip.end);
        let mirror = playback && clip.effects.reverse && !clip.effects.freeze;
        let (start, end) = if mirror {
            clip.playback_source_bounds(length)
        } else {
            (clip.start, clip.end)
        };
        (
            clock::seconds(start),
            clock::seconds(end),
            mirror.then(|| clock::seconds(length)),
        )
    };
    let (start, end, _) = range(current);
    let source_length = end - start;
    let [in_from, in_to, out_from, out_to] = if let Some(points) = args.get("points") {
        let points: [f64; 4] = serde_json::from_value(points.clone())
            .context("Provide four fractional ramp points.")?;
        ensure!(
            (0.0..=1.0).contains(&points[0])
                && points[3] <= 1.0
                && points.windows(2).all(|pair| pair[0] <= pair[1]),
            "Put the four ramp points in order between 0 and 1."
        );
        points
    } else {
        ensure!(
            args["from"].is_number() || args["to"].is_number(),
            "Set a ramp from and/or to a time inside the source."
        );
        let from = args["from"].as_f64().unwrap_or(start).clamp(start, end);
        let to = args["to"].as_f64().unwrap_or(end).clamp(from, end);
        let ease = args["ease"].as_f64().unwrap_or(0.4).max(0.04);
        let fraction = |at: f64| ((at - start) / source_length).clamp(0.0, 1.0);
        let (in_from, in_to) = if from > start + 0.001 {
            (fraction(from - ease), fraction(from))
        } else {
            (0.0, 0.0)
        };
        let (out_from, out_to) = if to < end - 0.001 {
            (fraction(to), fraction(to + ease))
        } else {
            (1.0, 1.0)
        };
        [in_from, in_to, out_from, out_to]
    };
    let has_in = in_to - in_from > 0.001;
    let has_out = out_to - out_from > 0.001;
    ensure!(
        has_in || has_out,
        "Set at least one ramp edge inside the source ({start:.3}s to {end:.3}s)."
    );
    let start_speed = args["start_speed"]
        .as_f64()
        .unwrap_or(around)
        .clamp(0.05, 100.0);
    let end_speed = args["end_speed"]
        .as_f64()
        .unwrap_or(around)
        .clamp(0.05, 100.0);
    timeline::retime(project, ids, sticky, |clip| {
        let (start, end, mirror) = range(clip);
        let at = |f| start + (end - start) * f;
        let from = if has_in { at(in_to) } else { start };
        let to = if has_out { at(out_from).max(from) } else { end };
        let ramp = crate::retime::Ramp {
            from,
            to,
            ease_in: if has_in { from - at(in_from) } else { 0.0 },
            ease_out: if has_out {
                at(out_to).max(from) - to
            } else {
                0.0
            },
            start_speed: if has_in { start_speed } else { speed },
            end_speed: if has_out { end_speed } else { speed },
            middle_speed: speed,
        };
        clip.effects.speed = speed;
        clip.effects.ramp = if let Some(length) = mirror {
            ramp.mirrored(length).value()
        } else {
            ramp.value()
        };
    })
}

#[cfg(test)]
mod inspector_tests {
    use super::*;

    #[test]
    fn group_transform_changes_only_unlocked_pictures() {
        let mut project = Project::new("Transform".into());
        project
            .sequence
            .tracks
            .iter_mut()
            .find(|track| track.name == "V2")
            .unwrap()
            .locked = true;
        project.timeline = vec![
            Clip::synthetic(ClipKind::Solid, 0, clock::ticks(2.0), "V1".into()),
            Clip::synthetic(
                ClipKind::Solid,
                clock::ticks(2.0),
                clock::ticks(3.0),
                "V1".into(),
            ),
            Clip::synthetic(ClipKind::Solid, 0, clock::ticks(2.0), "V2".into()),
            Clip::synthetic(ClipKind::Audio, 0, clock::ticks(2.0), "A1".into()),
        ];
        project.timeline[1].effects.transform = json!({"scale":2,"y":0.12,
            "zoom":{"mode":"in","p1":0.0,"p2":0.5,"p3":3.0,"p4":3.0}});
        let targets = project
            .timeline
            .iter()
            .map(|clip| clip.id)
            .collect::<Vec<_>>();
        edit(
            "set_transform",
            &json!({"clip_ids":targets,"scale":1.5,"x":0.2}),
            &mut project,
        )
        .unwrap();
        assert_eq!(project.timeline[0].effects.transform["scale"], 1.5);
        let second = &project.timeline[1].effects.transform;
        assert_eq!(second["x"], 0.2);
        assert_eq!(second["y"], 0.12);
        assert_eq!(second["zoom"]["p2"], 0.5);
        assert!(project.timeline[2].effects.transform.is_null());
        assert!(project.timeline[3].effects.transform.is_null());
    }

    #[test]
    fn group_fade_fits_each_clip_and_honors_legacy_fade_in() {
        let mut project = Project::new("Fades".into());
        project.timeline = vec![
            Clip::synthetic(ClipKind::Audio, 0, clock::ticks(0.5), "A1".into()),
            Clip::synthetic(
                ClipKind::Audio,
                clock::ticks(0.5),
                clock::ticks(1.0),
                "A1".into(),
            ),
        ];
        project.timeline[0].fade_in = true;
        let targets = project
            .timeline
            .iter()
            .map(|clip| clip.id)
            .collect::<Vec<_>>();
        edit(
            "set_audio",
            &json!({"clip_ids":targets,"fade_out":0.4,"fade_out_curve":"smooth"}),
            &mut project,
        )
        .unwrap();
        assert_eq!(
            project.timeline[0].effects.fade_out_frames,
            clock::ticks(0.5) - clock::ticks(1.0 / 3.0)
        );
        assert_eq!(
            project.timeline[1].effects.fade_out_frames,
            clock::ticks(0.4)
        );
        assert!(
            project
                .timeline
                .iter()
                .all(|clip| clip.effects.fade_out_curve == crate::editor::FadeCurve::Smooth)
        );
    }
}
