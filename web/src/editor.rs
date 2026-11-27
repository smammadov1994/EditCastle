use crate::{captions::CaptionTrack, clock};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const STARTER_CREDITS: u32 = 180;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_stamp: Option<crate::library::FileStamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_digest: Option<String>,
    pub title: String,
    #[serde(alias = "frames")]
    pub duration: u32,
    pub audio: bool,
    pub video: bool,
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub notes: String,
}

impl Source {
    pub fn storage_id(&self) -> Uuid {
        self.storage.unwrap_or(self.id)
    }

    pub fn same_kind(&self, other: &Self) -> bool {
        self.video == other.video && self.image == other.image
    }
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipKind {
    #[default]
    Video,
    Audio,
    Image,
    Text,
    Solid,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: Uuid,
    pub source: Uuid,
    pub start: u32,
    pub end: u32,
    #[serde(default)]
    pub fade_in: bool,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub kind: ClipKind,
    #[serde(default)]
    pub track: Option<String>,
    #[serde(default)]
    pub position: Option<u32>,
    #[serde(default)]
    pub link: Option<Uuid>,
    #[serde(default)]
    pub audio_detached: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub captioned: bool,
    #[serde(default)]
    pub effects: Effects,
}

fn normal_speed() -> f64 {
    1.0
}

#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FadeCurve {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    Smooth,
}

impl FadeCurve {
    pub fn apply(self, amount: f64) -> f64 {
        let x = amount.clamp(0.0, 1.0);
        match self {
            Self::Linear => x,
            Self::EaseIn => x * x * x,
            Self::EaseOut => 1.0 - (1.0 - x).powi(3),
            Self::Smooth => x * x * (3.0 - 2.0 * x),
        }
    }

    pub fn expression(self, progress: &str) -> String {
        let x = format!("clip({progress},0,1)");
        match self {
            Self::Linear => x,
            Self::EaseIn => format!("pow({x},3)"),
            Self::EaseOut => format!("1-pow(1-{x},3)"),
            Self::Smooth => format!("pow({x},2)*(3-2*{x})"),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Effects {
    pub speed: f64,
    pub gain_db: f64,
    pub fade_in_frames: u32,
    pub fade_out_frames: u32,
    pub fade_in_curve: FadeCurve,
    pub fade_out_curve: FadeCurve,
    pub reverse: bool,
    pub freeze: bool,
    pub transition: Option<String>,
    pub transition_frames: u32,
    pub title: String,
    pub color: String,
    pub grade: Value,
    pub transform: Value,
    pub ramp: Value,
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            speed: normal_speed(),
            gain_db: 0.0,
            fade_in_frames: 0,
            fade_out_frames: 0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
            reverse: false,
            freeze: false,
            transition: None,
            transition_frames: 0,
            title: String::new(),
            color: "#272733".into(),
            grade: Value::Null,
            transform: Value::Null,
            ramp: Value::Null,
        }
    }
}

impl Effects {
    pub fn title_color(&self) -> &str {
        self.transform["color"].as_str().unwrap_or_else(|| {
            if self.color == "#272733" {
                "#FFFFFF"
            } else {
                &self.color
            }
        })
    }
}

impl Clip {
    pub fn source_clock(&self) -> Option<Vec<[u32; 2]>> {
        if self.effects.freeze {
            return None;
        }
        let ramp = crate::retime::Ramp::read(&self.effects.ramp)?;
        Some(
            ramp.points(
                clock::seconds(self.start),
                clock::seconds(self.end),
                self.effects.reverse,
            )
            .into_iter()
            .map(|(offset, elapsed)| [clock::ticks(offset), clock::ticks(elapsed)])
            .collect(),
        )
    }

    pub fn from_source(source: &Source) -> Self {
        Self {
            id: Uuid::new_v4(),
            source: source.id,
            start: 0,
            end: source.duration,
            fade_in: false,
            mute: false,
            kind: if source.image {
                ClipKind::Image
            } else if source.video {
                ClipKind::Video
            } else {
                ClipKind::Audio
            },
            track: None,
            position: None,
            link: None,
            audio_detached: false,
            captioned: false,
            effects: Effects {
                freeze: source.image,
                ..Effects::default()
            },
        }
    }

    pub fn link_audio(&mut self, track: String) -> Self {
        let link = Uuid::new_v4();
        self.link = Some(link);
        self.audio_detached = true;
        let mut audio = self.clone();
        audio.id = Uuid::new_v4();
        audio.kind = ClipKind::Audio;
        audio.track = Some(track);
        audio.audio_detached = false;
        audio
    }

    pub fn synthetic(kind: ClipKind, position: u32, duration: u32, track: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            source: Uuid::nil(),
            start: 0,
            end: duration,
            fade_in: false,
            mute: false,
            kind,
            track: Some(track),
            position: Some(position),
            link: None,
            audio_detached: false,
            captioned: false,
            effects: Effects::default(),
        }
    }

    pub fn track(&self) -> &str {
        self.track
            .as_deref()
            .unwrap_or(if self.kind == ClipKind::Audio {
                "A1"
            } else {
                "V1"
            })
    }

    pub fn duration(&self) -> u32 {
        let source_frames = self.end.saturating_sub(self.start);
        if self.effects.freeze {
            return source_frames;
        }
        if let Some(ramp) = crate::retime::Ramp::read(&self.effects.ramp) {
            return (ramp.span(clock::seconds(self.start), clock::seconds(self.end))
                * f64::from(clock::TICKS_PER_SECOND))
            .round()
            .max(1.0) as u32;
        }
        (f64::from(source_frames) / self.effects.speed)
            .round()
            .max(1.0) as u32
    }

    pub fn plays_source(&self) -> bool {
        matches!(self.kind, ClipKind::Video | ClipKind::Audio) && !self.effects.freeze
    }

    pub fn playback_source_bounds(&self, source_duration: u32) -> (u32, u32) {
        if self.effects.reverse && !self.effects.freeze {
            (
                source_duration.saturating_sub(self.end),
                source_duration.saturating_sub(self.start),
            )
        } else {
            (self.start, self.end)
        }
    }

    pub fn source_at(&self, elapsed: i64) -> u32 {
        if self.effects.freeze {
            return self.start;
        }
        let (from, elapsed) = if self.effects.reverse {
            (self.end, -elapsed)
        } else {
            (self.start, elapsed)
        };
        if let Some(ramp) = crate::retime::Ramp::read(&self.effects.ramp) {
            return clock::ticks(ramp.source(
                clock::seconds(from),
                elapsed as f64 / f64::from(clock::TICKS_PER_SECOND),
            ));
        }
        (f64::from(from) + elapsed as f64 * self.effects.speed)
            .round()
            .max(0.0) as u32
    }

    pub fn elapsed_at_source(&self, at: u32) -> i64 {
        let source = f64::from(at);
        let from = f64::from(if self.effects.reverse {
            self.end
        } else {
            self.start
        });
        let sign = if self.effects.reverse { -1.0 } else { 1.0 };
        let elapsed = if let Some(ramp) = crate::retime::Ramp::read(&self.effects.ramp) {
            let scale = f64::from(clock::TICKS_PER_SECOND);
            ramp.span(from / scale, source / scale) * scale
        } else {
            (source - from) / self.effects.speed
        };
        (sign * elapsed).round() as i64
    }

    pub fn visible_source_at(&self, elapsed: u32) -> u32 {
        let at = self.source_at(i64::from(elapsed));
        at.saturating_sub(u32::from(self.effects.reverse && !self.effects.freeze))
            .clamp(self.start, self.end - 1)
    }

    pub fn fade_gain(&self, elapsed: u32, duration: u32) -> f64 {
        let fx = &self.effects;
        let head = if fx.fade_in_frames == 0 {
            1.0
        } else {
            fx.fade_in_curve
                .apply(f64::from(elapsed) / f64::from(fx.fade_in_frames))
        };
        let tail = if fx.fade_out_frames == 0 {
            1.0
        } else {
            fx.fade_out_curve
                .apply(f64::from(duration.saturating_sub(elapsed)) / f64::from(fx.fade_out_frames))
        };
        head.min(tail)
    }

    pub fn fit_transform(&mut self, old_duration: u32, head_delta: i64) {
        if self.kind == ClipKind::Text || self.effects.transform["zoom"].is_null() {
            return;
        }
        let mut transform = crate::transform::Transform::read(
            &self.effects.transform,
            clock::seconds(old_duration),
        );
        transform.shift(
            head_delta as f64 / f64::from(clock::TICKS_PER_SECOND),
            clock::seconds(self.duration()),
        );
        self.effects.transform = transform.value();
    }

    pub fn trim_head(&mut self, at: u32) {
        let before = self.position.unwrap_or(0);
        let old_duration = self.duration();
        let duration = old_duration
            .saturating_sub(at.saturating_sub(before))
            .max(1);
        if self.effects.freeze {
            self.end = self.start.saturating_add(duration);
        } else if self.effects.reverse {
            self.end = self
                .source_at(i64::from(at) - i64::from(before))
                .max(self.start + 1);
        } else {
            self.start = self
                .source_at(i64::from(at) - i64::from(before))
                .min(self.end - 1);
        }
        self.position = Some(at);
        self.clamp_fades();
        self.fit_transform(old_duration, i64::from(at) - i64::from(before));
    }

    pub fn trim_tail(&mut self, at: u32) {
        let before = self.position.unwrap_or(0);
        let old_duration = self.duration();
        if self.effects.freeze {
            self.end = self.start.saturating_add(at.saturating_sub(before).max(1));
        } else if self.effects.reverse {
            self.start = self
                .source_at(i64::from(at) - i64::from(before))
                .min(self.end - 1);
        } else {
            self.end = self
                .source_at(i64::from(at) - i64::from(before))
                .max(self.start + 1);
        }
        self.clamp_fades();
        self.fit_transform(old_duration, 0);
    }

    pub fn clamp_fades(&mut self) {
        let duration = self.duration();
        self.effects.fade_in_frames = self.effects.fade_in_frames.min(duration);
        self.effects.fade_out_frames = self
            .effects
            .fade_out_frames
            .min(duration - self.effects.fade_in_frames);
    }

    pub fn name<'a>(&'a self, p: &'a Project) -> &'a str {
        match self.kind {
            ClipKind::Text => &self.effects.title,
            ClipKind::Solid => "Solid",
            _ => p
                .source(self.source)
                .map(|source| source.title.as_str())
                .unwrap_or("Offline media"),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub name: String,
    pub id: Uuid,
    pub hidden: bool,
    pub locked: bool,
    pub muted: bool,
    pub solo: bool,
    pub gain_db: f64,
    #[serde(default)]
    pub duck_under: Option<Uuid>,
    #[serde(default = "default_duck_db")]
    pub duck_db: f64,
}

fn default_duck_db() -> f64 {
    -12.0
}

impl Track {
    pub fn new(name: String) -> Self {
        Self {
            name,
            id: Uuid::new_v4(),
            hidden: false,
            locked: false,
            muted: false,
            solo: false,
            gain_db: 0.0,
            duck_under: None,
            duck_db: default_duck_db(),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: Uuid,
    pub frame: u32,
    pub name: String,
    pub color: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct LookupTable {
    pub id: Uuid,
    pub name: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sequence {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_locked: Option<bool>,
    pub tracks: Vec<Track>,
    pub markers: Vec<Marker>,
    pub in_point: Option<u32>,
    pub out_point: Option<u32>,
    pub playhead: u32,
    pub playing: bool,
    pub captions: Option<CaptionTrack>,
    pub render_model: crate::render::Model,
    pub luts: Vec<LookupTable>,
}

impl Default for Sequence {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30.0,
            settings_locked: Some(false),
            tracks: ["V1", "V2", "A1", "A2"]
                .map(|name| Track::new(name.into()))
                .into(),
            markers: vec![],
            in_point: None,
            out_point: None,
            playhead: 0,
            playing: false,
            captions: None,
            render_model: crate::render::Model::default(),
            luts: vec![],
        }
    }
}

impl Sequence {
    pub fn offers_source_format(&self) -> bool {
        // A saved project from before the import prompt already has established settings.
        self.settings_locked == Some(false)
    }

    pub fn format(&self) -> SequenceFormat {
        SequenceFormat {
            width: self.width,
            height: self.height,
            fps: self.fps,
        }
    }

    pub fn set_format(&mut self, format: SequenceFormat) {
        self.width = format.width;
        self.height = format.height;
        self.fps = format.fps;
    }
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SequenceFormat {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SequenceOffer {
    pub asset: Uuid,
    pub version: Uuid,
    pub format: SequenceFormat,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub timeline: Vec<Clip>,
    pub soundtrack: Option<Uuid>,
    pub sequence: Sequence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<LibraryChange>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct LibrarySnapshot {
    pub sources: Vec<Source>,
    pub layout: crate::library::Library,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryChange {
    pub before: LibrarySnapshot,
    pub after: LibrarySnapshot,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub from: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio: Option<crate::studio_history::Turn>,
}

impl Message {
    pub fn new(from: &str, text: String) -> Self {
        Self {
            from: from.into(),
            text,
            citations: vec![],
            studio: None,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Citation {
    pub url: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<usize>,
}

impl Citation {
    pub fn new(url: &str, label: &str, after: Option<usize>) -> Option<Self> {
        let url = crate::public_web::address(url).ok()?;
        Some(Self {
            label: if label.trim().is_empty() {
                url.domain()?.into()
            } else {
                label.trim().chars().take(120).collect()
            },
            url: url.to_string(),
            after,
        })
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_name: Option<String>,
    pub sources: Vec<Source>,
    pub timeline: Vec<Clip>,
    pub soundtrack: Option<Uuid>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_history: Vec<crate::studio_history::PromptEntry>,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "crate::library::Library::is_empty")]
    pub library: crate::library::Library,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub library_revision: u64,
    #[serde(default)]
    pub sequence: Sequence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_offer: Option<SequenceOffer>,
    #[serde(default)]
    pub undo_steps: Vec<Snapshot>,
    #[serde(default)]
    pub redo_steps: Vec<Snapshot>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

pub fn export_file_stem(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 100 || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.replace(['/', '\\', ':', '"', '*', '?', '<', '>', '|'], "_"))
}

impl Project {
    pub fn new(title: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            title,
            export_name: None,
            sources: vec![],
            timeline: vec![],
            soundtrack: None,
            messages: vec![],
            prompt_history: vec![],
            revision: 0,
            library: crate::library::Library::default(),
            library_revision: 0,
            sequence: Sequence::default(),
            sequence_offer: None,
            undo_steps: vec![],
            redo_steps: vec![],
        }
    }

    pub fn remember_message(&mut self, message: Message) {
        self.messages.push(message);
        self.messages.drain(
            ..self
                .messages
                .len()
                .saturating_sub(crate::studio_history::MAX_MESSAGES),
        );
    }

    pub fn copy_as(&self, title: String) -> Self {
        let mut copy = self.clone();
        copy.id = Uuid::new_v4();
        copy.title = title;
        copy.revision = 0;
        copy.library_revision = 0;
        copy.sequence.playing = false;

        // Asset IDs are used by the global media routes. Each project gets its
        // own IDs while sharing the immutable versions already stored on disk.
        let mut ids = HashMap::new();
        let mut remap = |id| *ids.entry(id).or_insert_with(Uuid::new_v4);
        remap_library_assets(&mut copy.sources, &mut copy.library, &mut remap);
        remap_timeline_assets(
            &mut copy.timeline,
            &mut copy.soundtrack,
            &mut copy.sequence,
            &mut remap,
        );
        if let Some(offer) = &mut copy.sequence_offer {
            offer.asset = remap(offer.asset);
        }
        for turn in copy
            .messages
            .iter_mut()
            .filter_map(|message| message.studio.as_mut())
        {
            for id in &mut turn.asset_ids {
                *id = remap(*id);
            }
        }
        for snapshot in copy.undo_steps.iter_mut().chain(&mut copy.redo_steps) {
            remap_timeline_assets(
                &mut snapshot.timeline,
                &mut snapshot.soundtrack,
                &mut snapshot.sequence,
                &mut remap,
            );
            if let Some(change) = &mut snapshot.library {
                for snapshot in [&mut change.before, &mut change.after] {
                    remap_library_assets(&mut snapshot.sources, &mut snapshot.layout, &mut remap);
                }
            }
        }
        copy
    }

    pub fn source(&self, id: Uuid) -> Option<&Source> {
        self.sources.iter().find(|source| source.id == id)
    }

    pub fn lock_sequence_settings(&mut self) {
        if self.sequence.offers_source_format() {
            self.sequence.settings_locked = Some(true);
        }
        self.sequence_offer = None;
    }

    pub fn offer_sequence_format(&mut self, offer: Option<SequenceOffer>) {
        let Some(offer) = offer.filter(|_| self.sequence.offers_source_format()) else {
            return;
        };
        self.lock_sequence_settings();
        if self.sequence.format() != offer.format {
            self.sequence_offer = Some(offer);
        }
        self.revision += 1;
    }

    pub fn pending_sequence_offer(&self) -> Option<&SequenceOffer> {
        self.sequence_offer.as_ref().filter(|offer| {
            self.source(offer.asset)
                .is_some_and(|source| source.storage_id() == offer.version)
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            timeline: self.timeline.clone(),
            soundtrack: self.soundtrack,
            sequence: self.sequence.clone(),
            library: None,
        }
    }

    pub fn capture_library(&self) -> LibrarySnapshot {
        LibrarySnapshot {
            sources: self.sources.clone(),
            layout: self.library.clone(),
        }
    }

    pub fn record(&mut self, previous: Snapshot) {
        self.undo_steps.push(previous);
        if self.undo_steps.len() > 30 {
            self.undo_steps.remove(0);
        }
        self.redo_steps.clear();
    }

    pub fn history(&mut self, undo: bool) -> Result<()> {
        let from = if undo {
            &mut self.undo_steps
        } else {
            &mut self.redo_steps
        };
        let prior = from.pop().context(if undo {
            "No edit to undo."
        } else {
            "No edit to redo."
        })?;
        let mut state = self.snapshot();
        if let Some(change) = prior.library {
            self.apply_library_delta(&change.after, &change.before);
            state.library = Some(LibraryChange {
                before: change.after,
                after: change.before,
            });
        }
        if undo {
            self.redo_steps.push(state);
        } else {
            self.undo_steps.push(state);
        }
        self.timeline = prior.timeline;
        self.soundtrack = prior.soundtrack;
        self.sequence = prior.sequence;
        if self.sequence.offers_source_format() {
            self.sequence_offer = None;
        }
        Ok(())
    }

    pub fn apply_library_delta(&mut self, from: &LibrarySnapshot, to: &LibrarySnapshot) {
        let original = from
            .sources
            .iter()
            .map(|source| (source.id, source))
            .collect::<HashMap<_, _>>();
        let order = to
            .sources
            .iter()
            .enumerate()
            .map(|(index, source)| (source.id, index))
            .collect::<HashMap<_, _>>();
        self.sources
            .retain(|source| !original.contains_key(&source.id) || order.contains_key(&source.id));
        let installed = self
            .sources
            .iter()
            .enumerate()
            .map(|(index, source)| (source.id, index))
            .collect::<HashMap<_, _>>();
        for target in &to.sources {
            let prior = original.get(&target.id).copied();
            if prior == Some(target) {
                continue;
            }
            if let Some(index) = installed.get(&target.id) {
                let source = &mut self.sources[*index];
                let mut restored = target.clone();
                if let Some(prior) = prior {
                    if prior.title == target.title {
                        restored.title.clone_from(&source.title);
                    }
                    if prior.notes == target.notes {
                        restored.notes.clone_from(&source.notes);
                    }
                }
                *source = restored;
            } else {
                self.sources.push(target.clone());
            }
        }
        self.sources
            .sort_by_key(|source| order.get(&source.id).copied().unwrap_or(usize::MAX));
        self.library.restore(&from.layout, &to.layout);
        self.library_revision += 1;
    }

    pub fn replace_sources(&mut self, sources: Vec<(Uuid, Source)>) {
        self.replace_sources_and_layout(sources, |_| {});
    }

    pub fn replace_sources_and_layout(
        &mut self,
        sources: Vec<(Uuid, Source)>,
        update: impl FnOnce(&mut crate::library::Library),
    ) {
        if sources.is_empty() {
            return;
        }
        let mut previous = self.snapshot();
        let before = self.capture_library();
        // Materialize positions before shortening implicit earlier clips.
        self.materialize_positions();
        let mut changed = std::collections::HashMap::new();
        for (id, mut loaded) in sources {
            let Some(old) = self.sources.iter_mut().find(|source| source.id == id) else {
                continue;
            };
            loaded.storage = Some(loaded.id);
            loaded.id = id;
            loaded.title.clone_from(&old.title);
            loaded.notes.clone_from(&old.notes);
            changed.insert(id, (old.duration, loaded.clone()));
            *old = loaded;
            self.library.missing.remove(&id);
        }
        self.timeline.retain_mut(|clip| {
            let Some((old_length, source)) = changed.get(&clip.source) else {
                return true;
            };
            if clip.kind == ClipKind::Audio && !source.audio {
                return false;
            }
            if clip.effects.reverse && !clip.effects.freeze {
                let from = old_length.saturating_sub(clip.end);
                let to = old_length.saturating_sub(clip.start);
                if from >= source.duration {
                    return false;
                }
                clip.start = source.duration.saturating_sub(to);
                clip.end = source.duration - from;
                if clip.effects.ramp.is_object() {
                    let offset = (f64::from(source.duration) - f64::from(*old_length))
                        / f64::from(clock::TICKS_PER_SECOND);
                    for point in ["from", "to"] {
                        if let Some(at) = clip.effects.ramp[point].as_f64() {
                            clip.effects.ramp[point] = serde_json::json!(at + offset);
                        }
                    }
                }
            } else if clip.start >= source.duration {
                return false;
            } else if !clip.effects.freeze {
                clip.end = clip.end.min(source.duration);
            }
            clip.clamp_fades();
            clip.start < clip.end
        });
        if self
            .soundtrack
            .is_some_and(|id| changed.get(&id).is_some_and(|(_, source)| !source.audio))
        {
            self.soundtrack = None;
        }
        if let Some(captions) = &mut self.sequence.captions {
            captions
                .fixes
                .retain(|id, _| self.sources.iter().any(|source| source.id == *id));
            captions.edits.retain(|edit| {
                edit.source
                    .is_some_and(|id| self.sources.iter().any(|source| source.id == id))
            });
        }
        update(&mut self.library);
        previous.library = Some(LibraryChange {
            before,
            after: self.capture_library(),
        });
        self.record(previous);
        self.library_revision += 1;
        self.revision += 1;
    }

    pub fn placement(&self, id: Uuid) -> Option<u32> {
        let mut ends = std::collections::HashMap::<String, u32>::new();
        for clip in &self.timeline {
            let next = ends.entry(clip.track().to_owned()).or_default();
            let at = clip.position.unwrap_or(*next);
            if clip.id == id {
                return Some(at);
            }
            *next = (*next).max(at.saturating_add(clip.duration()));
        }
        None
    }

    pub fn frame_step(&self) -> u32 {
        clock::step(self.sequence.fps)
    }

    pub fn materialize_positions(&mut self) {
        let positions = self
            .timeline
            .iter()
            .map(|clip| self.placement(clip.id).unwrap_or(0))
            .collect::<Vec<_>>();
        for (clip, position) in self.timeline.iter_mut().zip(positions) {
            clip.position = Some(position);
        }
    }

    pub fn overwrite(&mut self, keep: &std::collections::HashSet<Uuid>) {
        let movers = self
            .timeline
            .iter()
            .filter(|clip| keep.contains(&clip.id))
            .cloned()
            .collect::<Vec<_>>();
        for edit in movers {
            let start = edit.position.unwrap_or(0);
            let end = start + edit.duration();
            let mut timeline = Vec::with_capacity(self.timeline.len() + 1);
            for clip in self.timeline.drain(..) {
                let at = clip.position.unwrap_or(0);
                let clip_end = at + clip.duration();
                if keep.contains(&clip.id)
                    || clip.track() != edit.track()
                    || clip_end <= start
                    || at >= end
                {
                    timeline.push(clip);
                    continue;
                }
                if at < start {
                    let mut left = clip.clone();
                    if clip_end > end {
                        left.effects.fade_out_frames = 0;
                    }
                    left.trim_tail(start);
                    timeline.push(left);
                }
                if clip_end > end {
                    let mut right = clip;
                    if at < start {
                        right.id = Uuid::new_v4();
                        right.link = None;
                        right.effects.fade_in_frames = 0;
                    }
                    right.trim_head(end);
                    timeline.push(right);
                }
            }
            self.timeline = timeline;
        }
    }

    pub fn retime(
        &mut self,
        ids: &std::collections::HashSet<Uuid>,
        mut change: impl FnMut(&mut Clip),
    ) {
        for track in &self.sequence.tracks {
            let mut order = self
                .timeline
                .iter_mut()
                .filter(|clip| clip.track() == track.name)
                .collect::<Vec<_>>();
            if !order
                .iter()
                .any(|clip| ids.contains(&clip.id) && clip.plays_source())
            {
                continue;
            }
            order.sort_by_key(|clip| clip.position.unwrap_or(0));
            let mut right = 0;
            let mut shift = 0_i64;
            for clip in order {
                let at = i64::from(clip.position.unwrap_or(0));
                let before = clip.duration();
                let changed = ids.contains(&clip.id);
                let next = if changed { at + shift } else { at };
                if changed && clip.plays_source() {
                    change(clip);
                    clip.clamp_fades();
                    clip.fit_transform(before, 0);
                    shift += i64::from(clip.duration()) - i64::from(before);
                }
                let at = next.max(i64::from(right)).min(i64::from(u32::MAX)) as u32;
                clip.position = Some(at);
                right = at.saturating_add(clip.duration());
            }
        }
    }

    pub fn track_end(&self, track: &str) -> u32 {
        self.timeline
            .iter()
            .filter(|clip| clip.track() == track)
            .map(|clip| {
                self.placement(clip.id)
                    .unwrap_or(0)
                    .saturating_add(clip.duration())
            })
            .max()
            .unwrap_or(0)
    }

    pub fn duration(&self) -> u32 {
        self.sequence
            .tracks
            .iter()
            .map(|track| self.track_end(&track.name))
            .max()
            .unwrap_or(0)
    }

    pub fn play_range(&self) -> Option<(u32, u32)> {
        let (start, end) = (self.sequence.in_point, self.sequence.out_point);
        if start.is_none() && end.is_none() {
            return None;
        }
        let first = start.unwrap_or(0);
        let last = end.unwrap_or_else(|| self.duration());
        let (lo, hi) = (first.min(last), first.max(last));
        (hi - lo >= self.frame_step()).then_some((lo, hi))
    }

    pub fn public(&self) -> Value {
        let transitions = crate::transition::layout(self);
        let timeline: Vec<Value> = self
            .timeline
            .iter()
            .map(|clip| {
                let mut value = serde_json::to_value(clip).expect("serializable timeline clip");
                value["duration"] = serde_json::json!(clip.duration());
                if clip.effects.transform.is_object()
                    && matches!(
                        clip.kind,
                        ClipKind::Video | ClipKind::Image | ClipKind::Solid
                    )
                {
                    value["effects"]["transform"] = crate::transform::Transform::read(
                        &clip.effects.transform,
                        clock::seconds(clip.duration()),
                    )
                    .value();
                }
                if let Some(source) = self.source(clip.source)
                    && source.audio
                    && let Some(key) = crate::media::timed_audio_key(clip, source)
                {
                    value["timed_audio"] = serde_json::json!(key);
                }
                if let Some(plan) = transitions.plans.get(&clip.id) {
                    value["transition_plan"] = serde_json::json!(plan);
                    if plan.extends() && !clip.effects.freeze && clip.kind == ClipKind::Video {
                        let (from, to) = plan.source_range(clip, self);
                        let points = crate::retime::Ramp::read(&clip.effects.ramp).map(|ramp| {
                            ramp.points(
                                clock::seconds(from),
                                clock::seconds(to),
                                clip.effects.reverse,
                            )
                            .into_iter()
                            .map(|(source, time)| [clock::ticks(source), clock::ticks(time)])
                            .collect::<Vec<_>>()
                        });
                        value["transition_source"] =
                            serde_json::json!({"start":from,"end":to,"points":points});
                    }
                }
                if let Some(source_clock) = clip.source_clock() {
                    value["source_clock"] = serde_json::json!(source_clock);
                }
                value
            })
            .collect();
        serde_json::json!({
            "id": self.id, "title": self.title, "export_name": self.export_name,
            "sources": self.sources.iter().map(|source| self.library.source_json(source)).collect::<Vec<_>>(),
            "folders": self.library.folders.iter().map(crate::library::Folder::public).collect::<Vec<_>>(),
            "library_revision":self.library_revision,
            "timeline": timeline, "cuts":transitions.cuts,"play_range":self.play_range(),"soundtrack": self.soundtrack, "messages": self.messages,
            "prompt_history":self.prompt_history,
            "director_mentions": crate::director_prompt::suggestions(self),
            "revision": self.revision, "sequence": self.sequence, "frame_step": self.frame_step(),
            "sequence_offer": self.pending_sequence_offer(),
            "canUndo": !self.undo_steps.is_empty(), "canRedo": !self.redo_steps.is_empty()
        })
    }

    pub fn validate_timeline(&self, clips: &[Clip], soundtrack: Option<Uuid>) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        if clips.len() > 500 {
            bail!("The timeline supports up to 500 clips.");
        }
        for clip in clips {
            let track = self
                .sequence
                .tracks
                .iter()
                .find(|track| track.name == clip.track())
                .context("That timeline track does not exist.")?;
            if !seen.insert(clip.id)
                || clip.start >= clip.end
                || clip.duration() > clock::MAX_TIME
                || clip.end > clock::MAX_TIME
                || clip.position.is_some_and(|frame| frame > clock::MAX_TIME)
                || !(0.05..=100.0).contains(&clip.effects.speed)
                || track.name.starts_with('A') != (clip.kind == ClipKind::Audio)
            {
                bail!("One of the clip ranges is invalid.");
            }
            if matches!(
                clip.kind,
                ClipKind::Video | ClipKind::Audio | ClipKind::Image
            ) {
                let source = self
                    .source(clip.source)
                    .context("This source does not belong to the project.")?;
                let media_ok = match clip.kind {
                    ClipKind::Video => source.video,
                    ClipKind::Image => source.image,
                    ClipKind::Audio => source.audio,
                    _ => false,
                };
                if !media_ok
                    || clip.start >= source.duration
                    || (!clip.effects.freeze && clip.end > source.duration)
                {
                    bail!("The clip exceeds its source.");
                }
            }
        }
        if soundtrack.is_some_and(|id| !self.source(id).is_some_and(|source| source.audio)) {
            bail!("Choose an audio source from this project.");
        }
        Ok(())
    }
}

fn remap_library_assets(
    sources: &mut [Source],
    library: &mut crate::library::Library,
    remap: &mut impl FnMut(Uuid) -> Uuid,
) {
    for source in sources {
        source.storage = Some(source.storage_id());
        source.id = remap(source.id);
    }
    library.entries = std::mem::take(&mut library.entries)
        .into_iter()
        .map(|(id, entry)| (remap(id), entry))
        .collect();
    library.missing = std::mem::take(&mut library.missing)
        .into_iter()
        .map(|(id, name)| (remap(id), name))
        .collect();
}

fn remap_timeline_assets(
    timeline: &mut [Clip],
    soundtrack: &mut Option<Uuid>,
    sequence: &mut Sequence,
    remap: &mut impl FnMut(Uuid) -> Uuid,
) {
    for clip in timeline.iter_mut().filter(|clip| !clip.source.is_nil()) {
        clip.source = remap(clip.source);
    }
    if let Some(id) = soundtrack {
        *id = remap(*id);
    }
    if let Some(captions) = &mut sequence.captions {
        for source in captions
            .edits
            .iter_mut()
            .filter_map(|edit| edit.source.as_mut())
        {
            *source = remap(*source);
        }
        captions.fixes = std::mem::take(&mut captions.fixes)
            .into_iter()
            .map(|(id, fixes)| (remap(id), fixes))
            .collect();
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct VisibleEdit {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<usize>,
    pub clip: Uuid,
    pub before: Option<u32>,
    pub after: Option<u32>,
    pub track: String,
    #[serde(default)]
    pub from_track: Option<String>,
    #[serde(default)]
    pub source: Option<Uuid>,
    #[serde(default)]
    pub before_width: Option<u32>,
    #[serde(default)]
    pub after_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruler_item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// What changed inside the clip, as [before, after] pairs, so the browser can perform each
    /// change as its own gesture: gain, fade_in, fade_out, grade, transition, typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: Uuid,
    pub project: Uuid,
    pub status: String,
    pub events: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio_turn: Option<crate::studio_history::Turn>,
    #[serde(default)]
    pub edits: Vec<VisibleEdit>,
    #[serde(default)]
    pub stop_requested: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub applied: bool,
}

impl Run {
    pub fn new(project: Uuid, events: Vec<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            project,
            status: "running".into(),
            events,
            started_at: Some(now()),
            finished_at: None,
            studio_turn: None,
            edits: vec![],
            stop_requested: false,
            applied: false,
        }
    }

    pub fn pending(&self) -> bool {
        self.status == "running"
    }

    pub fn studio_turn_mut(&mut self) -> &mut crate::studio_history::Turn {
        self.studio_turn
            .get_or_insert_with(|| crate::studio_history::Turn::new(self.id, None, vec![]))
    }

    pub fn finish(&mut self, status: &str, mut message: Message) -> Message {
        let mut turn = self
            .studio_turn
            .take()
            .unwrap_or_else(|| crate::studio_history::Turn::new(self.id, None, vec![]));
        turn.failed = status == "failed";
        turn.remember_events(&self.events);
        message.studio = Some(turn);
        self.status = status.into();
        self.finished_at = self.started_at.map(|started| now().max(started));
        self.events.push(message.text.clone());
        message
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ChargeState {
    Reserved,
    Spent,
    Released,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Charge {
    pub id: Uuid,
    pub title: String,
    pub amount: u32,
    pub state: ChargeState,
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_requested_at: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ReviewedShot {
    source: Source,
    workspace: Uuid,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RenderReceipt {
    pub project: Uuid,
    pub run: Uuid,
    pub title: String,
    pub visible: u32,
    pub direction: String,
    pub reference: Option<String>,
    pub issue: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Database {
    #[serde(default)]
    timebase: u32,
    #[serde(default = "autosave_default")]
    pub autosave: bool,
    pub projects: Vec<Project>,
    // Working projects are also persisted for crash recovery. A checkpoint holds the
    // last saved version of a project with unsaved changes in its working copy.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    checkpoints: HashMap<Uuid, Project>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    reviewed_shots: HashMap<Uuid, Vec<ReviewedShot>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    render_receipts: HashMap<Uuid, RenderReceipt>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    purchases: HashMap<String, CreditPurchase>,
    pub runs: Vec<Run>,
    pub charges: Vec<Charge>,
    pub granted: u32,
}

#[derive(Clone, Serialize, Deserialize)]
struct CreditPurchase {
    amount: u32,
    credits: u32,
    paid: bool,
}

fn autosave_default() -> bool {
    true
}

impl Database {
    pub fn register_checkout(&mut self, id: String, amount: u32, credits: u32) {
        self.purchases.entry(id).or_insert(CreditPurchase {
            amount,
            credits,
            paid: false,
        });
    }

    pub fn settle_checkout(&mut self, id: &str, amount: u32) -> Result<()> {
        let Some(purchase) = self.purchases.get_mut(id) else {
            return Ok(());
        };
        anyhow::ensure!(
            purchase.amount == amount,
            "The payment total does not match the credit pack."
        );
        if !purchase.paid {
            self.granted = self
                .granted
                .checked_add(purchase.credits)
                .context("The credit balance is too large.")?;
            purchase.paid = true;
        }
        Ok(())
    }

    pub fn take_reviewed_shots(&mut self, run: Uuid) -> Vec<Source> {
        self.reviewed_shots
            .remove(&run)
            .unwrap_or_default()
            .into_iter()
            .map(|shot| shot.source)
            .collect()
    }

    pub fn has_reviewed_shot(&self, run: Uuid, job: Uuid) -> bool {
        self.reviewed_shots
            .get(&run)
            .is_some_and(|shots| shots.iter().any(|shot| shot.source.id == job))
    }

    pub fn render_receipt(&self, project: Uuid, job: Uuid) -> Option<&RenderReceipt> {
        self.render_receipts.get(&job).filter(|receipt| {
            receipt.project == project
                && self.runs.iter().any(|run| {
                    run.id == receipt.run
                        && run.project == project
                        && !run.pending()
                        && !run.stop_requested
                })
        })
    }

    pub fn recoverable_renders(&self) -> impl Iterator<Item = (Uuid, &RenderReceipt)> {
        self.charges.iter().rev().filter_map(|charge| {
            let project = self.render_receipts.get(&charge.id)?.project;
            self.render_receipt(project, charge.id)
                .map(|receipt| (charge.id, receipt))
        })
    }

    pub fn record_render_issue(&mut self, job: Uuid, issue: String) {
        if let Some(receipt) = self.render_receipts.get_mut(&job) {
            receipt.issue = Some(issue);
        }
    }

    pub fn finish_render_receipt(&mut self, job: Uuid) {
        self.render_receipts.remove(&job);
    }

    pub fn publish_recovered_shot(&mut self, project: Uuid, source: Source) -> Result<()> {
        let job = source.id;
        let receipt = self
            .render_receipt(project, job)
            .context("This render is no longer available to recover.")?;
        let run_id = receipt.run;
        let label = format!("Recovered {} to Media", receipt.title);
        let project = self
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .context("Project not found.")?;
        if project.source(job).is_none() {
            project.sources.push(source);
            project.library_revision += 1;
        }
        if let Some(turn) = project
            .messages
            .iter_mut()
            .filter_map(|message| message.studio.as_mut())
            .find(|turn| turn.id == run_id)
        {
            if !turn.asset_ids.contains(&job) {
                turn.asset_ids.push(job);
                turn.record_event(&label);
            }
        } else {
            let mut message = Message::new("director", label.clone());
            message.studio = Some(crate::studio_history::Turn::new(run_id, None, vec![job]));
            project.remember_message(message);
        }
        if let Some(run) = self.runs.iter_mut().find(|run| run.id == run_id) {
            if run.events.len() >= crate::studio_history::MAX_EVENTS {
                run.events.remove(0);
            }
            run.events.push(label);
        }
        self.finish_render_receipt(job);
        if let Some(shots) = self.reviewed_shots.get_mut(&run_id) {
            shots.retain(|shot| shot.source.id != job);
            if shots.is_empty() {
                self.reviewed_shots.remove(&run_id);
            }
        }
        Ok(())
    }

    pub fn discard_stopped_renders(&mut self) {
        self.render_receipts.retain(|job, receipt| {
            let run = self.runs.iter().find(|run| run.id == receipt.run);
            if run.is_some_and(|run| run.stop_requested) {
                if let Some(charge) = self
                    .charges
                    .iter_mut()
                    .find(|charge| charge.id == *job && charge.state == ChargeState::Reserved)
                {
                    charge.cancel_requested_at.get_or_insert_with(now);
                }
                false
            } else {
                run.is_some()
            }
        });
    }

    pub fn add_project(&mut self, project: Project) -> Result<()> {
        anyhow::ensure!(
            self.projects.len() < 200,
            "The studio has reached its project limit."
        );
        self.projects.push(project);
        Ok(())
    }

    pub fn has_unsaved_changes(&self, id: Uuid) -> bool {
        self.checkpoints.contains_key(&id)
    }

    fn migrate_timebase(&mut self) -> Result<()> {
        if self.timebase == clock::TICKS_PER_SECOND {
            return Ok(());
        }
        ensure_legacy(self.timebase)?;
        for project in self
            .projects
            .iter_mut()
            .chain(self.checkpoints.values_mut())
        {
            for source in &mut project.sources {
                scale_legacy(&mut source.duration);
            }
            scale_timeline(&mut project.timeline, &mut project.sequence);
            for snapshot in project.undo_steps.iter_mut().chain(&mut project.redo_steps) {
                scale_timeline(&mut snapshot.timeline, &mut snapshot.sequence);
            }
        }
        for edit in self.runs.iter_mut().flat_map(|run| &mut run.edits) {
            for value in [
                &mut edit.before,
                &mut edit.after,
                &mut edit.before_width,
                &mut edit.after_width,
            ]
            .into_iter()
            .flatten()
            {
                scale_legacy(value);
            }
        }
        self.timebase = clock::TICKS_PER_SECOND;
        Ok(())
    }

    pub fn available(&self) -> u32 {
        self.granted.saturating_sub(
            self.charges
                .iter()
                .filter(|charge| charge.state != ChargeState::Released)
                .map(|charge| charge.amount)
                .sum(),
        )
    }

    pub fn reserve_render(
        &mut self,
        id: Uuid,
        run: Uuid,
        title: String,
        amount: u32,
        visible: u32,
        direction: String,
        reference: Option<String>,
    ) -> Result<()> {
        let project = self
            .runs
            .iter()
            .find(|entry| entry.id == run && entry.pending() && !entry.stop_requested)
            .context("Director closed this draft.")?
            .project;
        if self.available() < amount {
            bail!(
                "This shot needs {amount} credits. {available} are available.",
                available = self.available()
            );
        }
        self.charges.push(Charge {
            id,
            title: title.clone(),
            amount,
            state: ChargeState::Reserved,
            created: now(),
            cancel_requested_at: None,
        });
        self.render_receipts.insert(
            id,
            RenderReceipt {
                project,
                run,
                title,
                visible,
                direction,
                reference,
                issue: None,
            },
        );
        Ok(())
    }

    pub fn settle(&mut self, id: Uuid, state: ChargeState) {
        if let Some(charge) = self
            .charges
            .iter_mut()
            .find(|charge| charge.id == id && charge.state == ChargeState::Reserved)
        {
            let released = state == ChargeState::Released;
            charge.state = state;
            if released {
                self.finish_render_receipt(id);
            }
        }
    }
}

fn ensure_legacy(timebase: u32) -> Result<()> {
    anyhow::ensure!(
        timebase == 0 || timebase == crate::FPS,
        "This project uses an unsupported edit clock."
    );
    Ok(())
}

fn scale_legacy(value: &mut u32) {
    *value = value.saturating_mul(clock::MODEL_FRAME);
}

fn scale_timeline(timeline: &mut [Clip], sequence: &mut Sequence) {
    for clip in timeline {
        scale_legacy(&mut clip.start);
        scale_legacy(&mut clip.end);
        if let Some(position) = &mut clip.position {
            scale_legacy(position);
        }
        scale_legacy(&mut clip.effects.fade_in_frames);
        scale_legacy(&mut clip.effects.fade_out_frames);
        scale_legacy(&mut clip.effects.transition_frames);
    }
    scale_legacy(&mut sequence.playhead);
    for point in [&mut sequence.in_point, &mut sequence.out_point]
        .into_iter()
        .flatten()
    {
        scale_legacy(point);
    }
    for marker in &mut sequence.markers {
        scale_legacy(&mut marker.frame);
    }
    if let Some(captions) = &mut sequence.captions {
        for change in &mut captions.edits {
            scale_legacy(&mut change.source_frame);
        }
    }
}

pub struct Store {
    pub root: PathBuf,
    pub account: bool,
    pub audio_render_slot: tokio::sync::Mutex<()>,
    pub audio_analysis_slot: tokio::sync::Mutex<()>,
    pub transcription_slot: tokio::sync::Mutex<()>,
    db: Mutex<Database>,
    _lock: File,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_workspace(path, None)
    }

    pub fn open_workspace(path: &Path, initial_account_credits: Option<u32>) -> Result<Self> {
        if !path.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
        }
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.join("lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .context("This studio directory is already in use")?;
        let ledger = path.join("studio.json");
        let mut db: Database = if ledger.exists() {
            serde_json::from_slice(&fs::read(ledger)?).context("Reading studio projects")?
        } else {
            Database {
                timebase: clock::TICKS_PER_SECOND,
                autosave: autosave_default(),
                projects: vec![Project::new("Untitled film".into())],
                checkpoints: HashMap::new(),
                reviewed_shots: HashMap::new(),
                render_receipts: HashMap::new(),
                purchases: HashMap::new(),
                runs: vec![],
                charges: vec![],
                granted: initial_account_credits.unwrap_or(STARTER_CREDITS),
            }
        };
        db.migrate_timebase()?;
        for p in db.projects.iter_mut().chain(db.checkpoints.values_mut()) {
            crate::captions::migrate(&mut p.timeline, &mut p.sequence.captions);
            for snapshot in p.undo_steps.iter_mut().chain(&mut p.redo_steps) {
                crate::captions::migrate(&mut snapshot.timeline, &mut snapshot.sequence.captions);
            }
        }
        fs::create_dir_all(path.join("media"))?;
        let store = Self {
            root: path.to_owned(),
            account: initial_account_credits.is_some(),
            audio_render_slot: tokio::sync::Mutex::new(()),
            audio_analysis_slot: tokio::sync::Mutex::new(()),
            transcription_slot: tokio::sync::Mutex::new(()),
            db: Mutex::new(db),
            _lock: lock,
        };
        let workspaces = store.update(|db| {
            db.discard_stopped_renders();
            let workspaces = store.recover_reviewed_shots(db)?;
            for run in db.runs.iter_mut().filter(|run| run.pending()) {
                let (status, note) = if run.applied {
                    (
                        "done",
                        "Director saved the edit before the studio restarted.",
                    )
                } else if run
                    .studio_turn
                    .as_ref()
                    .is_some_and(|turn| !turn.asset_ids.is_empty())
                {
                    (
                        "failed",
                        "The studio restarted during this run. Reviewed shots are in Media.",
                    )
                } else {
                    (
                        "failed",
                        "The studio restarted before Director finished this run.",
                    )
                };
                let message = run.finish(status, Message::new("director", note.into()));
                if let Some(project) = db
                    .projects
                    .iter_mut()
                    .find(|project| project.id == run.project)
                {
                    project.remember_message(message);
                }
            }
            Ok(workspaces)
        })?;
        for workspace in workspaces {
            let _ = fs::remove_dir_all(store.root.join(format!("work-{workspace}")));
        }
        Ok(store)
    }

    pub fn record_reviewed_shot(&self, run_id: Uuid, source: &Source, work: &Path) -> Result<()> {
        let workspace = work
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("work-"))
            .and_then(|id| id.parse::<Uuid>().ok())
            .filter(|_| work.parent() == Some(self.root.as_path()))
            .context("The render workspace is unavailable.")?;
        self.update(|db| {
            let run = db
                .runs
                .iter()
                .find(|run| run.id == run_id)
                .context("Director run not found.")?;
            let drafting = run.pending() && !run.stop_requested;
            let recovering = db
                .render_receipt(run.project, source.id)
                .is_some_and(|receipt| receipt.run == run_id);
            anyhow::ensure!(
                drafting || recovering,
                "This render is no longer available for approval."
            );
            let reviewed = db.reviewed_shots.entry(run_id).or_default();
            let approval = ReviewedShot {
                source: source.clone(),
                workspace,
            };
            if let Some(entry) = reviewed
                .iter_mut()
                .find(|entry| entry.source.id == source.id)
            {
                *entry = approval;
            } else {
                reviewed.push(approval);
            }
            if drafting {
                db.finish_render_receipt(source.id);
            }
            Ok(())
        })
    }

    fn recover_reviewed_shots(&self, db: &mut Database) -> Result<HashSet<Uuid>> {
        let mut workspaces = HashSet::new();
        for (run_id, shots) in std::mem::take(&mut db.reviewed_shots) {
            workspaces.extend(shots.iter().map(|shot| shot.workspace));
            let Some(run_index) = db.runs.iter().position(|run| {
                run.id == run_id && !run.stop_requested && (!run.pending() || !run.applied)
            }) else {
                continue;
            };
            let project_id = db.runs[run_index].project;
            let archived = !db.runs[run_index].pending();
            let Some(project_index) = db
                .projects
                .iter()
                .position(|project| project.id == project_id)
            else {
                continue;
            };
            let mut recovered = 0;
            for ReviewedShot { source, workspace } in shots {
                let id = source.id;
                if archived
                    && !db
                        .render_receipt(project_id, id)
                        .is_some_and(|receipt| receipt.run == run_id)
                {
                    continue;
                }
                if !archived && db.projects[project_index].source(id).is_some() {
                    continue;
                }
                let target = self.media(id, true);
                if !target.is_file() {
                    let staged = self
                        .root
                        .join(format!("work-{workspace}"))
                        .join(format!("{id}-visible.mp4"));
                    match fs::rename(staged, &target) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(error).context("Restoring a reviewed render to Media");
                        }
                    }
                }
                if archived {
                    db.publish_recovered_shot(project_id, source)?;
                    continue;
                }
                let project = &mut db.projects[project_index];
                project.sources.push(source);
                project.library_revision += 1;
                let assets = &mut db.runs[run_index].studio_turn_mut().asset_ids;
                if !assets.contains(&id) {
                    assets.push(id);
                }
                recovered += 1;
            }
            if recovered > 0 {
                let run = &mut db.runs[run_index];
                let event = format!("Recovered {recovered} reviewed shots in Media");
                if run.events.len() >= crate::studio_history::MAX_EVENTS {
                    run.events.remove(0);
                }
                run.events.push(event);
            }
        }
        Ok(workspaces)
    }

    pub fn read<T>(&self, f: impl FnOnce(&Database) -> T) -> T {
        f(&self.db.lock().expect("studio database"))
    }

    pub fn update<T>(&self, f: impl FnOnce(&mut Database) -> Result<T>) -> Result<T> {
        self.write(|current, next| {
            let result = f(next)?;
            if !next.autosave {
                next.checkpoints
                    .retain(|id, _| next.projects.iter().any(|p| p.id == *id));
                for project in &next.projects {
                    if !next.checkpoints.contains_key(&project.id)
                        && let Some(saved) = current.projects.iter().find(|p| p.id == project.id)
                        && saved != project
                    {
                        next.checkpoints.insert(project.id, saved.clone());
                    }
                }
            }
            Ok(result)
        })
    }

    pub fn set_autosave(&self, enabled: bool) -> Result<()> {
        self.write(|_, next| {
            next.autosave = enabled;
            if enabled {
                next.checkpoints.clear();
            }
            Ok(())
        })
    }

    pub fn save_project(&self, id: Uuid) -> Result<()> {
        self.write(|_, next| {
            anyhow::ensure!(
                next.projects.iter().any(|p| p.id == id),
                "Project not found."
            );
            next.checkpoints.remove(&id);
            Ok(())
        })
    }

    pub fn revert_project(&self, id: Uuid, revision: u64, library_revision: u64) -> Result<()> {
        self.write(|_, next| {
            anyhow::ensure!(
                !next
                    .runs
                    .iter()
                    .any(|run| run.project == id && run.pending()),
                "Wait for Director to finish before reverting the project."
            );
            let project = next
                .projects
                .iter_mut()
                .find(|p| p.id == id)
                .context("Project not found.")?;
            anyhow::ensure!(
                project.revision == revision && project.library_revision == library_revision,
                "The project changed. Review the current version before reverting."
            );
            let Some(mut saved) = next.checkpoints.remove(&id) else {
                return Ok(());
            };
            saved.revision = project.revision + 1;
            saved.library_revision = project.library_revision + 1;
            *project = saved;
            Ok(())
        })
    }

    fn write<T>(&self, f: impl FnOnce(&Database, &mut Database) -> Result<T>) -> Result<T> {
        let mut current = self.db.lock().expect("studio database");
        let mut next = current.clone();
        let result = f(&current, &mut next)?;
        private_write(&self.root.join("studio.json"), &serde_json::to_vec(&next)?)?;
        *current = next;
        Ok(result)
    }

    pub fn lut(&self, id: Uuid) -> PathBuf {
        self.root.join("media").join(format!("{id}.cube"))
    }

    pub fn original(&self, id: Uuid) -> PathBuf {
        self.root.join("media").join(format!("{id}.original"))
    }

    pub fn media(&self, id: Uuid, video: bool) -> PathBuf {
        self.root
            .join("media")
            .join(format!("{id}.{}", if video { "mp4" } else { "m4a" }))
    }
}

pub fn private_write(path: &Path, data: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
