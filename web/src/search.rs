use crate::{
    clock,
    editor::{Source, Store},
    speech, vision,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Default)]
pub struct Query {
    shows: String,
    said: Vec<String>,
    typing: bool,
}

impl Query {
    pub fn library(text: &str) -> Self {
        let mut query = Self {
            typing: true,
            ..Self::default()
        };
        let mut parts = text.split(['"', '“', '”']);
        query.shows.push_str(parts.next().unwrap_or_default());
        while let Some(phrase) = parts.next() {
            if !phrase.trim().is_empty() {
                query.said.push(phrase.trim().into());
            }
            query.shows.push(' ');
            query.shows.push_str(parts.next().unwrap_or_default());
        }
        query.shows = query.shows.trim().to_lowercase();
        query
    }

    pub fn agent(shows: &str, said: &str) -> Self {
        Self {
            shows: shows.trim().to_lowercase(),
            said: [said.trim()]
                .into_iter()
                .filter(|phrase| !phrase.is_empty())
                .map(str::to_owned)
                .collect(),
            typing: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.shows.is_empty() && self.said.is_empty()
    }
}

#[derive(Serialize)]
struct VisualMatch {
    tag: String,
    times: Vec<f64>,
}

#[derive(Serialize)]
struct SpeechMatch {
    start: f64,
    end: f64,
    words: String,
}

fn seconds(ticks: u32) -> f64 {
    (clock::seconds(ticks) * 1000.0).round() / 1000.0
}

fn tokens(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric() && character != '\'')
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

fn occurrences(transcript: &speech::Transcript, phrase: &str, typing: bool) -> Vec<SpeechMatch> {
    let desired = tokens(phrase);
    let Some((last, complete)) = desired.split_last() else {
        return vec![];
    };
    let words = &transcript.words;
    let spoken = words
        .iter()
        .map(|word| tokens(&word.text).into_iter().next().unwrap_or_default())
        .collect::<Vec<_>>();
    spoken
        .windows(desired.len())
        .enumerate()
        .filter_map(|(start, candidate)| {
            let end = start + desired.len() - 1;
            let final_word = &candidate[candidate.len() - 1];
            let matches = candidate[..candidate.len() - 1] == *complete
                && if typing {
                    final_word.starts_with(last)
                } else {
                    final_word == last
                };
            matches.then(|| SpeechMatch {
                start: seconds(words[start].start),
                end: seconds(words[end].end),
                words: words[start.saturating_sub(4)..(end + 6).min(words.len())]
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            })
        })
        .take(10)
        .collect()
}

#[derive(Serialize)]
struct Hit {
    asset_id: Uuid,
    name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    shows: Vec<VisualMatch>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    said: Vec<SpeechMatch>,
}

pub fn find(store: &Store, sources: &[Source], query: &Query) -> serde_json::Value {
    const COMMON: &[&str] = &["a", "an", "the", "of", "in", "on", "at", "with"];
    let words = query
        .shows
        .split_whitespace()
        .filter(|word| query.typing || !COMMON.contains(word))
        .collect::<Vec<_>>();
    let mut found = vec![];
    let mut unseen = 0;
    let mut unheard = 0;
    let visual = vision::available();
    let speech = speech::available();
    for source in sources {
        let mut hit = Hit {
            asset_id: source.id,
            name: source.title.clone(),
            shows: vec![],
            said: vec![],
        };
        if !words.is_empty() {
            let content = format!("{} {}", source.title, source.notes).to_lowercase();
            let tags = vision::cached(store, source);
            if tags.is_none() && source.video {
                unseen += 1;
            }
            let mut matched = true;
            for (index, word) in words.iter().enumerate() {
                let prefix = query.typing && index == words.len() - 1;
                let labels = tags
                    .iter()
                    .flat_map(|tags| tags.iter())
                    .filter(|tag| vision::matching(tag, word, prefix))
                    .collect::<Vec<_>>();
                if labels.is_empty() && !content.contains(word) {
                    matched = false;
                    break;
                }
                for tag in labels {
                    if hit.shows.iter().any(|seen| seen.tag == tag.name) {
                        continue;
                    }
                    hit.shows.push(VisualMatch {
                        tag: tag.name.clone(),
                        times: tag.times.iter().take(8).map(|&at| seconds(at)).collect(),
                    });
                }
            }
            if !matched {
                continue;
            }
        }
        if !query.said.is_empty() {
            if !source.audio {
                continue;
            }
            let Some(transcript) = speech::cached(store, source) else {
                unheard += 1;
                continue;
            };
            let places = query
                .said
                .iter()
                .map(|phrase| occurrences(&transcript, phrase, query.typing))
                .collect::<Vec<_>>();
            if places.iter().any(Vec::is_empty) {
                continue;
            }
            hit.said = places.into_iter().flatten().collect();
        }
        found.push(hit);
    }
    let mut output =
        serde_json::json!({"found":found, "times":"Seconds in each file, like source_start."});
    let mut notes = vec![];
    if unseen > 0 {
        notes.push(if visual {
            format!("{unseen} media file(s) still waiting for visual indexing")
        } else {
            "Visual indexing is available on a macOS studio host".into()
        });
    }
    if unheard > 0 {
        notes.push(if speech {
            format!("{unheard} media file(s) still waiting for transcription")
        } else {
            "The studio host needs local Whisper to index speech".into()
        });
    }
    if !notes.is_empty() {
        output["note"] = serde_json::json!(format!("{}.", notes.join(". ")));
    }
    if query.typing {
        output["indexing"] = serde_json::json!((visual && unseen > 0) || (speech && unheard > 0));
    }
    output
}

pub fn summaries(store: &Store, sources: &[Source]) -> Vec<Option<Vec<String>>> {
    let tags = sources
        .iter()
        .map(|source| vision::cached(store, source))
        .collect::<Vec<_>>();
    let mut counts = HashMap::<&str, usize>::new();
    let tagged = tags.iter().flatten().count();
    for tags in tags.iter().flatten() {
        for tag in tags.iter() {
            *counts.entry(&tag.name).or_default() += 1;
        }
    }
    tags.iter()
        .map(|tags| {
            tags.as_ref().map(|tags| {
                tags.iter()
                    .take(20)
                    .filter(|tag| tagged < 4 || counts[tag.name.as_str()] * 5 < tagged * 3)
                    .take(8)
                    .map(|tag| tag.name.clone())
                    .collect()
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
pub enum Index {
    Vision,
    Speech,
}

pub async fn index_library(store: &Store, index: Index) {
    let available = match index {
        Index::Vision => vision::available(),
        Index::Speech => speech::available(),
    };
    if !available {
        return;
    }
    let mut retry = HashMap::<Uuid, Instant>::new();
    loop {
        let sources = store.read(|db| {
            db.projects
                .iter()
                .flat_map(|project| {
                    project
                        .sources
                        .iter()
                        .filter(|source| match index {
                            Index::Vision => source.video,
                            Index::Speech => source.audio,
                        })
                        .cloned()
                })
                .collect::<Vec<_>>()
        });
        retry.retain(|_, failed| failed.elapsed() < Duration::from_secs(120));
        let next = sources.into_iter().find(|source| {
            !retry.contains_key(&source.id)
                && match index {
                    Index::Vision => !vision::indexed(store, source),
                    Index::Speech => !speech::indexed(store, source),
                }
        });
        let Some(source) = next else {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        };
        let outcome = match index {
            Index::Vision => vision::index(store, &source).await,
            Index::Speech => speech::transcribe(store, &source).await.map(|_| ()),
        };
        if outcome.is_err() {
            retry.insert(source.id, Instant::now());
        }
    }
}
