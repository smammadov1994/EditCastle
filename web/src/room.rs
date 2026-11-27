use crate::clock;
use crate::{
    captions::{CaptionTrack, Fix},
    editor::{self, Clip, ClipKind, Marker, Project, Source, Store, Track},
    media,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

fn number(value: &Value, key: &str, default: f64) -> f64 {
    value[key].as_f64().unwrap_or(default)
}
fn frames(seconds: f64) -> u32 {
    clock::ticks(seconds)
}
fn seconds(ticks: u32) -> f64 {
    clock::seconds(ticks)
}
fn uuid(value: &Value) -> Option<Uuid> {
    value.as_str()?.parse().ok()
}
fn text<'a>(value: &'a Value, key: &str, default: &'a str) -> &'a str {
    value[key].as_str().unwrap_or(default)
}
fn color(value: &Value) -> String {
    let byte = |key| (number(value, key, 1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", byte("r"), byte("g"), byte("b"))
}

pub async fn migrate(input: &Path, data: &Path) -> Result<Uuid> {
    let file = tokio::fs::canonicalize(input)
        .await
        .context("Open the native .room project.")?;
    let stored: Value = serde_json::from_slice(&tokio::fs::read(&file).await?)
        .context("Read the .room project file.")?;
    let store = Store::open(data)?;
    import_project(
        &store,
        &stored,
        file.file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Imported Editroom project"),
        RoomFiles::Host(file.parent().expect("canonical project path")),
    )
    .await
}

pub enum RoomFiles<'a> {
    Host(&'a Path),
    Archive {
        root: &'a Path,
        document: &'a Path,
    },
    Selected {
        media: &'a HashMap<Uuid, PathBuf>,
        looks: &'a HashMap<String, PathBuf>,
    },
}

impl RoomFiles<'_> {
    fn relative(&self, name: &str) -> Option<PathBuf> {
        match self {
            Self::Host(root) => Some(root.join(name)),
            Self::Archive { root, document } => {
                let mut relative = document.parent()?.strip_prefix(root).ok()?.to_path_buf();
                for component in Path::new(name).components() {
                    match component {
                        Component::Normal(part) => relative.push(part),
                        Component::CurDir => {}
                        Component::ParentDir if relative.pop() => {}
                        _ => return None,
                    }
                }
                Some(root.join(relative))
            }
            Self::Selected { .. } => None,
        }
    }

    fn asset(&self, id: Uuid, asset: &Value) -> Option<PathBuf> {
        if let Self::Selected { media, .. } = self {
            return media.get(&id).cloned();
        }
        let relative = asset["relativePath"]
            .as_str()
            .and_then(|path| self.relative(path));
        let original = asset["path"].as_str();
        let playable = asset["playablePath"].as_str();
        let resolve = |path: &str| match self {
            Self::Host(_) => Some(PathBuf::from(path)),
            _ => self.relative(path),
        };
        [
            relative,
            original.and_then(resolve),
            playable.and_then(resolve),
        ]
        .into_iter()
        .flatten()
        .find(|path| path.is_file())
    }

    fn look(&self, name: &str) -> Option<PathBuf> {
        match self {
            Self::Selected { looks, .. } => looks.get(name).cloned(),
            _ => self.relative(name),
        }
    }

    #[cfg(target_os = "macos")]
    fn reuse_transcript(&self, store: &Store, source: &Source, asset: &Value, path: &Path) {
        if !matches!(self, Self::Host(_)) || crate::speech::indexed(store, source) {
            return;
        }
        let resolved = path.canonicalize().ok();
        let original = asset["path"].as_str().map(Path::new).filter(|saved| {
            resolved
                .as_ref()
                .is_some_and(|current| saved.canonicalize().is_ok_and(|saved| &saved == current))
        });
        // Native can transcribe a playable proxy. Use it only if its original
        // is the file this import located, rather than a replacement.
        let playable = original.and(asset["playablePath"].as_str().map(Path::new));
        let _ = [playable, original, Some(path), resolved.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|candidate| crate::speech::import_native_cache(store, source, candidate));
    }
}

pub async fn import_project(
    store: &Store,
    stored: &Value,
    title: &str,
    files: RoomFiles<'_>,
) -> Result<Uuid> {
    let raw = stored.get("project").unwrap_or(stored);
    ensure!(
        raw["clips"].is_array() && raw["assets"].is_array(),
        "This file is missing an Editroom timeline."
    );
    ensure!(
        raw["clips"].as_array().unwrap().len() <= 1001
            && raw["assets"].as_array().unwrap().len() <= 1000,
        "The project exceeds the studio's clip or media limit."
    );
    let mut project = Project::new(title.chars().take(100).collect());
    project.export_name = raw["exportName"]
        .as_str()
        .and_then(editor::export_file_stem);
    let result = async {
        decode(store, raw, &stored["editroomWeb"], &files, &mut project).await?;
        project.validate_timeline(&project.timeline, project.soundtrack)?;
        store.update(|db| {
            db.projects.push(project.clone());
            Ok(())
        })?;
        Ok(project.id)
    }
    .await;
    if result.is_err() {
        for source in &project.sources {
            media::discard_import(store, source).await;
        }
        for lut in &project.sequence.luts {
            let _ = tokio::fs::remove_file(store.lut(lut.id)).await;
        }
    }
    result
}

async fn decode(
    store: &Store,
    raw: &Value,
    web: &Value,
    files: &RoomFiles<'_>,
    project: &mut Project,
) -> Result<()> {
    project.sequence.width = number(raw, "width", 1920.0).clamp(128.0, 7680.0) as u32;
    project.sequence.height = number(raw, "height", 1080.0).clamp(128.0, 7680.0) as u32;
    project.sequence.fps = number(raw, "fps", 30.0);
    project.sequence.settings_locked = raw["settingsLocked"].as_bool();
    project.sequence.in_point = raw["inPoint"].as_f64().map(frames);
    project.sequence.out_point = raw["outPoint"].as_f64().map(frames);
    project.sequence.playhead = web["playhead"].as_f64().map(frames).unwrap_or(0);
    if let Some(model) = web.get("renderModel")
        && let Ok(model) = serde_json::from_value(model.clone())
    {
        project.sequence.render_model = model;
    }
    project.sequence.tracks.clear();
    let mut tracks = HashMap::new();
    for (key, prefix) in [("videoTracks", 'V'), ("audioTracks", 'A')] {
        for (index, value) in raw[key].as_array().into_iter().flatten().enumerate() {
            let name = format!("{prefix}{}", index + 1);
            let mut track = Track::new(name.clone());
            if let Some(id) = uuid(&value["id"]) {
                tracks.insert(id, name);
                track.id = id;
            }
            track.locked = value["locked"] == true;
            track.hidden = value["hidden"] == true;
            track.muted = value["muted"] == true;
            track.solo = value["solo"] == true;
            track.gain_db = number(value, "gainDB", 0.0);
            if prefix == 'A' {
                track.duck_under = uuid(&value["duckUnder"]);
                track.duck_db = number(value, "duckDB", -12.0).clamp(-60.0, 0.0);
            }
            project.sequence.tracks.push(track);
        }
    }
    for name in ["V1", "A1"] {
        if !project
            .sequence
            .tracks
            .iter()
            .any(|track| track.name == name)
        {
            project.sequence.tracks.push(Track::new(name.into()));
        }
    }
    let mut library_roots = vec![];
    for folder in raw["folders"].as_array().into_iter().flatten() {
        let Some(native) = folder["path"].as_str().map(PathBuf::from) else {
            continue;
        };
        let relative = folder["relativePath"].as_str().map(PathBuf::from);
        let resolved = folder["relativePath"]
            .as_str()
            .and_then(|path| files.relative(path));
        let name = native
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Media")
            .to_owned();
        let folder = project.library.upload_folder(name);
        library_roots.push((native, relative, resolved, folder.id));
    }
    library_roots.sort_by_key(|(path, _, _, _)| std::cmp::Reverse(path.components().count()));
    let mut excluded = HashSet::new();
    for hidden in raw["hiddenMedia"].as_array().into_iter().flatten() {
        let Some(path) = hidden.as_str().map(Path::new) else {
            continue;
        };
        let place = library_roots
            .iter()
            .find_map(|(native, relative, resolved, folder)| {
                let relative = std::iter::once(native)
                    .chain(relative.iter())
                    .chain(resolved.iter())
                    .find_map(|root| path.strip_prefix(root).ok())?
                    .to_str()?;
                project.library.upload_path(*folder, relative).ok()
            });
        if let Some(place) = place
            && excluded.insert(place.clone())
        {
            project.library.hidden.push(place);
        }
    }
    let mut assets = HashMap::new();
    let audio_assets = raw["clips"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|clip| clip["kind"] == "audio")
        .filter_map(|clip| uuid(&clip["assetID"]))
        .collect::<HashSet<_>>();
    let captioned_assets = raw["clips"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|clip| clip["captioned"] == true)
        .filter_map(|clip| uuid(&clip["assetID"]))
        .collect::<std::collections::HashSet<_>>();
    for asset in raw["assets"].as_array().unwrap() {
        let Some(id) = uuid(&asset["id"]) else {
            continue;
        };
        let name = text(asset, "name", "Media");
        ensure!(
            !assets.contains_key(&id),
            "Duplicate media ID in the native project."
        );
        let path = files.asset(id, asset);
        let mut source = if let Some(path) = &path {
            media::import(store, path, name.chars().take(100).collect())
                .await
                .with_context(|| format!("Could not transfer {name}."))?
        } else {
            let kind = text(
                asset,
                "kind",
                if audio_assets.contains(&id) {
                    "audio"
                } else {
                    "video"
                },
            );
            let source = Source {
                id: Uuid::new_v4(),
                storage: None,
                origin_stamp: None,
                device_digest: None,
                duration: frames(number(asset, "duration", 0.0)).max(1),
                title: name.chars().take(100).collect(),
                notes: String::new(),
                video: kind != "audio",
                image: kind == "image",
                audio: kind == "audio" || asset["hasAudio"] == true || audio_assets.contains(&id),
            };
            let filename = web["missingFiles"][id.to_string()]
                .as_str()
                .or_else(|| asset["relativePath"].as_str())
                .or_else(|| asset["path"].as_str())
                .unwrap_or(name)
                .rsplit(['/', '\\'])
                .find(|part| !part.is_empty() && !part.chars().any(char::is_control))
                .unwrap_or("Media");
            project
                .library
                .missing
                .insert(source.id, filename.chars().take(255).collect());
            source
        };
        source.notes = web["sourceNotes"][id.to_string()]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(3000)
            .collect();
        if source.audio
            && let Some(value) = web["transcripts"].get(id.to_string())
            && let Ok(transcript) =
                serde_json::from_value::<crate::speech::Transcript>(value.clone())
            && transcript
                .lines
                .iter()
                .chain(&transcript.words)
                .all(|line| line.start <= line.end && line.end <= clock::MAX_TIME)
        {
            let path = store
                .media(source.storage_id(), source.video)
                .with_extension("timed-words.json");
            let _ = editor::private_write(&path, &serde_json::to_vec(&transcript)?);
        }
        #[cfg(target_os = "macos")]
        if let Some(path) = &path
            && source.audio
        {
            files.reuse_transcript(store, &source, asset, path);
        }
        if path.is_some()
            && source.audio
            && captioned_assets.contains(&id)
            && crate::speech::available()
        {
            let _ = crate::speech::transcribe(store, &source).await;
        }
        let written = asset["path"].as_str().map(Path::new);
        let relative_source = asset["relativePath"].as_str().map(Path::new);
        for (native, relative, resolved, folder) in &library_roots {
            let private_path = written
                .and_then(|path| path.strip_prefix(native).ok())
                .or_else(|| path.as_deref()?.strip_prefix(native).ok())
                .or_else(|| relative_source?.strip_prefix(relative.as_ref()?).ok())
                .or_else(|| path.as_deref()?.strip_prefix(resolved.as_ref()?).ok());
            if let Some(path) = private_path.and_then(Path::to_str)
                && let Ok(place) = project.library.upload_path(*folder, path)
            {
                project.library.add_upload(place, source.id);
                break;
            }
        }
        assets.insert(id, source.id);
        project.sources.push(source);
    }
    let mut imported_luts = HashMap::new();
    let required = raw["clips"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|clip| clip["grade"]["look"].as_str())
        .filter(|path| path.to_lowercase().ends_with(".cube"))
        .collect::<HashSet<_>>();
    let names = web["looks"].as_object();
    let mut look_files = required.clone();
    look_files.extend(
        names
            .into_iter()
            .flat_map(|map| map.keys().map(String::as_str)),
    );
    for look in look_files {
        let bytes = async {
            let file = files.look(look)?;
            let metadata = tokio::fs::metadata(&file).await.ok()?;
            if !metadata.is_file() || metadata.len() > 14 * 1024 * 1024 {
                return None;
            }
            let bytes = tokio::fs::read_to_string(&file).await.ok()?;
            crate::grade::Cube::parse(&bytes).ok()?;
            Some(bytes)
        }
        .await;
        let Some(bytes) = bytes else {
            ensure!(
                !required.contains(look) || matches!(files, RoomFiles::Host(_)),
                "A custom color look is missing or invalid."
            );
            continue;
        };
        let id = Uuid::new_v4();
        editor::private_write(&store.lut(id), bytes.as_bytes())?;
        let name = names
            .and_then(|names| names.get(look))
            .and_then(Value::as_str)
            .or_else(|| Path::new(look).file_stem().and_then(|part| part.to_str()))
            .unwrap_or("Custom look")
            .chars()
            .take(100)
            .collect();
        project.sequence.luts.push(editor::LookupTable { id, name });
        imported_luts.insert(look, id);
    }
    for value in raw["clips"].as_array().unwrap() {
        let kind = match text(value, "kind", "video") {
            "video" => ClipKind::Video,
            "audio" => ClipKind::Audio,
            "image" => ClipKind::Image,
            "text" => ClipKind::Text,
            "solid" => ClipKind::Solid,
            _ => continue,
        };
        let start = frames(number(value, "start", 0.0));
        let length = number(value, "duration", 0.0);
        if length <= 0.0 {
            continue;
        }
        let track = uuid(&value["trackID"])
            .and_then(|id| tracks.get(&id).cloned())
            .unwrap_or_else(|| {
                if kind == ClipKind::Audio {
                    "A1".into()
                } else {
                    "V1".into()
                }
            });
        let mut clip = Clip::synthetic(kind, start, frames(length).max(1), track);
        if let Some(id) = uuid(&value["id"]) {
            if web["projectedClips"]
                .as_array()
                .is_some_and(|clips| clips.iter().any(|clip| uuid(clip) == Some(id)))
            {
                continue;
            }
            clip.id = id;
        }
        clip.link = uuid(&value["linkID"]);
        clip.audio_detached = kind == ClipKind::Video
            && !web["embeddedAudio"]
                .as_array()
                .is_some_and(|clips| clips.iter().any(|id| uuid(id) == Some(clip.id)));
        clip.captioned = value["captioned"] == true;
        let fx = &mut clip.effects;
        fx.speed = number(value, "speed", 1.0).clamp(0.05, 100.0);
        fx.gain_db = number(value, "gainDB", 0.0).clamp(-60.0, 12.0);
        if let Some(gain) = web["mutedClips"][clip.id.to_string()].as_f64() {
            clip.mute = true;
            fx.gain_db = gain.clamp(-60.0, 12.0);
        }
        fx.fade_in_frames = frames(number(value, "fadeIn", 0.0));
        fx.fade_out_frames = frames(number(value, "fadeOut", 0.0));
        fx.fade_in_curve = serde_json::from_value(value["fadeInCurve"].clone()).unwrap_or_default();
        fx.fade_out_curve =
            serde_json::from_value(value["fadeOutCurve"].clone()).unwrap_or_default();
        fx.freeze = value["freeze"] == true || kind == ClipKind::Image;
        fx.reverse = value["reversed"] == true;
        if kind == ClipKind::Text {
            let style = &value["text"];
            fx.title = text(style, "string", "Text").to_owned();
            fx.color = color(&style["color"]);
            fx.transform = json!({"font":text(style,"fontName","System"),"size":number(style,"size",110.0),"bold":style["bold"].as_bool().unwrap_or(true),
                "x":number(style,"x",0.5),"y":number(style,"y",0.5),"color":fx.color});
        } else if kind == ClipKind::Solid {
            fx.color = color(&value["color"]);
        }
        if kind != ClipKind::Text && value["transform"].is_object() {
            fx.transform = value["transform"].clone();
        }
        if let Some(transition) = value["transitionIn"]["kind"].as_str() {
            fx.transition = Some(transition.into());
            fx.transition_frames = frames(number(&value["transitionIn"], "duration", 0.5));
        }
        if let Some(grade) = value["grade"].as_object() {
            fx.grade = json!(grade);
            if let Some(strength) = grade.get("lookIntensity") {
                fx.grade["look_intensity"] = strength.clone();
            }
            if let Some(look) = grade
                .get("look")
                .and_then(Value::as_str)
                .filter(|look| look.to_lowercase().ends_with(".cube"))
            {
                if let Some(id) = imported_luts.get(look) {
                    fx.grade["look"] = json!(format!("lut:{id}"));
                }
            }
        }
        if let Some(ramp) = value["ramp"].as_object() {
            let ramp = json!(ramp);
            let from = number(&ramp, "inTo", 0.0);
            let to = number(&ramp, "outFrom", from);
            fx.ramp = json!({"from":from,"to":to,"ease_in":from-number(&ramp,"inFrom",from),"ease_out":number(&ramp,"outTo",to)-to,
                "start_speed":number(&ramp,"startSpeed",fx.speed),"end_speed":number(&ramp,"endSpeed",fx.speed),"speed":fx.speed});
        }
        if matches!(kind, ClipKind::Video | ClipKind::Audio | ClipKind::Image) {
            clip.source = uuid(&value["assetID"])
                .and_then(|id| assets.get(&id).copied())
                .context("The native cut references a missing asset.")?;
            let source = project.source(clip.source).unwrap();
            let from = frames(number(value, "sourceStart", 0.0)).min(source.duration - 1);
            clip.start = from;
            clip.end = if kind == ClipKind::Image || fx.freeze {
                from + frames(length).max(1)
            } else {
                from.saturating_add(frames(length * fx.speed).max(1))
                    .min(source.duration)
            };
            if let Some(ramp) = crate::retime::Ramp::read(&fx.ramp) {
                clip.end =
                    frames(ramp.source(seconds(from), length)).clamp(from + 1, source.duration);
            }
            if fx.reverse && !fx.freeze {
                (clip.start, clip.end) = (source.duration - clip.end, source.duration - clip.start);
                if let Some(ramp) = crate::retime::Ramp::read(&fx.ramp) {
                    fx.ramp = ramp.mirrored(seconds(source.duration)).value();
                }
            }
        }
        project.timeline.push(clip);
    }
    project.soundtrack = uuid(&web["soundtrack"]).and_then(|id| assets.get(&id).copied());
    if let Some(generated) = web["projectedTracks"].as_array() {
        project.sequence.tracks.retain(|track| {
            !generated.iter().any(|id| uuid(id) == Some(track.id))
                || project
                    .timeline
                    .iter()
                    .any(|clip| clip.track() == track.name)
        });
    }
    project.sequence.markers = raw["markers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|marker| Marker {
            id: uuid(&marker["id"]).unwrap_or_else(Uuid::new_v4),
            frame: frames(number(marker, "time", 0.0)),
            name: text(marker, "name", "Marker").into(),
            color: text(marker, "tint", "purple").into(),
        })
        .collect();
    if raw["captions"].is_object() {
        let style = &raw["captions"]["style"];
        let mut track = CaptionTrack {
            hidden: raw["captions"]["hidden"] == true,
            size: number(style, "size", 64.0),
            position: number(style, "y", 0.78),
            highlight: style["highlight"].as_bool().unwrap_or(true),
            outline: style["outline"].as_bool().unwrap_or(true),
            ..Default::default()
        };
        for (key, field) in [
            ("textColor", &mut track.text_color),
            ("highlightColor", &mut track.highlight_color),
            ("outlineColor", &mut track.outline_color),
        ] {
            if style[key].is_object() {
                *field = color(&style[key]);
                let alpha = (number(&style[key], "a", 1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
                if alpha < 255 {
                    field.push_str(&format!("{alpha:02X}"));
                }
            }
        }
        let corrections = raw["captionFixes"]
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(id, fixes)| (Value::String(id.clone()), fixes))
                    .collect::<Vec<_>>()
            })
            .or_else(|| {
                raw["captionFixes"].as_array().map(|pairs| {
                    pairs
                        .chunks_exact(2)
                        .map(|pair| (pair[0].clone(), &pair[1]))
                        .collect()
                })
            })
            .unwrap_or_default();
        for (id, list) in corrections {
            let Some(source) = uuid(&id).and_then(|id| assets.get(&id)) else {
                continue;
            };
            let mut fixes = list
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|fix| {
                    Some(Fix {
                        start: frames(fix["start"].as_f64()?),
                        text: fix["text"].as_str()?.into(),
                        merged: fix["merged"] == true,
                    })
                })
                .collect::<Vec<_>>();
            fixes.sort_by_key(|fix| fix.start);
            track.fixes.insert(*source, fixes);
        }
        project.sequence.captions = Some(track);
    }
    (project.messages, project.prompt_history) = crate::studio_history::import(raw, web, &assets);
    Ok(())
}

pub const MAX_BUNDLE: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_DOCUMENT: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 2048;

pub fn unpack(archive: &Path, root: &Path) -> Result<PathBuf> {
    let mut zip = zip::ZipArchive::new(BufReader::new(File::open(archive)?))
        .context("Open the project ZIP archive.")?;
    ensure!(
        zip.len() <= MAX_ENTRIES,
        "The project archive has too many files."
    );
    let mut size = 0_u64;
    let mut document = None;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let path = entry
            .enclosed_name()
            .context("An archive path is outside the project.")?;
        ensure!(
            !entry.is_symlink()
                && path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "The project archive contains an unsupported path."
        );
        if path.starts_with("__MACOSX") || entry.is_dir() {
            continue;
        }
        ensure!(
            entry.is_file(),
            "The project archive contains a special file."
        );
        size = size.saturating_add(entry.size());
        ensure!(size <= MAX_BUNDLE, "The unpacked project exceeds 2 GB.");
        let output = root.join(&path);
        if path.extension().is_some_and(|ext| {
            ext.eq_ignore_ascii_case("room") || ext.eq_ignore_ascii_case("cutplay")
        }) {
            ensure!(
                document.is_none(),
                "Choose an archive containing one .room or .cutplay project."
            );
            ensure!(
                entry.size() <= MAX_DOCUMENT,
                "The project document is too large."
            );
            document = Some(output.clone());
        }
        fs::create_dir_all(output.parent().expect("archive entry parent"))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .context("The project contains two files at the same path.")?;
        let expected = entry.size();
        let mut limited = (&mut entry).take(expected + 1);
        ensure!(
            io::copy(&mut limited, &mut file)? == expected,
            "An archive file has the wrong length."
        );
    }
    document.context("The ZIP does not contain a .room or .cutplay project.")
}

fn leaf(name: &str, fallback: &str) -> String {
    let safe = name
        .chars()
        .filter(|ch| !ch.is_control())
        .map(|ch| {
            if matches!(
                ch,
                '/' | '\\' | ':' | '<' | '>' | '\u{22}' | '?' | '*' | '|'
            ) {
                '_'
            } else {
                ch
            }
        })
        .take(100)
        .collect::<String>();
    let safe = safe.trim_matches(|ch: char| ch.is_whitespace() || ch == '.');
    if safe.is_empty() {
        fallback.to_owned()
    } else {
        safe.to_owned()
    }
}

pub fn source_filename(title: &str, extension: &str) -> String {
    let name = leaf(title, "Editroom source");
    if Path::new(&name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
    {
        name
    } else {
        format!("{name}.{extension}")
    }
}

pub fn archive_name(project: &Project) -> String {
    leaf(&project.title, "Editroom project")
}

fn rgba(hex: &str, fallback: &str) -> Value {
    let hex = hex.trim_start_matches('#');
    let color = if matches!(hex.len(), 6 | 8) && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        hex
    } else {
        fallback
    };
    let byte = |index| f64::from(u8::from_str_radix(&color[index..index + 2], 16).unwrap()) / 255.0;
    json!({"r":byte(0),"g":byte(2),"b":byte(4),"a":if color.len()==8 {byte(6)} else {1.0}})
}

fn extension(data: &Value, source: &Source) -> Option<&'static str> {
    let format = data["format"]["format_name"].as_str()?;
    if source.image {
        return match format {
            "png_pipe" => Some("png"),
            "jpeg_pipe" | "image2" => Some("jpg"),
            "webp_pipe" => Some("webp"),
            _ => None,
        };
    }
    let first = format.split(',').next()?;
    Some(match first {
        "mov" if !source.video => "m4a",
        "mov"
            if data["format"]["tags"]["major_brand"]
                .as_str()
                .is_some_and(|brand| brand.trim() == "qt") =>
        {
            "mov"
        }
        "mov" => "mp4",
        "matroska" => "mkv",
        "mpegts" => "ts",
        "mpeg" => "mpg",
        "wav" => "wav",
        "aiff" => "aiff",
        "mp3" => "mp3",
        "aac" => "aac",
        "flac" => "flac",
        "ogg" => "ogg",
        "avi" => "avi",
        "mxf" => "mxf",
        "flv" => "flv",
        "asf" => "wmv",
        "caf" => "caf",
        "hevc" => "hevc",
        "h264" => "h264",
        "ac3" => "ac3",
        _ => return None,
    })
}

pub async fn portable_media(
    store: &Store,
    source: &Source,
    work: &Path,
) -> Result<(PathBuf, String, Value)> {
    let original = store.original(source.storage_id());
    if original.is_file()
        && let Ok(data) = media::details(&original).await
    {
        let ext = extension(&data, source).unwrap_or("media");
        return Ok((original, ext.into(), data));
    }
    let proxy = store.media(source.storage_id(), source.video);
    if source.image {
        let still = work.join(format!("{}.jpg", source.id));
        media::extract(&proxy, 0, &still, false).await?;
        let data = media::details(&still).await?;
        return Ok((still, "jpg".into(), data));
    }
    let data = media::details(&proxy).await?;
    Ok((proxy, if source.video { "mp4" } else { "m4a" }.into(), data))
}

fn native_track(track: &Track) -> Value {
    json!({
        "id":track.id,"kind":if track.name.starts_with('A') {"audio"} else {"video"},
        "hidden":track.hidden,"locked":track.locked,"muted":track.muted,"solo":track.solo,
        "gainDB":track.gain_db,"duckUnder":track.duck_under,"duckDB":track.duck_db,
    })
}

fn native_clip(
    clip: &Clip,
    project: &Project,
    starts: &HashMap<Uuid, u32>,
    look_paths: &HashMap<Uuid, String>,
) -> Result<Value> {
    let fx = &clip.effects;
    let track = project
        .sequence
        .tracks
        .iter()
        .find(|track| track.name == clip.track())
        .context("The cut references an unknown track.")?;
    let asset = project.source(clip.source);
    let backwards = fx.reverse && !fx.freeze;
    let length = asset.map_or(0, |asset| asset.duration);
    let (from, _) = clip.playback_source_bounds(length);
    let ramp = crate::retime::Ramp::read(&fx.ramp).map(|ramp| {
        let ramp = if backwards { ramp.mirrored(seconds(length)) } else { ramp };
        json!({"inFrom":ramp.from-ramp.ease_in,"inTo":ramp.from,"outFrom":ramp.to,"outTo":ramp.to+ramp.ease_out,
            "startSpeed":ramp.start_speed,"endSpeed":ramp.end_speed})
    });
    let text_style = if clip.kind == ClipKind::Text {
        json!({"string":fx.title,"fontName":text(&fx.transform,"font",""),"size":number(&fx.transform,"size",110.0),
            "bold":fx.transform["bold"].as_bool().unwrap_or(true),"x":number(&fx.transform,"x",0.5),"y":number(&fx.transform,"y",0.5),
            "color":rgba(fx.title_color(),"FFFFFF")})
    } else {
        json!({"string":"Text","fontName":"","size":110.0,"bold":true,"x":0.5,"y":0.5,"color":rgba("FFFFFF","FFFFFF")})
    };
    let transform = (clip.kind != ClipKind::Text && fx.transform.is_object()).then(|| json!({
        "x":number(&fx.transform,"x",0.0),"y":number(&fx.transform,"y",0.0),"scale":number(&fx.transform,"scale",1.0),"zoom":fx.transform["zoom"]
    }));
    let grade = fx.grade.as_object().map(|_| {
        let look = fx.grade["look"].as_str().and_then(|look| {
            if let Some(id) = look.strip_prefix("lut:").and_then(|id| id.parse::<Uuid>().ok()) {
                look_paths.get(&id).cloned()
            } else {
                Some(look.to_owned())
            }
        });
        let wheel = |name| json!({"x":number(&fx.grade[name],"x",0.0),"y":number(&fx.grade[name],"y",0.0)});
        json!({"look":look,"lookIntensity":number(&fx.grade,"look_intensity",1.0),"exposure":number(&fx.grade,"exposure",0.0),
            "contrast":number(&fx.grade,"contrast",0.0),"saturation":number(&fx.grade,"saturation",0.0),"temperature":number(&fx.grade,"temperature",0.0),
            "tint":number(&fx.grade,"tint",0.0),"shadows":wheel("shadows"),"midtones":wheel("midtones"),"highlights":wheel("highlights")})
    });
    Ok(json!({
        "id":clip.id,"trackID":track.id,"kind":clip.kind,"assetID":asset.map(|asset|asset.id),
        "start":seconds(starts.get(&clip.id).copied().unwrap_or(0)),"duration":seconds(clip.duration()),
        "sourceStart":seconds(from),"speed":fx.speed,"fadeIn":seconds(if clip.fade_in {fx.fade_in_frames.max(clock::ticks(1.0/3.0))} else {fx.fade_in_frames}),
        "fadeOut":seconds(fx.fade_out_frames),"fadeInCurve":fx.fade_in_curve,"fadeOutCurve":fx.fade_out_curve,
        "gainDB":if clip.mute {-60.0} else {fx.gain_db},"linkID":clip.link,"freeze":fx.freeze && clip.kind==ClipKind::Video,
        "color":rgba(&fx.color,"292933"),"text":text_style,"grade":grade,"transform":transform,
        "transitionIn":fx.transition.as_ref().map(|kind| json!({"kind":kind,"duration":seconds(fx.transition_frames)})),
        "reversed":backwards,"ramp":ramp,"captioned":clip.captioned,
    }))
}

pub async fn bundle(store: &Store, project: &Project, dir: &Path) -> Result<PathBuf> {
    let name = archive_name(project);
    let mut attachments = Vec::new();
    let mut assets = Vec::new();
    let mut folders = Vec::new();
    let mut folder_paths = HashMap::new();
    let mut used_paths = HashSet::new();
    for folder in &project.library.folders {
        let mut path = format!("Media/{}", leaf(&folder.name, "Media"));
        if !used_paths.insert(path.to_lowercase()) {
            path.push_str(&format!("-{}", &folder.id.simple().to_string()[..8]));
            used_paths.insert(path.to_lowercase());
        }
        folders.push(json!({"id":folder.id,"path":path,"relativePath":path}));
        folder_paths.insert(folder.id, path);
    }
    let hidden_media = project
        .library
        .excluded_paths()
        .filter(|(_, path)| !path.is_empty())
        .filter_map(|(folder, path)| {
            folder_paths
                .get(&folder.id)
                .map(|root| format!("{root}/{path}"))
        })
        .collect::<Vec<_>>();
    for source in &project.sources {
        let missing = project.library.missing.get(&source.id);
        let (file, ext, data) = if let Some(name) = missing {
            let ext = Path::new(name)
                .extension()
                .and_then(|part| part.to_str())
                .filter(|ext| {
                    ext.len() <= 10 && ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
                .unwrap_or(if source.image {
                    "jpg"
                } else if source.video {
                    "mp4"
                } else {
                    "m4a"
                });
            (None, ext.to_owned(), Value::Null)
        } else {
            let (file, ext, data) = portable_media(store, source, dir)
                .await
                .with_context(|| format!("Package {}.", source.title))?;
            (Some(file), ext, data)
        };
        let title = missing.map_or(source.title.as_str(), String::as_str);
        let original_name = Path::new(title)
            .file_stem()
            .and_then(|part| part.to_str())
            .unwrap_or(title);
        let mut path = format!("Media/{}.{}", leaf(original_name, "Media"), ext);
        if let Some((folder, relative)) = project.library.locate(source.id)
            && let Some(root) = folder_paths.get(&folder.id)
        {
            let mut pieces = Path::new(relative)
                .iter()
                .map(|x| leaf(x.to_str().unwrap_or(""), "Media"))
                .collect::<Vec<_>>();
            if let Some(file) = pieces.last_mut() {
                let stem = Path::new(&file)
                    .file_stem()
                    .and_then(|x| x.to_str())
                    .unwrap_or(file);
                *file = format!("{stem}.{ext}");
            }
            path = format!("{root}/{}", pieces.join("/"));
        }
        if !used_paths.insert(path.to_lowercase()) {
            path = format!(
                "{}-{}.{}",
                path.trim_end_matches(&format!(".{ext}")),
                &source.id.simple().to_string()[..8],
                ext
            );
            used_paths.insert(path.to_lowercase());
        }
        let picture = source
            .video
            .then(|| media::picture_info(&data, source.image).ok())
            .flatten();
        let (width, height) = picture.map_or((0, 0), |info| (info.width, info.height));
        let fps = picture.and_then(|info| info.fps).unwrap_or(0.0);
        assets.push(json!({"id":source.id,"name":source.title,"kind":if source.image {"image"} else if source.video {"video"} else {"audio"},
            "path":path,"relativePath":path,"duration":seconds(source.duration),"width":width,"height":height,"fps":fps,"hasAudio":source.audio}));
        if let Some(file) = file {
            attachments.push((path, file));
        }
    }
    let mut look_paths = HashMap::new();
    let mut look_names = HashMap::new();
    for lut in &project.sequence.luts {
        let path = format!(
            "Looks/{}-{}.cube",
            leaf(&lut.name, "Look"),
            &lut.id.simple().to_string()[..8]
        );
        attachments.push((path.clone(), store.lut(lut.id)));
        look_names.insert(path.clone(), &lut.name);
        look_paths.insert(lut.id, path);
    }
    let mut materialized = project.clone();
    materialized.materialize_positions();
    let starts = materialized
        .timeline
        .iter()
        .filter_map(|clip| Some((clip.id, clip.position?)))
        .collect::<HashMap<_, _>>();
    let mut clips = materialized
        .timeline
        .iter()
        .map(|clip| native_clip(clip, &materialized, &starts, &look_paths))
        .collect::<Result<Vec<_>>>()?;
    let tracks = &project.sequence.tracks;
    let video_tracks = tracks
        .iter()
        .filter(|track| track.name.starts_with('V'))
        .map(native_track)
        .collect::<Vec<_>>();
    let mut audio_tracks = tracks
        .iter()
        .filter(|track| track.name.starts_with('A'))
        .map(native_track)
        .collect::<Vec<_>>();
    let mut projected_clips = Vec::new();
    let mut projected_tracks = Vec::new();
    let mut embedded = Vec::new();
    // Native Editroom keeps audible media in audio lanes. Mirror the web soundtrack and old
    // embedded audio in separate lanes, while retaining their meaning on reimport.
    for clip in &project.timeline {
        if clip.kind != ClipKind::Video || clip.audio_detached || clip.link.is_some() {
            continue;
        }
        embedded.push(clip.id);
        if clip.mute
            || clip.effects.freeze
            || !project
                .source(clip.source)
                .is_some_and(|source| source.audio)
        {
            continue;
        }
        let track_id = Uuid::new_v4();
        let id = Uuid::new_v4();
        let mut audio = clips
            .iter()
            .find(|value| uuid(&value["id"]) == Some(clip.id))
            .expect("project clip")
            .clone();
        audio["id"] = json!(id);
        audio["trackID"] = json!(track_id);
        audio["kind"] = json!("audio");
        audio["transitionIn"] = Value::Null;
        audio["grade"] = Value::Null;
        audio["transform"] = Value::Null;
        clips.push(audio);
        let mut track = Track::new("A".into());
        track.id = track_id;
        audio_tracks.push(native_track(&track));
        projected_clips.push(id);
        projected_tracks.push(track_id);
    }
    if let Some(source) = project.soundtrack.and_then(|id| project.source(id)) {
        let mut track = Track::new(format!("A{}", audio_tracks.len() + 1));
        track.gain_db = -4.4;
        track.solo = tracks
            .iter()
            .any(|track| track.name.starts_with('A') && track.solo);
        let length = project.duration().min(source.duration);
        if length > 0 {
            materialized.sequence.tracks.push(track.clone());
            let mut clip = Clip::synthetic(ClipKind::Audio, 0, length, track.name.clone());
            clip.source = source.id;
            clips.push(native_clip(&clip, &materialized, &starts, &look_paths)?);
            audio_tracks.push(native_track(&track));
            projected_clips.push(clip.id);
            projected_tracks.push(track.id);
        }
    }
    let caption_data = project.sequence.captions.as_ref().map(|track| json!({"hidden":track.hidden,"style":{
        "size":track.size,"y":track.position,"highlight":track.highlight,"outline":track.outline,
        "textColor":rgba(&track.text_color,"FFFFFF"),"highlightColor":rgba(&track.highlight_color,"FFD640"),"outlineColor":rgba(&track.outline_color,"000000")
    }}));
    let fixes = project.sequence.captions.as_ref().map(|track| track.fixes.iter().flat_map(|(id,fixes)| [
        json!(id),json!(fixes.iter().map(|fix|json!({"start":seconds(fix.start),"text":fix.text,"merged":fix.merged})).collect::<Vec<_>>()),
    ]).collect::<Vec<_>>());
    let notes = project
        .sources
        .iter()
        .filter(|source| !source.notes.is_empty())
        .map(|source| (source.id, source.notes.as_str()))
        .collect::<HashMap<_, _>>();
    let transcripts = project
        .sources
        .iter()
        .filter(|source| source.audio)
        .filter_map(|source| {
            crate::speech::cached(store, source).map(|transcript| (source.id, transcript))
        })
        .collect::<HashMap<_, _>>();
    let muted = project
        .timeline
        .iter()
        .filter(|clip| clip.mute)
        .map(|clip| (clip.id, clip.effects.gain_db))
        .collect::<HashMap<_, _>>();
    let markers = project
        .sequence
        .markers
        .iter()
        .map(|marker| {
            json!({
                "id":marker.id,"time":seconds(marker.frame),"name":marker.name,"tint":marker.color
            })
        })
        .collect::<Vec<_>>();
    let asset_ids = project.sources.iter().map(|asset| asset.id).collect();
    let history = crate::studio_history::native_turns(&project.messages, &asset_ids);
    let document = json!({
        "version":1,"project":{
            "width":project.sequence.width,"height":project.sequence.height,"fps":project.sequence.fps,
            "settingsLocked":!project.sequence.offers_source_format(),"videoTracks":video_tracks,"audioTracks":audio_tracks,"clips":clips,"assets":assets,"folders":folders,"hiddenMedia":hidden_media,
            "markers":markers,"inPoint":project.sequence.in_point.map(seconds),"outPoint":project.sequence.out_point.map(seconds),
            "exportName":project.export_name,
            "videoStudioTurns":history,"magicHistory":project.prompt_history,
            "captions":caption_data,"captionFixes":fixes
        },
        "editroomWeb":{"playhead":seconds(project.sequence.playhead),"renderModel":project.sequence.render_model,"transcripts":transcripts,
            "sourceNotes":notes,"mutedClips":muted,"soundtrack":project.soundtrack,"projectedClips":projected_clips,
            "projectedTracks":projected_tracks,"embeddedAudio":embedded,"looks":look_names,
            "messages":project.messages,"promptHistory":project.prompt_history,"missingFiles":project.library.missing}
    });
    let path = dir.join("project.zip");
    let zip_path = path.clone();
    let folder_paths = folder_paths.into_values().collect::<Vec<_>>();
    tokio::task::spawn_blocking(move || -> Result<()> {
        use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
        let mut zip = ZipWriter::new(File::create(&zip_path)?);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file(format!("{name}.room"), options)?;
        let doc = serde_json::to_vec(&document)?;
        ensure!(
            doc.len() as u64 <= MAX_DOCUMENT,
            "The .room document is too large."
        );
        zip.write_all(&doc)?;
        for folder in folder_paths {
            zip.add_directory(folder, options)?;
        }
        let mut size = 0_u64;
        for (name, path) in attachments {
            let mut file = File::open(path)?;
            size = size.saturating_add(file.metadata()?.len());
            ensure!(
                size + MAX_DOCUMENT <= MAX_BUNDLE,
                "The portable project exceeds 2 GB."
            );
            zip.start_file(name, options)?;
            io::copy(&mut file, &mut zip)?;
        }
        zip.finish()?.sync_all()?;
        Ok(())
    })
    .await??;
    Ok(path)
}
