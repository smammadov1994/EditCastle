use crate::editor::{Clip, ClipKind, Project};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

#[derive(Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Dissolve,
    DipToBlack,
    Wipe,
}

impl Kind {
    pub fn read(raw: &str) -> Option<Self> {
        match raw {
            "dissolve" => Some(Self::Dissolve),
            "dipToBlack" => Some(Self::DipToBlack),
            "wipe" => Some(Self::Wipe),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
pub struct Edge {
    pub start: u32,
    pub end: u32,
    pub kind: Kind,
}

#[derive(Clone, Copy, Default, Serialize)]
pub struct Plan {
    pub lead_in: u32,
    pub tail_out: u32,
    pub reveal: Option<Edge>,
    pub conceal: Option<Edge>,
}

impl Plan {
    pub fn render_start(self, at: u32) -> u32 {
        at.saturating_sub(self.lead_in)
    }

    pub fn render_end(self, at: u32, duration: u32) -> u32 {
        at.saturating_add(duration).saturating_add(self.tail_out)
    }

    pub fn extends(self) -> bool {
        self.lead_in > 0 || self.tail_out > 0
    }

    pub fn source_range(self, clip: &Clip, project: &Project) -> (u32, u32) {
        if !self.extends() || !uses_source(clip) {
            return (clip.start, clip.end);
        }
        let limit = project
            .source(clip.source)
            .map_or(clip.end, |asset| asset.duration);
        let head = clip.source_at(-i64::from(self.lead_in)).min(limit);
        let tail = clip
            .source_at(i64::from(clip.duration()) + i64::from(self.tail_out))
            .min(limit);
        (head.min(tail), head.max(tail))
    }
}

#[derive(Clone, Serialize)]
pub struct Cut {
    pub previous: Uuid,
    pub incoming: Uuid,
    pub at: u32,
    pub kind: Option<Kind>,
    pub requested: u32,
    pub span: Option<Edge>,
    pub from_head: bool,
}

#[derive(Default)]
pub struct Layout {
    pub plans: HashMap<Uuid, Plan>,
    pub cuts: Vec<Cut>,
}

impl Layout {
    pub fn plan(&self, clip: Uuid) -> Plan {
        self.plans.get(&clip).copied().unwrap_or_default()
    }
}

fn uses_source(clip: &Clip) -> bool {
    clip.kind == ClipKind::Video && !clip.effects.freeze
}

fn head_room(clip: &Clip, project: &Project) -> u32 {
    if !uses_source(clip) {
        return u32::MAX;
    }
    let limit = project
        .source(clip.source)
        .map_or(clip.end, |asset| asset.duration);
    let before = if clip.effects.reverse { limit } else { 0 };
    (-clip.elapsed_at_source(before)).max(0) as u32
}

fn tail_room(clip: &Clip, project: &Project) -> u32 {
    if !uses_source(clip) {
        return u32::MAX;
    }
    let Some(source) = project.source(clip.source) else {
        return 0;
    };
    let after = if clip.effects.reverse {
        0
    } else {
        source.duration
    };
    (clip.elapsed_at_source(after) - i64::from(clip.duration())).max(0) as u32
}

fn span(
    project: &Project,
    before: &Clip,
    into: &Clip,
    at: u32,
    kind: Kind,
) -> Option<(Edge, bool)> {
    let a = before.duration();
    let b = into.duration();
    let want = into.effects.transition_frames;
    if kind == Kind::DipToBlack {
        let duration = want.min(a.saturating_mul(2)).min(b.saturating_mul(2));
        return (duration >= project.frame_step()).then_some((
            Edge {
                start: at.saturating_sub(duration / 2),
                end: at.saturating_add(duration - duration / 2),
                kind,
            },
            false,
        ));
    }
    let want = want.min(a).min(b);
    if want == 0 {
        return None;
    }
    let head = head_room(into, project);
    let tail = tail_room(before, project);
    let (duration, from_head) = if head >= want {
        (want, true)
    } else if tail >= want {
        (want, false)
    } else if head.max(tail) >= project.frame_step() {
        (head.max(tail), head >= tail)
    } else {
        return None;
    };
    let (start, end) = if from_head {
        (at.saturating_sub(duration), at)
    } else {
        (at, at.saturating_add(duration))
    };
    (start < end).then_some((Edge { start, end, kind }, from_head))
}

pub fn layout(project: &Project) -> Layout {
    let mut tracks = BTreeMap::<&str, Vec<(u32, &Clip)>>::new();
    for c in project
        .timeline
        .iter()
        .filter(|c| c.kind != ClipKind::Audio)
    {
        tracks
            .entry(c.track())
            .or_default()
            .push((project.placement(c.id).unwrap_or(0), c));
    }
    let mut output = Layout::default();
    let tolerance = project.frame_step() / 2;
    for track in tracks.values_mut() {
        track.sort_by_key(|(start, _)| *start);
        for pair in track.windows(2) {
            let [(first, a), (at, b)] = pair else {
                continue;
            };
            let end = first.saturating_add(a.duration());
            if end.abs_diff(*at) > tolerance {
                continue;
            }
            let kind = b.effects.transition.as_deref().and_then(Kind::read);
            let (edge, from_head) = kind
                .and_then(|kind| span(project, a, b, *at, kind))
                .map_or((None, false), |(edge, head)| (Some(edge), head));
            output.cuts.push(Cut {
                previous: a.id,
                incoming: b.id,
                at: *at,
                kind,
                requested: b.effects.transition_frames,
                span: edge,
                from_head,
            });
            let Some(edge) = edge else {
                continue;
            };
            let incoming = output.plans.entry(b.id).or_default();
            incoming.reveal = Some(if edge.kind == Kind::DipToBlack {
                Edge { start: *at, ..edge }
            } else {
                edge
            });
            if from_head {
                incoming.lead_in = at.saturating_sub(edge.start);
            }
            let outgoing = output.plans.entry(a.id).or_default();
            if edge.kind == Kind::DipToBlack {
                outgoing.conceal = Some(Edge { end, ..edge });
            } else if !from_head {
                outgoing.tail_out = edge.end.saturating_sub(end);
            }
        }
    }
    output
}
