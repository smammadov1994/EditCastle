use crate::{
    clock,
    editor::{Source, Store, private_write},
    media,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::OnceLock};

#[derive(Debug, Deserialize, Serialize)]
pub struct Tag {
    pub name: String,
    pub confidence: f32,
    pub times: Vec<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Tags(Vec<Tag>);

impl Tags {
    pub fn iter(&self) -> impl Iterator<Item = &Tag> {
        self.0.iter()
    }
}

const MIN_CONFIDENCE: f32 = 0.35;

pub const fn available() -> bool {
    cfg!(target_os = "macos")
}

pub fn matching(tag: &Tag, word: &str, typing: bool) -> bool {
    static ALIASES: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    let aliases = ALIASES.get_or_init(|| {
        serde_json::from_str(include_str!("../parity/visual-aliases.json"))
            .expect("valid native scene aliases")
    });
    let word = singular(word);
    word.chars().count() >= 2
        && tag
            .name
            .split_whitespace()
            .chain(
                aliases
                    .get(&tag.name)
                    .into_iter()
                    .flatten()
                    .map(String::as_str),
            )
            .any(|label| {
                label == word
                    || singular(label) == word
                    || (typing && label.starts_with(word))
                    || (label.chars().count() >= 5
                        && word.chars().count() >= 5
                        && stem(label) == stem(word))
            })
}

fn singular(word: &str) -> &str {
    if word.chars().count() > 3 && !word.ends_with("ss") {
        word.strip_suffix('s').unwrap_or(word)
    } else {
        word
    }
}

fn stem(mut word: &str) -> &str {
    for ending in ["ing", "ter", "ers", "er", "ed", "es", "s"] {
        if word.chars().count() > ending.len() + 3
            && let Some(base) = word.strip_suffix(ending)
        {
            word = base;
            break;
        }
    }
    if word.chars().count() > 4 {
        word.strip_suffix('e').unwrap_or(word)
    } else {
        word
    }
}

fn cache_path(store: &Store, source: &Source) -> std::path::PathBuf {
    crate::source_color::ready(store, source).with_extension("scene-tags.json")
}

pub fn indexed(store: &Store, source: &Source) -> bool {
    cache_path(store, source).is_file()
}

pub fn cached(store: &Store, source: &Source) -> Option<Tags> {
    if !source.video {
        return None;
    }
    let path = cache_path(store, source);
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub async fn index(store: &Store, source: &Source) -> Result<()> {
    ensure!(
        available() && source.video,
        "On-host image classification is unavailable."
    );
    let input = crate::source_color::playback(store, source).await?;
    let working = media::temp(store).await?;
    let result = async {
        let times = if source.image {
            vec![0]
        } else {
            let count = clock::seconds(source.duration).round().clamp(8.0, 20.0) as u64;
            (0..count)
                .map(|index| (u64::from(source.duration) * (2 * index + 1) / (2 * count)) as u32)
                .collect()
        };
        let mut seen = vec![];
        let photo = working.join("sample.jpg");
        for at in times {
            let _ = tokio::fs::remove_file(&photo).await;
            if media::extract_at(&input, at, &photo, true).await.is_err() {
                continue;
            }
            let data = tokio::fs::read(&photo).await?;
            if let Ok(labels) = tokio::task::spawn_blocking(move || classify(&data)).await? {
                seen.push((at, labels));
            }
        }
        ensure!(!seen.is_empty(), "No frames could be classified.");
        let tags = gather(seen);
        let output = cache_path(store, source);
        private_write(&output, &serde_json::to_vec(&tags)?)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(working).await;
    result
}

fn gather(seen: Vec<(u32, Vec<(String, f32)>)>) -> Tags {
    let mut tags = HashMap::new();
    for (at, labels) in seen {
        for (name, confidence) in labels {
            if !confidence.is_finite() || confidence < MIN_CONFIDENCE {
                continue;
            }
            let tag = tags.entry(name.clone()).or_insert_with(|| Tag {
                name,
                confidence,
                times: vec![],
            });
            tag.confidence = tag.confidence.max(confidence);
            if tag.times.last() != Some(&at) {
                tag.times.push(at);
            }
        }
    }
    let mut tags: Vec<_> = tags.into_values().collect();
    tags.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| a.name.cmp(&b.name))
    });
    Tags(tags)
}

#[cfg(target_os = "macos")]
fn classify(bytes: &[u8]) -> Result<Vec<(String, f32)>> {
    use anyhow::Context;
    use objc2::{AnyThread, rc::autoreleasepool};
    use objc2_foundation::{NSArray, NSData, NSDictionary};
    use objc2_vision::{VNClassifyImageRequest, VNImageRequestHandler, VNRequest};

    autoreleasepool(|_| {
        let data = NSData::with_bytes(bytes);
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &data,
            &NSDictionary::new(),
        );
        // Vision owns the request and result objects on this worker thread.
        let request = unsafe { VNClassifyImageRequest::new() };
        let as_request: &VNRequest = &request;
        handler
            .performRequests_error(&NSArray::from_slice(&[as_request]))
            .map_err(|_| anyhow::anyhow!("Vision could not classify the frame."))?;
        let observations =
            unsafe { request.results() }.context("Vision returned no classifications.")?;
        Ok((0..observations.count())
            .map(|index| {
                let item = observations.objectAtIndex(index);
                let label = unsafe { item.identifier() }.to_string().replace('_', " ");
                (label, unsafe { item.confidence() })
            })
            .collect())
    })
}

#[cfg(not(target_os = "macos"))]
fn classify(_bytes: &[u8]) -> Result<Vec<(String, f32)>> {
    anyhow::bail!("On-host image classification is unavailable.")
}
