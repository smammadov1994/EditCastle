use crate::{
    clock,
    editor::{Clip, ClipKind, Project, Store},
    speech::{self, Transcript},
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptionTrack {
    pub hidden: bool,
    pub size: f64,
    pub position: f64,
    pub highlight: bool,
    pub text_color: String,
    pub highlight_color: String,
    pub outline: bool,
    pub outline_color: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clip_ids: Vec<Uuid>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<CaptionEdit>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub fixes: HashMap<Uuid, Vec<Fix>>,
}

impl Default for CaptionTrack {
    fn default() -> Self {
        Self {
            hidden: false,
            size: 64.0,
            position: 0.78,
            highlight: true,
            text_color: "#ffffff".into(),
            highlight_color: "#ffd640".into(),
            outline: true,
            outline_color: "#000000".into(),
            clip_ids: vec![],
            edits: vec![],
            fixes: HashMap::new(),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptionEdit {
    pub clip: Uuid,
    pub source_frame: u32,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Uuid>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Fix {
    pub start: u32,
    pub text: String,
    #[serde(default)]
    pub merged: bool,
}

#[derive(Clone, Serialize)]
pub struct Word {
    pub start: u32,
    pub end: u32,
    pub text: String,
    source: u32,
    #[serde(skip)]
    covers: Vec<u32>,
}

#[derive(Clone, Serialize)]
pub struct Block {
    pub id: String,
    pub clip: Uuid,
    pub start: u32,
    pub end: u32,
    pub text: String,
    pub words: Vec<Word>,
    #[serde(skip)]
    source: Uuid,
}

pub fn migrate(timeline: &mut [Clip], track: &mut Option<CaptionTrack>) {
    let Some(track) = track else { return };
    for c in timeline.iter_mut() {
        c.captioned |= track.clip_ids.contains(&c.id);
    }
    track.clip_ids.clear();
    for edit in &mut track.edits {
        if edit.source.is_none() {
            edit.source = timeline
                .iter()
                .find(|c| c.id == edit.clip)
                .map(|c| c.source);
        }
    }
}

pub fn targets(project: &Project, selected: &HashSet<Uuid>) -> HashSet<Uuid> {
    let links: HashSet<_> = project
        .timeline
        .iter()
        .filter(|c| selected.contains(&c.id))
        .filter_map(|c| c.link)
        .collect();
    let related =
        |c: &Clip| selected.contains(&c.id) || c.link.is_some_and(|id| links.contains(&id));
    project
        .timeline
        .iter()
        .filter(|c| {
            related(c)
                && !c.effects.freeze
                && project.source(c.source).is_some_and(|a| a.audio)
                && (c.kind == ClipKind::Audio
                    || c.kind == ClipKind::Video
                        && !project.timeline.iter().any(|sound| {
                            sound.kind == ClipKind::Audio
                                && c.link.is_some()
                                && c.link == sound.link
                        }))
        })
        .map(|c| c.id)
        .collect()
}

pub fn default_targets(project: &Project) -> HashSet<Uuid> {
    let voices = project
        .sequence
        .tracks
        .iter()
        .filter_map(|t| t.duck_under)
        .collect::<HashSet<_>>();
    let ids = project
        .timeline
        .iter()
        .filter(|c| {
            project.source(c.source).is_some_and(|a| a.video)
                || project
                    .sequence
                    .tracks
                    .iter()
                    .any(|t| t.name == c.track() && voices.contains(&t.id))
        })
        .map(|c| c.id)
        .collect();
    targets(project, &ids)
}

pub fn apply_fix(track: &mut CaptionTrack, block: &Block, text: &str) {
    let mut seen = HashSet::new();
    let covered = block
        .words
        .iter()
        .flat_map(|w| w.covers.iter().copied())
        .filter(|start| seen.insert(*start))
        .collect::<Vec<_>>();
    write_fixes(track.fixes.entry(block.source).or_default(), &covered, text);
}

fn write_fixes(fixes: &mut Vec<Fix>, covered: &[u32], text: &str) {
    let words = text.split_whitespace().collect::<Vec<_>>();
    let combined = words.join(" ");
    let one_for_one = words.len() == covered.len();
    for (index, start) in covered.iter().copied().enumerate() {
        fixes.retain(|f| start.abs_diff(f.start) >= 120);
        let word = if one_for_one {
            words[index]
        } else if index == 0 {
            &combined
        } else {
            ""
        };
        fixes.push(Fix {
            start,
            text: word.into(),
            merged: !one_for_one && !words.is_empty() && index != 0,
        });
    }
    fixes.sort_by_key(|fix| fix.start);
}

pub fn blocks(project: &Project, store: &Store) -> Vec<Block> {
    let Some(track) = &project.sequence.captions else {
        return vec![];
    };
    let mut scripts = HashMap::new();
    let mut out = Vec::new();
    for clip in project
        .timeline
        .iter()
        .filter(|c| c.captioned && !c.effects.freeze && !c.effects.reverse)
    {
        let Some(source) = project.source(clip.source) else {
            continue;
        };
        let script = scripts
            .entry(clip.source)
            .or_insert_with(|| speech::cached(store, source));
        if let Some(script) = script {
            out.extend(layout_clip(
                clip,
                project.placement(clip.id).unwrap_or(0),
                script,
                track,
            ));
        }
    }
    out.sort_by_key(|b| b.start);
    for index in 1..out.len() {
        let next = out[index].start;
        let b = &mut out[index - 1];
        if b.end > next {
            b.end = next.max(b.start + clock::ticks(0.05));
        }
    }
    out
}

fn layout_clip(clip: &Clip, base: u32, script: &Transcript, track: &CaptionTrack) -> Vec<Block> {
    let raw = if script.words.is_empty() {
        &script.lines
    } else {
        &script.words
    };
    let mut fixes = track
        .fixes
        .get(&clip.source)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut legacy = Vec::new();
    for edit in track
        .edits
        .iter()
        .filter(|e| e.source == Some(clip.source) || e.clip == clip.id)
    {
        if let Some(line) = script
            .lines
            .iter()
            .find(|line| line.start == edit.source_frame)
        {
            let covered = raw
                .iter()
                .filter(|word| {
                    midpoint(word.start, word.end) >= line.start
                        && midpoint(word.start, word.end) < line.end
                })
                .map(|word| word.start)
                .collect::<Vec<_>>();
            write_fixes(&mut legacy, &covered, &edit.text);
        }
    }
    if !legacy.is_empty() {
        legacy.retain(|change| !fixes.iter().any(|f| change.start.abs_diff(f.start) < 120));
        legacy.extend_from_slice(fixes);
        legacy.sort_by_key(|fix| fix.start);
        fixes = &legacy;
    }
    let duration = clip.duration();
    let at = |tick| base + clip.elapsed_at_source(tick).clamp(0, i64::from(duration)) as u32;
    let mut entries = Vec::<Word>::new();
    let mut previous = None;
    for word in raw {
        let mid = midpoint(word.start, word.end);
        if mid < clip.start || mid >= clip.end {
            continue;
        }
        let a = at(word.start.max(clip.start));
        let b = at(word.end.min(clip.end));
        let fix = matching_fix(fixes, word.start);
        if fix.is_some_and(|f| f.merged) {
            if let Some(last) = entries
                .last_mut()
                .filter(|last| last.covers.last().copied() == previous)
            {
                last.end = last.end.max(b);
                last.covers.push(word.start);
            }
        } else if a < b {
            let text = fix.map_or(word.text.as_str(), |f| &f.text).trim();
            if !text.is_empty() {
                entries.push(Word {
                    start: a,
                    end: b,
                    text: text.to_owned(),
                    source: word.start,
                    covers: vec![word.start],
                });
            }
        }
        previous = Some(word.start);
    }
    let heard = entries
        .into_iter()
        .flat_map(expand_words)
        .collect::<Vec<_>>();
    let mut phrases = Vec::new();
    for run in heard.chunk_by(|a, b| {
        b.start.saturating_sub(a.end) < clock::ticks(0.5) && !a.text.ends_with(['.', '?', '!'])
    }) {
        phrases.extend(partition(run));
    }
    phrases
        .iter()
        .enumerate()
        .map(|(index, words)| {
            let first = &words[0];
            let last = words.last().unwrap();
            let end = base + duration;
            let next = phrases.get(index + 1).map_or(end, |next| next[0].start);
            let stop = next
                .min(end)
                .min((last.end + clock::ticks(0.25)).max(first.start + clock::ticks(0.6)))
                .max(last.end);
            Block {
                id: format!("{}@{}", clip.id, first.source),
                clip: clip.id,
                source: clip.source,
                start: first.start,
                end: stop,
                text: words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                words: words.to_vec(),
            }
        })
        .collect()
}

fn midpoint(first: u32, last: u32) -> u32 {
    ((u64::from(first) + u64::from(last)) / 2) as u32
}

fn matching_fix(fixes: &[Fix], start: u32) -> Option<&Fix> {
    let index = fixes.partition_point(|fix| fix.start.saturating_add(119) < start);
    fixes
        .get(index)
        .filter(|fix| fix.start.abs_diff(start) < 120)
}

fn expand_words(entry: Word) -> Vec<Word> {
    let parts = entry.text.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 2 {
        return vec![entry];
    }
    let size = |text: &str| text.chars().count() as u64 + 1;
    let total = parts.iter().map(|w| size(w)).sum::<u64>();
    let span = u64::from(entry.end - entry.start);
    let mut before = 0;
    parts
        .into_iter()
        .map(|word| {
            let at = entry.start + ((before * span + total / 2) / total) as u32;
            before += size(word);
            let end = entry.start + ((before * span + total / 2) / total) as u32;
            Word {
                start: at,
                end,
                text: word.into(),
                source: entry.source,
                covers: entry.covers.clone(),
            }
        })
        .collect()
}

fn partition(run: &[Word]) -> Vec<&[Word]> {
    let mut total = 0;
    let ends = run
        .iter()
        .enumerate()
        .map(|(i, w)| {
            total += w.text.chars().count() + usize::from(i > 0);
            total
        })
        .collect::<Vec<_>>();
    let duration = run.last().unwrap().end - run[0].start;
    let count = total
        .div_ceil(26)
        .max(duration.div_ceil(clock::ticks(2.6)) as usize)
        .clamp(1, run.len());
    let mut out = Vec::new();
    let mut from = 0;
    for k in 1..count {
        let target = total as f64 * k as f64 / count as f64;
        let score = |i: usize| {
            (ends[i] as f64 - target).abs() - if run[i].text.ends_with(',') { 6.0 } else { 0.0 }
        };
        let best = (from..run.len() - (count - k))
            .filter(|i| run[*i].source != run[*i + 1].source)
            .min_by(|a, b| score(*a).total_cmp(&score(*b)));
        let Some(i) = best else { break };
        out.push(&run[from..=i]);
        from = i + 1;
    }
    out.push(&run[from..]);
    out
}
