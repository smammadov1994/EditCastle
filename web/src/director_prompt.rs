use crate::{
    clock,
    editor::{ClipKind, Project},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Media,
    Clip,
    Marker,
}

#[derive(Serialize)]
struct Target<'a> {
    kind: Kind,
    id: Uuid,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_kind: Option<ClipKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    track: Option<&'a str>,
    at: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    end: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<&'a str>,
}

fn label(seen: &mut HashMap<String, usize>, name: &str) -> String {
    let occurrence = seen.entry(name.to_lowercase()).or_default();
    *occurrence += 1;
    if *occurrence == 1 {
        name.to_owned()
    } else {
        format!("{name}·{occurrence}")
    }
}

fn clips(project: &Project) -> Vec<(&crate::editor::Clip, u32)> {
    let track_order = project
        .sequence
        .tracks
        .iter()
        .rev()
        .filter(|track| track.name.starts_with('V'))
        .chain(
            project
                .sequence
                .tracks
                .iter()
                .filter(|track| track.name.starts_with('A')),
        )
        .enumerate()
        .map(|(index, track)| (track.name.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut ends = HashMap::<&str, u32>::new();
    let mut ordered = Vec::with_capacity(project.timeline.len());
    for clip in &project.timeline {
        let end = ends.entry(clip.track()).or_default();
        let at = clip.position.unwrap_or(*end);
        *end = (*end).max(at.saturating_add(clip.duration()));
        ordered.push((clip, at));
    }
    ordered.sort_by_key(|(clip, at)| (*at, track_order.get(clip.track()).copied()));
    ordered
}

fn catalog<'a>(
    project: &'a Project,
    ordered: &[(&'a crate::editor::Clip, u32)],
) -> Vec<Target<'a>> {
    let mut names = HashMap::new();
    let mut targets = project
        .sources
        .iter()
        .map(|source| Target {
            kind: Kind::Media,
            id: source.id,
            name: label(&mut names, &source.title),
            media_kind: Some(if source.image {
                ClipKind::Image
            } else if source.video {
                ClipKind::Video
            } else {
                ClipKind::Audio
            }),
            track: None,
            at: source.duration,
            end: None,
            color: None,
        })
        .collect::<Vec<_>>();

    names.clear();
    let video_links = ordered
        .iter()
        .filter(|(clip, _)| clip.kind != ClipKind::Audio)
        .filter_map(|(clip, _)| clip.link)
        .collect::<HashSet<_>>();
    for &(clip, at) in ordered {
        if clip.kind == ClipKind::Audio && clip.link.is_some_and(|link| video_links.contains(&link))
        {
            continue;
        }
        let name = clip.name(project).trim();
        let name = if clip.kind == ClipKind::Text && name.chars().count() > 24 {
            let short: String = name.chars().take(22).collect();
            format!("{}…", short.trim_end())
        } else {
            name.to_owned()
        };
        targets.push(Target {
            kind: Kind::Clip,
            id: clip.id,
            name: label(&mut names, &name),
            media_kind: Some(clip.kind),
            track: Some(clip.track()),
            at,
            end: Some(at.saturating_add(clip.duration())),
            color: None,
        });
    }

    names.clear();
    let mut markers = project.sequence.markers.iter().collect::<Vec<_>>();
    markers.sort_by_key(|marker| marker.frame);
    for marker in markers {
        let name = if marker.name.trim().is_empty() {
            let mut color = marker.color.chars();
            let capital = color
                .next()
                .map(|first| first.to_uppercase().to_string())
                .unwrap_or_default();
            format!("{capital}{} marker", color.as_str())
        } else {
            marker.name.trim().to_owned()
        };
        targets.push(Target {
            kind: Kind::Marker,
            id: marker.id,
            name: label(&mut names, &name),
            media_kind: None,
            track: None,
            at: marker.frame,
            end: None,
            color: Some(&marker.color),
        });
    }
    targets
}

pub fn suggestions(project: &Project) -> Value {
    json!(catalog(project, &clips(project)))
}

#[derive(Clone, Copy)]
enum Mention<'a> {
    Playhead,
    Selected,
    Range,
    Item(&'a Target<'a>),
}

fn word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn matching_prefix(text: &str, name: &str) -> Option<usize> {
    let width = name.chars().count();
    let end = text
        .char_indices()
        .nth(width)
        .map_or(text.len(), |(i, _)| i);
    (text[..end].eq_ignore_ascii_case(name) || text[..end].to_lowercase() == name.to_lowercase())
        .then_some(end)
}

fn quoted_name(text: &str) -> Option<(String, usize)> {
    let rest = text.strip_prefix('"')?;
    let mut escaped = false;
    for (i, ch) in rest.char_indices() {
        if ch == '"' && !escaped {
            let length = i + 2;
            return serde_json::from_str(&text[..length])
                .ok()
                .map(|name| (name, length));
        }
        escaped = ch == '\\' && !escaped;
    }
    None
}

fn find<'a>(prompt: &str, catalog: &'a [Target<'a>]) -> Vec<(usize, usize, Mention<'a>)> {
    use Mention::{Item, Playhead, Range, Selected};
    let mut choices = vec![
        ("playhead", Playhead),
        ("selected", Selected),
        ("in-out", Range),
    ];
    for target in catalog {
        choices.push((&target.name, Item(target)));
        if target.kind == Kind::Media
            && let Some(stem) = Path::new(&target.name)
                .file_stem()
                .and_then(|part| part.to_str())
            && stem != target.name
            && !target
                .name
                .rsplit_once('·')
                .is_some_and(|(_, suffix)| suffix.parse::<usize>().is_ok())
        {
            choices.push((stem, Item(target)));
        }
    }
    choices.sort_by_key(|(name, _)| std::cmp::Reverse(name.chars().count()));
    let mut matches = vec![];
    let mut resume = 0;
    for (at, _) in prompt.match_indices('@') {
        if at < resume || prompt[..at].chars().next_back().is_some_and(word) {
            continue;
        }
        let mut start = at + 1;
        let mut kind = None;
        for (prefix, group) in [
            ("clip:", Kind::Clip),
            ("marker:", Kind::Marker),
            ("media:", Kind::Media),
        ] {
            if let Some(end) = matching_prefix(&prompt[start..], prefix) {
                start += end;
                kind = Some(group);
                break;
            }
        }
        let tail = &prompt[start..];
        let quoted = quoted_name(tail);
        for &(name, mention) in &choices {
            if name.is_empty()
                || kind.is_some_and(|kind| !matches!(mention, Item(target) if target.kind == kind))
            {
                continue;
            }
            let length = if let Some((quoted, length)) = &quoted {
                if quoted.to_lowercase() != name.to_lowercase() {
                    continue;
                }
                *length
            } else if let Some(length) = matching_prefix(tail, name) {
                if tail[length..].chars().next().is_some_and(word) {
                    continue;
                }
                length
            } else {
                continue;
            };
            resume = start + length;
            matches.push((at, resume, mention));
            break;
        }
    }
    matches
}

fn clock_text(at: u32) -> String {
    let hundredths = (clock::seconds(at) * 100.0).round() as u32;
    let (hours, minutes, seconds, fraction) = (
        hundredths / 360_000,
        hundredths / 6_000 % 60,
        hundredths / 100 % 60,
        hundredths % 100,
    );
    let leading = if hours > 0 {
        format!("{hours}:{minutes:02}")
    } else {
        minutes.to_string()
    };
    let ending = format!("{seconds:02}.{fraction:02}");
    format!(
        "{leading}:{}",
        ending.trim_end_matches('0').trim_end_matches('.')
    )
}

struct TypedTime<'a> {
    start: usize,
    end: usize,
    input: &'a str,
    seconds: f64,
    units: bool,
}

fn number_len(text: &str) -> usize {
    text.bytes().take_while(u8::is_ascii_digit).count()
}

fn read_time(text: &str, fps: f64) -> Option<(usize, f64, bool)> {
    let mut end = number_len(text);
    let mut parts = vec![&text[..end]];
    while parts.len() < 4
        && text
            .as_bytes()
            .get(end)
            .is_some_and(|ch| *ch == b':' || (*ch == b';' && parts.len() == 3))
    {
        let next = number_len(&text[end + 1..]);
        if next == 0 {
            break;
        }
        parts.push(&text[end + 1..end + 1 + next]);
        end += 1 + next;
    }
    if parts.len() < 4 && text.as_bytes().get(end) == Some(&b'.') {
        let fraction = number_len(&text[end + 1..]);
        if fraction > 0 {
            let first = end - parts.last()?.len();
            end += 1 + fraction;
            *parts.last_mut()? = &text[first..end];
        }
    }
    let values = parts
        .iter()
        .map(|part| part.parse::<f64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let seconds = match values.as_slice() {
        [seconds] => {
            let rest = text[end..].trim_start();
            let unit = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
            if !matches!(
                rest[..unit].to_ascii_lowercase().as_str(),
                "s" | "sec" | "secs" | "second" | "seconds"
            ) || rest[unit..].chars().next().is_some_and(word)
            {
                return None;
            }
            return Some((text.len() - rest.len() + unit, *seconds, true));
        }
        [minutes, seconds] if parts[0].len() <= 3 && parts[1].split('.').next()?.len() == 2 => {
            minutes * 60.0 + seconds
        }
        [hours, minutes, seconds]
            if parts[0].len() <= 2
                && parts[1].len() == 2
                && parts[2].split('.').next()?.len() == 2 =>
        {
            hours * 3600.0 + minutes * 60.0 + seconds
        }
        [hours, minutes, seconds, frames]
            if parts[0].len() <= 2
                && parts[1].len() == 2
                && parts[2].len() == 2
                && (2..=3).contains(&parts[3].len()) =>
        {
            hours * 3600.0 + minutes * 60.0 + seconds + frames / fps.max(1.0)
        }
        _ => return None,
    };
    if text
        .as_bytes()
        .get(end)
        .is_some_and(|ch| ch.is_ascii_digit() || *ch == b':')
    {
        return None;
    }
    Some((end, seconds, false))
}

fn typed_times(text: &str, fps: f64) -> Vec<TypedTime<'_>> {
    let mut times = vec![];
    let mut resume = 0;
    for (start, ch) in text.char_indices() {
        if start < resume
            || !ch.is_ascii_digit()
            || text
                .as_bytes()
                .get(start.wrapping_sub(1))
                .is_some_and(|ch| ch.is_ascii_digit() || matches!(ch, b':' | b'.'))
        {
            continue;
        }
        if let Some((len, seconds, units)) = read_time(&text[start..], fps) {
            resume = start + len;
            times.push(TypedTime {
                start,
                end: resume,
                input: &text[start..resume],
                seconds,
                units,
            });
        }
    }
    times
}

fn clip_context(clip: &crate::editor::Clip, at: u32, project: &Project) -> Value {
    json!({"clip_id":clip.id,"name":clip.name(project),"track":clip.track(),
        "start":clock::seconds(at),"end":clock::seconds(at.saturating_add(clip.duration()))})
}

pub fn context(project: &Project, prompt: &str, at: u32, selection: &[Uuid]) -> Value {
    let ordered = clips(project);
    let catalog = catalog(project, &ordered);
    let at_seconds = clock::seconds(at);
    let play_range = project
        .play_range()
        .map(|(start, end)| json!({"in":clock::seconds(start),"out":clock::seconds(end)}));
    let selected = ordered
        .iter()
        .filter(|(clip, _)| selection.contains(&clip.id))
        .map(|(clip, at)| clip_context(clip, *at, project))
        .collect::<Vec<_>>();
    let picture = |start: f64, end: f64| {
        let mut visible = ordered
            .iter()
            .filter(|(clip, at)| {
                let clip_start = clock::seconds(*at);
                clip.kind != ClipKind::Audio
                    && (if end == start {
                        clip_start <= start
                    } else {
                        clip_start < end
                    })
                    && clock::seconds(at.saturating_add(clip.duration())) > start
            })
            .collect::<Vec<_>>();
        if end == start {
            // At a single time, show the top picture first.
            visible.sort_by_key(|(clip, _)| {
                std::cmp::Reverse(clip.track().get(1..).and_then(|n| n.parse::<u32>().ok()))
            });
        }
        visible
            .into_iter()
            .map(|(clip, at)| clip_context(clip, *at, project))
            .collect::<Vec<_>>()
    };
    let under_playhead = picture(at_seconds, at_seconds);
    let mut named = vec![];
    let mut clock_prompt = String::new();
    let mut marker_times = HashMap::new();
    let mut last = 0;
    for (begin, end, mention) in find(prompt, &catalog) {
        let text = &prompt[begin..end];
        clock_prompt.push_str(&prompt[last..begin]);
        let data = match mention {
            Mention::Playhead => {
                json!({"text":text,"kind":"playhead","seconds":at_seconds,"picture":under_playhead})
            }
            Mention::Selected => json!({"text":text,"kind":"selected","clips":selected}),
            Mention::Range => json!({"text":text,"kind":"in_out","range":play_range}),
            Mention::Item(target) => match target.kind {
                Kind::Media => json!({"text":text,"kind":"media","asset_id":target.id,
                    "name":target.name,"media_kind":target.media_kind}),
                Kind::Clip => {
                    json!({"text":text,"kind":"clip","clip_id":target.id,"name":target.name,
                    "track":target.track,"start":clock::seconds(target.at),"end":target.end.map(clock::seconds)})
                }
                Kind::Marker => {
                    marker_times.insert(clock_prompt.len(), clock::seconds(target.at));
                    clock_prompt.push_str(&clock_text(target.at));
                    json!({"text":text,"kind":"marker","marker_id":target.id,"name":target.name,
                        "seconds":clock::seconds(target.at)})
                }
            },
        };
        if !matches!(mention, Mention::Item(target) if target.kind == Kind::Marker) {
            clock_prompt.push_str(&" ".repeat(text.len()));
        }
        named.push(data);
        last = end;
    }
    clock_prompt.push_str(&prompt[last..]);
    let mut times = typed_times(&clock_prompt, project.sequence.fps);
    for time in &mut times {
        if let Some(seconds) = marker_times.get(&time.start) {
            time.seconds = *seconds;
        }
    }
    let range = times.windows(2).find_map(|pair| {
        let (a, b) = (&pair[0], &pair[1]);
        let gap = clock_prompt[a.end..b.start].trim().to_ascii_lowercase();
        let before = clock_prompt[..a.start].to_ascii_lowercase();
        if !matches!(
            gap.as_str(),
            "and" | "to" | "-" | "–" | "—" | "through" | "until" | "till"
        ) || b.seconds <= a.seconds
        {
            return None;
        }
        let prefix = if before.ends_with("between ") {
            8
        } else if before.ends_with("from ") {
            5
        } else {
            0
        };
        if prefix == 0 && (a.units || b.units) {
            return None;
        }
        Some((
            a.start,
            b.end,
            json!({"text":clock_prompt[a.start-prefix..b.end].trim(),
            "start":a.seconds,"end":b.seconds,"picture":picture(a.seconds,b.seconds)}),
        ))
    });
    let instants = times
        .iter()
        .filter(|time| {
            !time.units
                && range
                    .as_ref()
                    .is_none_or(|(from, to, _)| !(time.start >= *from && time.end <= *to))
        })
        .map(|time| {
            json!({"text":time.input,"seconds":time.seconds,
            "picture":picture(time.seconds,time.seconds)})
        })
        .collect::<Vec<_>>();
    json!({"playhead_seconds":at_seconds,"picture_at_playhead":under_playhead,"selected_clips":selected,
        "in_out":play_range,"references":named,"typed_range":range.map(|(_,_,value)| value),
        "typed_times":instants})
}
