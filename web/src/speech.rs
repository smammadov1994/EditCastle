use crate::clock;
use crate::{
    editor::{Source, Store, private_write},
    media,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{env, path::PathBuf, process::Stdio};
use tokio::{
    fs,
    process::Command,
    time::{Duration, timeout},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Line {
    pub start: u32,
    pub end: u32,
    pub text: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Transcript {
    pub language: String,
    pub lines: Vec<Line>,
    pub words: Vec<Line>,
}

pub fn cached(store: &Store, source: &Source) -> Option<Transcript> {
    if !source.audio {
        return Some(Transcript::default());
    }
    let media = store.media(source.storage_id(), source.video);
    if let Ok(data) = std::fs::read(media.with_extension("timed-words.json")) {
        return serde_json::from_slice(&data).ok();
    }
    // Older projects already have transcript timings at 24 fps. Keep them
    // available so their captions remain visible immediately after migration.
    let mut legacy: Transcript =
        serde_json::from_slice(&std::fs::read(media.with_extension("words.json")).ok()?).ok()?;
    for line in legacy.lines.iter_mut().chain(&mut legacy.words) {
        line.start = line.start.saturating_mul(clock::MODEL_FRAME);
        line.end = line.end.saturating_mul(clock::MODEL_FRAME);
    }
    Some(legacy)
}

/// Reuse word timings from the native app when importing media on the same Mac.
#[cfg(target_os = "macos")]
pub fn import_native_cache(store: &Store, source: &Source, path: &std::path::Path) -> Option<()> {
    use std::{fmt::Write, os::unix::fs::MetadataExt};
    let native_cache = PathBuf::from(env::var_os("HOME")?)
        .join("Library/Application Support/Editroom/Transcripts");
    let metadata = std::fs::metadata(path).ok()?;
    let mut date = std::mem::MaybeUninit::<libc::tm>::uninit();
    let second = metadata.mtime() as libc::time_t;
    // gmtime_r writes to our buffer and has no global timezone side effects.
    let date = unsafe {
        if libc::gmtime_r(&second, date.as_mut_ptr()).is_null() {
            return None;
        }
        date.assume_init()
    };
    let stamp = format!(
        "{}|{}|{:04}-{:02}-{:02} {:02}:{:02}:{:02} +0000",
        path.to_str()?,
        metadata.len(),
        date.tm_year + 1900,
        date.tm_mon + 1,
        date.tm_mday,
        date.tm_hour,
        date.tm_min,
        date.tm_sec
    );
    let mut hash = stamp.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    let mut stem = String::new();
    loop {
        stem.write_char(char::from_digit((hash % 36) as u32, 36)?)
            .ok()?;
        hash /= 36;
        if hash == 0 {
            break;
        }
    }
    let hash = stem.chars().rev().collect::<String>();
    let native = native_cache.join(format!("{}-{hash}.json", path.file_stem()?.to_str()?));
    #[derive(Deserialize)]
    struct NativeWord {
        start: f64,
        end: f64,
        text: String,
    }
    #[derive(Deserialize)]
    struct NativeTranscript {
        words: Vec<NativeWord>,
        language: String,
    }
    let native: NativeTranscript = serde_json::from_slice(&std::fs::read(native).ok()?).ok()?;
    let words = native
        .words
        .into_iter()
        .filter_map(|word| {
            let start = clock::ticks(word.start);
            let end = clock::ticks(word.end);
            (end > start).then_some(Line {
                start,
                end,
                text: word.text,
            })
        })
        .collect::<Vec<_>>();
    let mut lines = Vec::<Line>::new();
    for word in &words {
        if let Some(last) = lines.last_mut().filter(|line| {
            word.start.saturating_sub(line.end) < clock::ticks(0.8)
                && !line.text.ends_with(['.', '?', '!'])
        }) {
            last.end = word.end.max(last.end);
            last.text.push(' ');
            last.text.push_str(&word.text);
        } else {
            lines.push(word.clone());
        }
    }
    let file = store
        .media(source.storage_id(), source.video)
        .with_extension("timed-words.json");
    private_write(
        &file,
        &serde_json::to_vec(&Transcript {
            language: native.language,
            lines,
            words,
        })
        .ok()?,
    )
    .ok()
}

fn model() -> Option<PathBuf> {
    env::var_os("EDITROOM_WHISPER_MODEL")
        .map(PathBuf::from)
        .or_else(|| {
            Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web-runtime/ggml-base.bin"))
        })
        .filter(|path| path.is_file())
}

pub fn available() -> bool {
    model().is_some()
        && env::var_os("PATH").is_some_and(|path| {
            env::split_paths(&path).any(|entry| entry.join("whisper-cli").is_file())
        })
}

pub fn indexed(store: &Store, source: &Source) -> bool {
    let media = store.media(source.storage_id(), source.video);
    media.with_extension("timed-words.json").is_file()
        || media.with_extension("words.json").is_file()
}

pub async fn transcribe(store: &Store, source: &Source) -> Result<Transcript> {
    if let Some(transcript) = cached(store, source) {
        return Ok(transcript);
    }
    let _slot = store.transcription_slot.lock().await;
    if let Some(transcript) = cached(store, source) {
        return Ok(transcript);
    }
    let configured =
        model().context("The studio host needs a local Whisper model to transcribe this clip.")?;
    let work = media::temp(store).await?;
    let result = async {
        let audio = work.join("speech.wav");
        media::speech_audio(&store.media(source.storage_id(), source.video), &audio).await?;
        let output = work.join("speech");
        let run = timeout(
            Duration::from_secs(900),
            Command::new("whisper-cli")
                .arg("-m")
                .arg(configured)
                .arg("-f")
                .arg(audio)
                .arg("-of")
                .arg(&output)
                .args(["-l", "auto", "-ojf", "-dtw", "base", "-np", "-t", "4"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await?
        .context("Install whisper-cli to transcribe audio locally.")?;
        ensure!(run.success(), "This audio could not be transcribed.");
        let json: Value = serde_json::from_slice(&fs::read(output.with_extension("json")).await?)?;
        let mut transcript = Transcript {
            language: json["result"]["language"].as_str().unwrap_or("").into(),
            ..Transcript::default()
        };
        for segment in json["transcription"].as_array().into_iter().flatten() {
            if let Some(line) = read_line(segment) {
                transcript.lines.push(line);
            }
            let mut previous: Option<Line> = None;
            for token in segment["tokens"].as_array().into_iter().flatten() {
                let Some(mut word) = read_line(token) else {
                    continue;
                };
                let lower = word.text.trim_start();
                if lower.starts_with("[_") || lower.starts_with("<|") {
                    continue;
                }
                let begins = word.text.starts_with(char::is_whitespace);
                word.text = lower.trim_end().into();
                if !begins && let Some(last) = &mut previous {
                    last.text.push_str(&word.text);
                    last.end = word.end.max(last.end);
                } else if let Some(last) = previous.replace(word) {
                    transcript.words.push(last);
                }
            }
            if let Some(last) = previous {
                transcript.words.push(last);
            }
        }
        let cache = store
            .media(source.storage_id(), source.video)
            .with_extension("timed-words.json");
        private_write(&cache, &serde_json::to_vec(&transcript)?)?;
        Ok(transcript)
    }
    .await;
    let _ = fs::remove_dir_all(work).await;
    result
}

fn read_line(value: &Value) -> Option<Line> {
    let content = value["text"].as_str()?;
    let clean = content.trim();
    if clean.is_empty() || clean.starts_with('[') && clean.ends_with(']') {
        return None;
    }
    let to_frame = |milliseconds: f64| clock::ticks(milliseconds / 1000.0);
    let start = to_frame(value["offsets"]["from"].as_f64()?);
    let end = to_frame(value["offsets"]["to"].as_f64()?);
    (end >= start).then(|| Line {
        start,
        end,
        text: content.into(),
    })
}

fn seconds(frame: u32) -> f64 {
    (clock::seconds(frame) * 1000.0).round() / 1000.0
}

pub fn response(transcript: Result<Transcript>, source: &Source, words: bool) -> Value {
    let mut json =
        json!({"asset_id":source.id,"name":source.title,"duration":seconds(source.duration)});
    if !source.audio {
        json["status"] = json!("no_sound");
        json["note"] = json!(format!("{} has no sound.", source.title));
        return json;
    }
    let transcript = match transcript {
        Ok(transcript) => transcript,
        Err(_) => {
            let (status, note) = if available() {
                ("failed", format!("Could not transcribe {}.", source.title))
            } else {
                (
                    "unavailable",
                    "Set up local Whisper on the Studio host to transcribe audio.".into(),
                )
            };
            json["status"] = json!(status);
            json["note"] = json!(note);
            return json;
        }
    };
    if transcript.lines.is_empty() {
        json["status"] = json!("no_speech");
        json["note"] = json!("Nobody talks in it (or only music or noise).");
        return json;
    }
    json["status"] = json!("ready");
    json["language"] = json!(transcript.language);
    json["times"] = json!("Seconds in the file. A clip plays the file from its source_start.");
    json["sentences"] = json!(
        transcript
            .lines
            .iter()
            .map(|line| json!({
                "start":seconds(line.start),"end":seconds(line.end),"text":line.text.trim()
            }))
            .collect::<Vec<_>>()
    );
    let mut position = 0;
    let mut pauses = vec![];
    for line in &transcript.lines {
        if line.start.saturating_sub(position) >= clock::ticks(0.5) {
            pauses.push(json!({"start":seconds(position),"end":seconds(line.start)}));
        }
        position = position.max(line.end);
    }
    if source.duration.saturating_sub(position) >= clock::ticks(0.5) {
        pauses.push(json!({"start":seconds(position),"end":seconds(source.duration)}));
    }
    json["pauses"] = json!(pauses);
    if words {
        json["words"] = json!(
            transcript
                .words
                .iter()
                .map(|word| json!([seconds(word.start), seconds(word.end), word.text]))
                .collect::<Vec<_>>()
        );
    }
    json
}
