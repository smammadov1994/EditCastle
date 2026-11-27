use crate::{
    clock, director_prompt,
    editor::{ClipKind, Project, Store},
};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::LazyLock,
};
use uuid::Uuid;

static LENGTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(\d+(?:\.\d+)?|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|fifteen|twenty|thirty|forty|fifty|sixty)[\s-]*(seconds?|secs?|s|minutes?|mins?|m)\b").unwrap()
});
static COLON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{1,3}):([0-5]\d)\b").unwrap());
static COUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(\d+|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|fifteen|twenty|thirty)\s*\+?\s*(?:(?:different|unique|separate|short|quick|fast)\s+)?(?:clips|shots|cuts|scenes|videos)\b").unwrap()
});
static EVERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:(?:all|every one of|each of)\s+(?:of\s+)?(?:(?:the|my|our)\s+)?(?:clips|videos|shots|footage|media|files)|(?:every|each)\s+(?:single\s+)?(?:clip|shot|video|file))\b").unwrap()
});
static RATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d+(?:\.\d+)?)\s*(%|x\b|times\b)").unwrap());
static QUOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""([^"]{1,120})"|“([^”]{1,120})”|‘([^’]{1,120})’|(?:^|\s)'([^']{1,120})'(?:$|[\s,.!?;:])"#).unwrap()
});
static KEEP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:only|just|solely|keep|nothing but|nothing else|except|use the|using the)\b")
        .unwrap()
});
static DROP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:remove|take out|without|delete|drop|get rid|cut out|no more|lose)\b").unwrap()
});
static MUSIC_OFF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:(?:no|without|remove|drop|mute|take out|turn off)\s+(?:the\s+)?(?:background\s+)?(?:music|soundtrack|song)|(?:music|soundtrack|song)\s+off)\b").unwrap()
});
static MUSIC_ADD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:add|include|use|with|put in|put on)\s+(?:(?:some|the|a|an)\s+)?(?:background\s+)?(?:music|soundtrack|song)\b").unwrap()
});

#[derive(Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Pace {
    #[default]
    Unchanged,
    Calm,
    Fast,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Music {
    #[default]
    Unchanged,
    Off,
    Add,
}

pub(super) struct Ramp {
    pub cuts: bool,
    pub speed: f64,
}

#[derive(Default)]
pub(super) struct Recipe {
    pub seconds: Option<f64>,
    pub pace: Pace,
    pub look: Option<String>,
    pub music: Music,
    pub frame: Option<(u32, u32)>,
    pub speed: Option<f64>,
    pub ramp: Option<Ramp>,
    pub playhead: u32,
    pub title: Option<String>,
    pub title_duration: Option<f64>,
    pub title_at_end: bool,
    pub remove_titles: bool,
    pub level_audio: bool,
    pub sound_effects: bool,
    pub judge_effects: bool,
    pub whoosh: bool,
    pub captions: Option<bool>,
    pub every: bool,
    pub whole_library: bool,
    pub shots: Option<usize>,
    pub general: bool,
    pub keep: HashSet<Uuid>,
    pub drop: HashSet<Uuid>,
    pub clear_rest: bool,
    pub clips: HashSet<Uuid>,
    pub keep_clips: bool,
    pub drop_clips: bool,
    pub missing_scope: bool,
    pub span: Option<(u32, u32)>,
    pub at: Option<u32>,
    pub moments: HashMap<Uuid, Vec<f64>>,
    pub unsupported: Vec<&'static str>,
}

fn has(text: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| text.contains(pattern))
}

fn number_word(word: &str) -> Option<usize> {
    Some(match word {
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        "fifteen" => 15,
        "twenty" => 20,
        "thirty" => 30,
        "forty" => 40,
        "fifty" => 50,
        "sixty" => 60,
        _ => word.parse().ok()?,
    })
}

fn count(word: &str) -> Option<usize> {
    number_word(word).filter(|n| (2..=200).contains(n))
}

fn time_length(text: &str) -> Option<f64> {
    let mut candidates = LENGTH.captures_iter(text);
    let number = |word: &str| {
        word.parse::<f64>()
            .ok()
            .or_else(|| number_word(word).map(|n| n as f64))
    };
    while let Some(groups) = candidates.next() {
        let hit = groups.get(0)?;
        let before = text[..hit.start()]
            .split_whitespace()
            .next_back()
            .unwrap_or("");
        let after = text[hit.end()..].trim_start();
        if ["at", "from", "between", "around"].contains(&before)
            || has(
                after.get(..after.len().min(24)).unwrap_or(after),
                &["per shot", "per clip", "each shot", "each clip"],
            )
        {
            continue;
        }
        let value = number(&groups[1])?;
        if groups[2].starts_with('m') {
            let seconds = candidates
                .next()
                .filter(|next| {
                    !next[2].starts_with('m')
                        && next.get(0).is_some_and(|next| {
                            matches!(text[hit.end()..next.start()].trim(), "" | "and")
                        })
                })
                .and_then(|next| number(&next[1]))
                .unwrap_or(0.0);
            return Some(60.0 * value + seconds);
        }
        return Some(value);
    }
    if has(text, &["duration", "long", "length", "to ", "make it "])
        && let Some(groups) = COLON.captures(text)
    {
        return Some(groups[1].parse::<f64>().ok()? * 60.0 + groups[2].parse::<f64>().ok()?);
    }
    None
}

fn playback_speed(text: &str) -> Option<f64> {
    if has(
        text,
        &[
            "slow motion",
            "slow-mo",
            "slowmo",
            "half speed",
            "half as fast",
        ],
    ) {
        return Some(0.5);
    }
    if has(text, &["double speed", "twice as fast"]) {
        return Some(2.0);
    }
    if has(text, &["normal speed", "real time", "realtime"]) {
        return Some(1.0);
    }
    if !has(
        text,
        &[
            "speed",
            "faster",
            "slower",
            "fast",
            "slow",
            "sped",
            "timelapse",
            "time lapse",
            "playback",
        ],
    ) {
        return None;
    }
    let groups = RATE.captures(text)?;
    let value = groups[1].parse::<f64>().ok()?;
    Some(
        (if &groups[2] == "%" {
            value / 100.0
        } else {
            value
        })
        .clamp(0.05, 100.0),
    )
}

fn word(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|p| !p.is_empty())
}

fn names(request: &str, project: &Project) -> HashSet<Uuid> {
    let words: Vec<_> = word(request).collect();
    project
        .sources
        .iter()
        .filter(|asset| {
            let title = asset.title.to_lowercase();
            let part = Path::new(&title)
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or(&title);
            let pieces: Vec<_> = word(part).collect();
            if part.chars().count() >= 3
                && !pieces.is_empty()
                && words.windows(pieces.len()).any(|window| window == pieces)
            {
                return true;
            }
            let numeric = part
                .rsplit(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap_or_default();
            numeric.len() >= 3 && words.contains(&numeric)
        })
        .map(|asset| asset.id)
        .collect()
}

impl Recipe {
    pub fn analyze(
        store: &Store,
        project: &Project,
        request: &str,
        at: u32,
        selection: &[Uuid],
    ) -> Self {
        let context = director_prompt::context(project, request, at, selection);
        let raw = request.to_lowercase();
        let quoted = QUOTE
            .captures(request)
            .and_then(|c| c.iter().skip(1).flatten().next())
            .map(|m| m.as_str().trim());
        let mut text = QUOTE.replace_all(&raw, " ").into_owned();
        for reference in context["references"].as_array().into_iter().flatten() {
            if let Some(reference) = reference["text"].as_str() {
                text = text.replace(&reference.to_lowercase(), " ");
            }
        }
        let mut r = Self::default();
        r.playhead = at;
        let has_cut = project
            .timeline
            .iter()
            .any(|c| matches!(c.kind, ClipKind::Video | ClipKind::Image));
        let title = has(
            &text,
            &[
                "title",
                "heading",
                "text on",
                "words on",
                "add text",
                "put text",
                "some text",
                "call it",
                "name it",
                "text saying",
                "text that says",
            ],
        ) && !has(
            &text,
            &[
                "no title",
                "without title",
                "without a title",
                "without text",
                "no text",
                "remove the title",
                "take out the title",
            ],
        );
        let captions = has(
            &text,
            &["captions", "caption the", "subtitles", "subtitle the"],
        );
        if title
            || (captions && quoted.is_some() && has(&text, &["saying", "that says", "that reads"]))
        {
            r.title = Some(
                quoted
                    .filter(|q| !q.is_empty())
                    .unwrap_or("Title")
                    .to_owned(),
            );
            r.title_at_end = has(
                &text,
                &[
                    "at the end",
                    "at end",
                    "ending",
                    "closing",
                    "outro",
                    "over the last",
                ],
            );
        } else if captions {
            r.captions = Some(!has(
                &text,
                &[
                    "no captions",
                    "no subtitles",
                    "without captions",
                    "without subtitles",
                    "remove",
                    "take out",
                    "delete",
                ],
            ));
        }
        r.remove_titles = has(
            &text,
            &[
                "remove the title",
                "remove titles",
                "take out the title",
                "delete the title",
                "no titles",
            ],
        );
        r.general = has(
            &text,
            &[
                "improve",
                "better",
                "tighten",
                "tidy",
                "clean",
                "polish",
                "fix",
                "best",
                "nicer",
                "shorter",
                "trim",
                "cut it down",
                "make it work",
                "edit it",
                "rhythm",
                "recut",
                "re-cut",
                "re-edit",
                "to the beat",
            ],
        );
        let asks_every = EVERY.is_match(&text);
        let new_cut = has(
            &text,
            &[
                "most of the clips",
                "most clips",
                "more clips",
                "more shots",
                "other clips",
                "other shots",
                "whole library",
                "all my media",
                "media folder",
                "all the media",
                "from scratch",
                "start over",
                "make a new",
                "create a new",
                "new video",
                "new cut",
                "new edit",
                "montage",
            ],
        );
        r.shots = COUNT.captures(&text).and_then(|groups| count(&groups[1]));
        r.seconds = time_length(&text).map(|seconds| seconds.clamp(0.8, 600.0));
        r.every = asks_every
            && (!has_cut
                || new_cut
                || r.seconds.is_some()
                || r.shots.is_some()
                || has(
                    &text,
                    &[
                        "use every",
                        "fit every",
                        "include every",
                        "use all",
                        "fit all",
                        "include all",
                    ],
                ));
        r.whole_library = new_cut || r.every;
        if r.title.is_some()
            && !r.whole_library
            && !r.general
            && !has(
                &text,
                &[
                    "cut length",
                    "video length",
                    "whole video",
                    "total runtime",
                    "total duration",
                ],
            )
        {
            r.title_duration = r.seconds.take();
        }
        r.speed = playback_speed(&text);
        if r.speed.is_none() {
            if has(
                &text,
                &[
                    "calm",
                    "slow paced",
                    "slow-paced",
                    "slower pace",
                    "slow down the pacing",
                    "relax",
                    "gentle",
                    "chill",
                ],
            ) {
                r.pace = Pace::Calm;
            }
            if has(
                &text,
                &[
                    "fast",
                    "quicker cuts",
                    "quick cuts",
                    "snappy",
                    "punch",
                    "energ",
                ],
            ) {
                r.pace = Pace::Fast;
            }
        }
        r.frame = if has(
            &text,
            &["9:16", "vertical", "portrait", "tiktok", "tik tok"],
        ) {
            Some((1080, 1920))
        } else if has(&text, &["16:9", "horizontal", "landscape", "widescreen"]) {
            Some((1920, 1080))
        } else if has(&text, &["1:1", "square"]) {
            Some((1080, 1080))
        } else {
            None
        };
        r.look = crate::grade::LOOKS
            .iter()
            .copied()
            .chain(["Faded"])
            .find(|look| text.contains(&look.to_lowercase()))
            .map(str::to_owned);
        if has(
            &text,
            &["black and white", "black-and-white", "monochrome", "b&w"],
        ) {
            r.look = Some("Mono".into());
        }
        if has(
            &text,
            &[
                "remove the look",
                "remove the grade",
                "no filter",
                "no look",
                "remove filters",
                "original color",
                "original colour",
                "reset the look",
            ],
        ) {
            r.look = Some("none".into());
        }
        r.music = if MUSIC_OFF.is_match(&text) {
            Music::Off
        } else if MUSIC_ADD.is_match(&text) {
            Music::Add
        } else {
            Music::Unchanged
        };
        r.level_audio = has(
            &text,
            &["audio", "sound", "volume", "loud", "level", "voice"],
        ) && has(
            &text,
            &[
                "equaliz",
                "equalis",
                "normaliz",
                "normalis",
                "even out",
                "same level",
                "same volume",
                "same loudness",
                "level out",
                "level the",
                "balance the audio",
                "match the levels",
            ],
        );
        if has(
            &text,
            &[
                "motion ramp",
                "speed ramp",
                "ramping",
                "ramp speed",
                "ramp into",
                "slow-mo moment",
                "slow motion moment",
            ],
        ) {
            let cuts = has(
                &text,
                &[
                    "cut",
                    "transition",
                    "between",
                    "into each",
                    "into the next",
                    "whip",
                ],
            );
            let fallback = if has(&text, &["fast", "speed up"]) {
                3.0
            } else {
                0.3
            };
            let speed = if cuts {
                r.speed.take().filter(|&speed| speed > 1.0).unwrap_or(4.0)
            } else {
                r.speed
                    .take()
                    .filter(|&speed| speed != 1.0)
                    .unwrap_or(fallback)
            };
            r.ramp = Some(Ramp { cuts, speed });
            r.seconds = None;
            r.pace = Pace::Unchanged;
        }
        if has(
            &text,
            &[
                "sound effects",
                "sound effect",
                "sound fx",
                "sfx",
                "whoosh",
                "foley",
                "sound design",
            ],
        ) {
            r.sound_effects = !has(
                &text,
                &[
                    "no sound effects",
                    "without sound effects",
                    "remove sound effects",
                    "no sfx",
                ],
            );
            r.judge_effects = raw.contains('?')
                || has(
                    &text,
                    &[
                        "if it fits",
                        "if needed",
                        "only if",
                        "where it fits",
                        "do you think",
                        "should we",
                        "should i",
                    ],
                );
            r.whoosh = has(&text, &["whoosh", "swoosh", "swish", "woosh"]);
        }
        if has(
            &text,
            &[
                "generate footage",
                "generate a shot",
                "generate a video",
                "make an animation",
                "create an animation",
                "generate new footage",
                "from a prompt",
                "text-to-video",
            ],
        ) {
            r.unsupported.push("new footage generation");
        }
        let mut pointed = HashSet::new();
        for reference in context["references"].as_array().into_iter().flatten() {
            let id = |key: &str| {
                reference[key]
                    .as_str()
                    .and_then(|id| id.parse::<Uuid>().ok())
            };
            match reference["kind"].as_str() {
                Some("media") => {
                    if let Some(id) = id("asset_id") {
                        pointed.insert(id);
                    }
                }
                Some("clip") => {
                    if let Some(id) = id("clip_id") {
                        r.clips.insert(id);
                    }
                }
                Some("selected") => r.clips.extend(selection.iter().copied()),
                Some("playhead") => r.at = Some(at),
                Some("in_out") => r.span = project.play_range(),
                Some("marker") => {
                    if let Some(seconds) = reference["seconds"].as_f64() {
                        r.at = Some(clock::ticks(seconds));
                    }
                }
                _ => {}
            }
        }
        if has(
            &text,
            &[
                "selected clip",
                "selected shot",
                "the selection",
                "these clips",
                "this clip",
                "this shot",
            ],
        ) {
            r.clips.extend(selection.iter().copied());
        }
        if has(&text, &["at the playhead", "here", "at the cursor"]) {
            r.at = Some(at);
        }
        if has(&text, &["in/out", "marked range", "in-out"]) {
            r.span = project.play_range();
        }
        if let (Some(from), Some(to)) = (
            context["typed_range"]["start"].as_f64(),
            context["typed_range"]["end"].as_f64(),
        ) {
            r.span = Some((clock::ticks(from), clock::ticks(to)));
        }
        if r.at.is_none()
            && text.contains(" at ")
            && let Some(seconds) = context["typed_times"][0]["seconds"].as_f64()
        {
            r.at = Some(clock::ticks(seconds));
        }
        let mut named = names(&raw, project);
        named.extend(pointed.iter().copied());
        let selecting_effect = (r.sound_effects || r.music == Music::Add)
            && !named.is_empty()
            && named.iter().all(|id| {
                project
                    .source(*id)
                    .is_some_and(|asset| asset.audio && !asset.video)
            });
        let only = KEEP.is_match(&text) && !selecting_effect;
        let media_text = MUSIC_OFF
            .replace_all(&text, " ")
            .replace("remove everything else", " ")
            .replace("remove anything else", " ")
            .replace("take out everything else", " ")
            .replace("delete everything else", " ");
        let remove = DROP.is_match(&media_text) && !selecting_effect;
        if named.is_empty()
            && r.clips.is_empty()
            && !has_cut
            && has(
                &text,
                &[
                    "show ",
                    "showing ",
                    "use shots of",
                    "footage of",
                    "clips of",
                ],
            )
        {
            let words: Vec<_> = word(&text)
                .filter(|w| {
                    w.len() >= 3
                        && ![
                            "show", "showing", "the", "and", "with", "from", "footage", "clips",
                            "shots", "use", "only", "montage", "video", "make", "new",
                        ]
                        .contains(w)
                })
                .collect();
            for asset in project.sources.iter().filter(|asset| asset.video) {
                if let Some(tags) = crate::vision::cached(store, asset) {
                    let mut times: Vec<_> = tags
                        .iter()
                        .filter(|tag| {
                            words
                                .iter()
                                .any(|word| crate::vision::matching(tag, word, false))
                        })
                        .flat_map(|tag| tag.times.iter().copied().map(clock::seconds))
                        .collect();
                    times.sort_by(f64::total_cmp);
                    times.dedup();
                    if !times.is_empty() {
                        r.moments.insert(asset.id, times);
                    }
                }
            }
            named.extend(r.moments.keys().copied());
        }
        if only && !remove {
            r.keep = named;
            r.clear_rest = has(
                &text,
                &[
                    "nothing else",
                    "everything else",
                    "anything else",
                    "nothing but",
                ],
            );
        } else if remove && !only {
            r.drop = named;
        } else if !has_cut
            && (!pointed.is_empty() || !r.moments.is_empty())
            && !has(&text, &["also", " as well", "include", "other", "all "])
        {
            r.keep = named;
        }
        if has_cut && !(only || remove) && !pointed.is_empty() {
            r.clips.extend(
                project
                    .timeline
                    .iter()
                    .filter(|clip| pointed.contains(&clip.source))
                    .map(|clip| clip.id),
            );
        }
        if has_cut && !r.clips.is_empty() && (only || remove) {
            r.keep_clips = only && !remove;
            r.drop_clips = remove && !only;
            if r.keep_clips && !r.clear_rest {
                r.clear_rest = has(&text, &["nothing else", "everything else", "anything else"]);
            }
        }
        r.missing_scope = (r.clips.is_empty()
            && has(
                &raw,
                &[
                    "@selected",
                    "@clip:",
                    "selected clips",
                    "selected shots",
                    "the selection",
                ],
            ))
            || (r.span.is_none() && has(&raw, &["@in/out", "@in-out", "marked range"]));
        r
    }

    pub fn needs_cut(&self) -> bool {
        self.whole_library
            || self.general
            || self.seconds.is_some()
            || self.pace != Pace::Unchanged
            || self.shots.is_some()
    }

    pub fn has_settings(&self) -> bool {
        self.look.is_some()
            || self.frame.is_some()
            || self.speed.is_some()
            || self.ramp.is_some()
            || self.title.is_some()
            || self.remove_titles
            || self.level_audio
            || self.sound_effects
            || self.captions.is_some()
            || self.music != Music::Unchanged
            || !self.keep.is_empty()
            || !self.drop.is_empty()
            || self.clear_rest
            || self.keep_clips
            || self.drop_clips
            || !self.moments.is_empty()
    }
}
