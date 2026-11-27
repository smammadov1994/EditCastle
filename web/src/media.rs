use crate::source_color::{self, SourceColor};
use crate::{
    clock,
    editor::{Clip, ClipKind, FadeCurve, Project, SequenceFormat, SequenceOffer, Source, Store},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    sync::Semaphore,
    task::JoinSet,
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod contact_sheet;
mod loudness;
pub(crate) use contact_sheet::source_contact_sheet;

pub async fn temp(store: &Store) -> Result<PathBuf> {
    let dir = store.root.join(format!("work-{}", Uuid::new_v4()));
    fs::create_dir(&dir).await?;
    Ok(dir)
}

pub fn seconds(ticks: u32) -> String {
    format!("{:.9}", clock::seconds(ticks))
}

async fn ffmpeg(args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Result<()> {
    ffmpeg_at(None, args).await
}

async fn ffmpeg_at(
    dir: Option<&Path>,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<()> {
    let mut command = Command::new("ffmpeg");
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let status = timeout(
        Duration::from_secs(900),
        command
            .args(["-hide_banner", "-nostdin", "-v", "error", "-y"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await?
    .context("FFmpeg is required for editing and export.")?;
    ensure!(status.success(), "FFmpeg could not process this media.");
    Ok(())
}

const MEDIA_FORMATS: &str = "mov,mp4,m4a,3gp,3g2,mj2,matroska,webm,mxf,mxf_d10,mxf_opatom,avi,mpeg,mpegts,wav,aiff,flac,ogg,mp3,aac,ac3,hevc,h264,asf,rm,caf,amr";

pub async fn details(path: &Path) -> Result<Value> {
    let output = timeout(
        Duration::from_secs(60),
        Command::new("ffprobe")
            .args([
                "-v", "error", "-protocol_whitelist", "file",
                "-show_entries", "stream=codec_type,width,height,r_frame_rate,color_space,color_range,color_transfer,color_primaries,pix_fmt:stream_side_data=rotation:stream_disposition=attached_pic:stream_tags:format=format_name:format_tags",
                "-of", "json",
            ])
            .arg(path)
            .kill_on_drop(true)
            .output(),
    ).await??;
    ensure!(
        output.status.success(),
        "The media format could not be read."
    );
    serde_json::from_slice(&output.stdout).context("Read media properties.")
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(crate) struct PictureInfo {
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
}

pub(crate) fn picture_info(data: &Value, still: bool) -> Result<PictureInfo> {
    let stream = data["streams"]
        .as_array()
        .and_then(|streams| {
            streams.iter().find(|stream| {
                stream["codec_type"] == "video" && stream["disposition"]["attached_pic"] != 1
            })
        })
        .context("The source has no video stream.")?;
    let dimension = |key: &str| -> Result<u32> {
        stream[key]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .context("The picture has no dimensions.")
    };
    let (mut width, mut height) = (dimension("width")?, dimension("height")?);
    let quarter_turn = stream["side_data_list"].as_array().is_some_and(|entries| {
        entries.iter().any(|entry| {
            entry["rotation"]
                .as_i64()
                .is_some_and(|angle| angle.rem_euclid(180) == 90)
        })
    });
    if quarter_turn {
        std::mem::swap(&mut width, &mut height);
    }
    let fps = stream["r_frame_rate"]
        .as_str()
        .and_then(|rate| {
            let (numerator, denominator) = rate.split_once('/')?;
            Some(numerator.parse::<f64>().ok()? / denominator.parse::<f64>().ok()?)
        })
        .filter(|rate| !still && rate.is_finite() && *rate > 0.0);
    Ok(PictureInfo { width, height, fps })
}

fn picture_cache(media: &Path) -> PathBuf {
    media.with_extension("picture-v1.json")
}

async fn cache_picture(media: &Path, info: PictureInfo) {
    let path = picture_cache(media);
    let _ = tokio::task::spawn_blocking(move || {
        crate::editor::private_write(&path, &serde_json::to_vec(&info)?)
    })
    .await;
}

async fn inspect_picture(original: PathBuf, media: PathBuf, still: bool) -> Result<PictureInfo> {
    if let Ok(bytes) = fs::read(picture_cache(&media)).await
        && let Ok(info) = serde_json::from_slice::<PictureInfo>(&bytes)
    {
        return Ok(info);
    }
    static PICTURE_PROBES: Semaphore = Semaphore::const_new(4);
    let _slot = PICTURE_PROBES.acquire().await?;
    let data = if original.is_file() {
        details(&original).await?
    } else {
        details(&media).await?
    };
    let info = picture_info(&data, still)?;
    cache_picture(&media, info).await;
    Ok(info)
}

pub(crate) async fn source_pictures(
    store: &Store,
    sources: &[Source],
) -> HashMap<Uuid, PictureInfo> {
    let mut jobs = JoinSet::new();
    for source in sources.iter().filter(|source| source.video) {
        let id = source.id;
        let still = source.image;
        let version = source.storage_id();
        let original = store.original(version);
        let media = store.media(version, true);
        jobs.spawn(async move { (id, inspect_picture(original, media, still).await) });
    }
    let mut pictures = HashMap::new();
    while let Some(result) = jobs.join_next().await {
        if let Ok((id, Ok(picture))) = result {
            pictures.insert(id, picture);
        }
    }
    pictures
}

pub async fn picture_format(store: &Store, source: &Source) -> Result<SequenceFormat> {
    let id = source.storage_id();
    let PictureInfo { width, height, fps } =
        inspect_picture(store.original(id), store.media(id, true), source.image).await?;
    ensure!(
        (128..=7680).contains(&width) && (128..=7680).contains(&height),
        "Dimensions must be between 128 and 7680."
    );
    let fps = fps
        .filter(|rate| rate.is_finite() && *rate > 1.0)
        .and_then(|rate| {
            clock::EDIT_RATES
                .into_iter()
                .min_by(|a, b| (a - rate).abs().total_cmp(&(b - rate).abs()))
        })
        .unwrap_or(30.0);
    Ok(SequenceFormat { width, height, fps })
}

pub async fn sequence_offer(store: &Store, source: &Source) -> Option<SequenceOffer> {
    if !source.video || source.image {
        return None;
    }
    let format = picture_format(store, source).await.ok()?;
    Some(SequenceOffer {
        asset: source.id,
        version: source.storage_id(),
        format,
    })
}

pub async fn probe(path: &Path) -> Result<(bool, bool, u32)> {
    let output = timeout(
        Duration::from_secs(60),
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file",
                "-format_whitelist",
                MEDIA_FORMATS,
                "-show_entries",
                "stream=codec_type,duration,duration_ts,time_base:format=duration",
                "-of",
                "json",
            ])
            .arg(path)
            .kill_on_drop(true)
            .output(),
    )
    .await?
    .context("FFprobe is required for media inspection.")?;
    ensure!(
        output.status.success(),
        "The file could not be read as video or audio."
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    let streams = value["streams"]
        .as_array()
        .context("The file has no tracks.")?;
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video");
    let audio = streams.iter().any(|stream| stream["codec_type"] == "audio");
    let primary = video.or_else(|| {
        streams
            .iter()
            .find(|stream| stream["codec_type"] == "audio")
    });
    let duration = primary
        .and_then(|stream| {
            let count = stream["duration_ts"].as_i64()?;
            let (num, den) = stream["time_base"].as_str()?.split_once('/')?;
            Some(count as f64 * num.parse::<f64>().ok()? / den.parse::<f64>().ok()?)
        })
        .or_else(|| {
            primary
                .and_then(|stream| stream["duration"].as_str())
                .or(value["format"]["duration"].as_str())?
                .parse()
                .ok()
        })
        .filter(|duration| *duration > 0.0 && *duration <= 7200.0)
        .map(clock::ticks)
        .filter(|ticks| *ticks > 0)
        .context("The file has an unsupported duration.")?;
    ensure!(video.is_some() || audio, "The file has no audio or video.");
    Ok((video.is_some(), audio, duration))
}

// JPEG decoders interpret YCbCr using the JFIF (BT.601) matrix, even when the video was BT.709.
const REVIEW_IMAGE: &str = ",scale=480:-2:out_color_matrix=bt601:out_range=full,format=yuvj420p";

pub async fn extract(path: &Path, frame: u32, dest: &Path, scaled: bool) -> Result<()> {
    // Decode by index until the visible frame. A timestamp seek can round into the next frame.
    let mut filter = format!("select=eq(n\\,{frame})");
    if scaled {
        filter.push_str(REVIEW_IMAGE);
    }
    ffmpeg([
        "-protocol_whitelist",
        "file",
        "-i",
        path.to_str().unwrap(),
        "-vf",
        &filter,
        "-frames:v",
        "1",
        "-an",
        dest.to_str().unwrap(),
    ])
    .await?;
    ensure!(dest.exists(), "A frame was missing from the rendered clip.");
    Ok(())
}

// Read the last frame presented at or before this edit time. Decode extra preroll
// after the seek so a high-rate edit can still reference low-rate source footage.
pub async fn extract_at(path: &Path, at: u32, dest: &Path, scaled: bool) -> Result<()> {
    extract_still_at(path, at, dest, if scaled { REVIEW_IMAGE } else { "" }).await
}

async fn extract_still_at(path: &Path, at: u32, dest: &Path, output_filter: &str) -> Result<()> {
    let seek = (at / clock::TICKS_PER_SECOND).saturating_sub(2) * clock::TICKS_PER_SECOND;
    let filter = format!(
        "settb=1/{},trim=end_pts={},reverse{output_filter}",
        clock::TICKS_PER_SECOND,
        at + 1
    );
    ffmpeg([
        "-protocol_whitelist",
        "file",
        "-copyts",
        "-ss",
        &seconds(seek),
        "-i",
        path.to_str().unwrap(),
        "-vf",
        &filter,
        "-frames:v",
        "1",
        "-an",
        "-update",
        "1",
        dest.to_str().unwrap(),
    ])
    .await?;
    ensure!(
        dest.exists(),
        "A frame was missing at the requested media time."
    );
    Ok(())
}

async fn image_header(path: &Path) -> Result<bool> {
    use tokio::io::AsyncReadExt;
    let mut file = fs::File::open(path).await?;
    let mut head = [0; 12];
    let len = file.read(&mut head).await?;
    Ok(len >= 12
        && (head.starts_with(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10])
            || head.starts_with(&[0xff, 0xd8, 0xff])
            || head.starts_with(b"RIFF") && &head[8..12] == b"WEBP"))
}

pub async fn import(store: &Store, path: &Path, title: String) -> Result<Source> {
    if image_header(path).await? {
        let id = Uuid::new_v4();
        let dest = store.media(id, true);
        ffmpeg(["-protocol_whitelist","file","-i",path.to_str().unwrap(),"-an","-vf",
            "scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2,format=yuv420p,tpad=stop_mode=clone:stop_duration=5",
            "-t","5","-r","24","-c:v","libx264","-preset","veryfast","-movflags","+faststart",dest.to_str().unwrap()]).await?;
        if let Err(error) = fs::copy(path, store.original(id)).await {
            let _ = fs::remove_file(&dest).await;
            return Err(error).context("Keep the original image for portable projects.");
        }
        if let Ok(data) = details(path).await
            && let Ok(picture) = picture_info(&data, true)
        {
            cache_picture(&dest, picture).await;
        }
        return Ok(Source {
            storage: None,
            origin_stamp: None,
            device_digest: None,
            id,
            title,
            duration: clock::ticks(5.0),
            audio: false,
            video: true,
            image: true,
            notes: String::new(),
        });
    }
    let (video, _, _) = probe(path).await?;
    let id = Uuid::new_v4();
    let dest = store.media(id, video);
    let (mut color, picture) = if video {
        let data = details(path).await?;
        (
            SourceColor::inspect_details(path, &data).await?,
            picture_info(&data, false).ok(),
        )
    } else {
        (SourceColor::default(), None)
    };
    let result = if video {
        encode_video_proxy(&store.root, path, &dest, color).await
    } else {
        ffmpeg([
            "-protocol_whitelist",
            "file",
            "-i",
            path.to_str().unwrap(),
            "-map",
            "0:a:0",
            "-vn",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
            dest.to_str().unwrap(),
        ])
        .await
    };
    if let Err(error) = result {
        let _ = fs::remove_file(&dest).await;
        return Err(error);
    }
    let (_, audio, duration) = probe(&dest).await?;
    let original = store.original(id);
    fs::copy(path, &original)
        .await
        .context("Keep the original media for export.")?;
    if video {
        color.baked = true;
        let _ = source_color::cache(store, id, color);
    }
    if let Some(picture) = picture {
        cache_picture(&dest, picture).await;
    }
    Ok(Source {
        storage: None,
        origin_stamp: None,
        device_digest: None,
        id,
        title,
        duration,
        audio,
        video,
        image: false,
        notes: String::new(),
    })
}

pub(crate) async fn encode_video_proxy(
    dir: &Path,
    input: &Path,
    output: &Path,
    color: SourceColor,
) -> Result<()> {
    let filter = color.proxy_filter(dir).await?;
    let input = std::path::absolute(input)?;
    let output = std::path::absolute(output)?;
    let mut args = vec![
        "-filter_threads",
        "2",
        "-protocol_whitelist",
        "file",
        "-i",
        input.to_str().context("Invalid media filename.")?,
        "-map",
        "0:v:0",
        "-map",
        "0:a:0?",
        "-vf",
        filter
            .as_deref()
            .unwrap_or("scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2"),
        "-fps_mode",
        "passthrough",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
    ];
    if filter.is_some() {
        args.extend(source_color::SDR_TAGS);
        args.extend(["-map_metadata", "-1"]);
    }
    args.extend([
        "-movflags",
        "+faststart",
        output.to_str().context("Invalid playback filename.")?,
    ]);
    ffmpeg_at(Some(dir), args).await
}

pub async fn discard_import(store: &Store, source: &Source) {
    let _ = fs::remove_file(store.media(source.storage_id(), source.video)).await;
    let _ = fs::remove_file(store.original(source.storage_id())).await;
    if source.video {
        source_color::discard(store, source.storage_id()).await;
        let _ = fs::remove_file(picture_cache(&store.media(source.storage_id(), true))).await;
    }
    let _ = fs::remove_file(
        store
            .media(source.storage_id(), source.video)
            .with_extension("timed-words.json"),
    )
    .await;
    if source.video {
        let _ = fs::remove_file(store.media(source.storage_id(), true).with_extension("jpg")).await;
        let _ = fs::remove_file(
            store
                .media(source.storage_id(), true)
                .with_extension("thumb-v2.jpg"),
        )
        .await;
    }
}

pub async fn cut_render(src: &Path, dest: &Path, visible: u32) -> Result<SourceInfo> {
    let (video, audio, duration) = probe(src).await?;
    let requested = visible * clock::MODEL_FRAME;
    ensure!(
        video && duration >= requested,
        "The render is shorter than the requested shot."
    );
    // Re-encode the visible range so the last stored video frame matches the handoff.
    ffmpeg([
        "-i",
        src.to_str().unwrap(),
        "-map",
        "0:v:0",
        "-map",
        "0:a:0?",
        "-frames:v",
        &visible.to_string(),
        "-t",
        &seconds(requested),
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-movflags",
        "+faststart",
        dest.to_str().unwrap(),
    ])
    .await?;
    let (video, _, duration) = probe(dest).await?;
    ensure!(
        video && duration == requested,
        "The render could not be trimmed to the planned length."
    );
    Ok(SourceInfo { audio })
}

pub struct SourceInfo {
    pub audio: bool,
}

#[derive(Clone, Default, serde::Deserialize)]
pub struct ExportOptions {
    pub height: Option<u32>,
    pub shape: Option<String>,
    pub codec: Option<String>,
    pub quality: Option<String>,
    pub include_audio: Option<bool>,
    pub format: Option<String>,
    pub range_only: Option<bool>,
    pub loudness: Option<f64>,
    pub reframe_x: Option<f64>,
    pub reframe_y: Option<f64>,
}

fn fit_size(project: &Project, options: &ExportOptions) -> Result<(u32, u32, u32, u32)> {
    let (width, height) = (project.sequence.width, project.sequence.height);
    let short = options.height.unwrap_or(width.min(height));
    ensure!(
        (144..=4320).contains(&short) || options.height.is_none(),
        "Choose an export height from 144 to 4320."
    );
    let (out_w, out_h) = match options.shape.as_deref().unwrap_or("original") {
        "original" => {
            let height_out = f64::from(options.height.unwrap_or(height));
            (
                f64::from(width) / f64::from(height) * height_out,
                height_out,
            )
        }
        "square" => (f64::from(short), f64::from(short)),
        "vertical" => (f64::from(short), f64::from(short) * 16.0 / 9.0),
        "portrait" => (f64::from(short), f64::from(short) * 5.0 / 4.0),
        _ => bail!("Choose original, square, portrait or vertical."),
    };
    let even = |side: f64| ((side / 2.0).round() as u32 * 2).max(2);
    Ok((width / 2 * 2, height / 2 * 2, even(out_w), even(out_h)))
}

struct Composition {
    args: Vec<String>,
    filters: Vec<String>,
    count: usize,
    color_managed: bool,
}

impl Composition {
    fn input<'a>(&mut self, params: impl IntoIterator<Item = &'a str>, value: &str) -> usize {
        self.args.extend(params.into_iter().map(str::to_owned));
        self.args.extend(["-i".into(), value.into()]);
        let index = self.count;
        self.count += 1;
        index
    }
}

fn tempo(mut speed: f64) -> String {
    let mut factors = Vec::new();
    while speed > 2.0 {
        factors.push("atempo=2".to_string());
        speed /= 2.0;
    }
    while speed < 0.5 {
        factors.push("atempo=0.5".to_string());
        speed *= 2.0;
    }
    factors.push(format!("atempo={speed:.6}"));
    factors.join(",")
}

fn framerate(project: &Project) -> String {
    match project.sequence.fps {
        23.976 => "24000/1001".into(),
        29.97 => "30000/1001".into(),
        59.94 => "60000/1001".into(),
        119.88 => "120000/1001".into(),
        value => value.to_string(),
    }
}

async fn grade_filter(
    store: &Store,
    grade: &serde_json::Value,
    dir: &Path,
    index: usize,
    alpha: bool,
) -> Result<Option<String>> {
    if crate::grade::is_neutral(grade) {
        return Ok(None);
    }
    let filename = format!("grade-{index}.cube");
    let path = dir.join(&filename);
    let custom = grade["look"]
        .as_str()
        .and_then(crate::grade::look_id)
        .map(|id| store.lut(id));
    let grade = grade.clone();
    tokio::task::spawn_blocking(move || crate::grade::bake(&path, &grade, custom.as_deref()))
        .await??;
    let format = if alpha { "gbrap" } else { "gbrp" };
    Ok(Some(format!(
        "format={format},lut3d=file={filename}:interp=trilinear,"
    )))
}

#[derive(Clone, Copy)]
struct PictureWindow {
    at: u32,
    shown_at: u32,
    shown_length: u32,
    source_start: u32,
    source_end: u32,
    plan: crate::transition::Plan,
}

impl PictureWindow {
    fn new(project: &Project, clip: &Clip, at: u32, plan: crate::transition::Plan) -> Self {
        let shown_at = plan.render_start(at);
        let shown_length = plan.render_end(at, clip.duration()) - shown_at;
        let (source_start, source_end) = plan.source_range(clip, project);
        Self {
            at,
            shown_at,
            shown_length,
            source_start,
            source_end,
            plan,
        }
    }

    fn wipes(self) -> bool {
        self.plan
            .reveal
            .is_some_and(|edge| edge.kind == crate::transition::Kind::Wipe)
    }

    fn alpha_filter(self, clip: &Clip, in_frames: u32, wipe_x: &str, width: u32) -> String {
        let plan = self.plan;
        if plan.reveal.is_none() && plan.conceal.is_none() && !plan.extends() {
            return picture_fades(clip, in_frames);
        }
        let length = clip.duration();
        let head = in_frames.min(length);
        let tail = clip.effects.fade_out_frames.min(length);
        let rel = format!("(T-{})", seconds(self.at - self.shown_at));
        let mut alpha = "alpha(X,Y)".to_owned();
        if head > 0 || tail > 0 {
            let a = fade_expression(&rel, clip.effects.fade_in_curve, length, head, true);
            let b = fade_expression(&rel, clip.effects.fade_out_curve, length, tail, false);
            alpha.push_str(&format!("*min({a},{b})"));
        }
        let progress = |edge: crate::transition::Edge| {
            format!(
                "clip((T+{}-{})/{},0,1)",
                seconds(self.shown_at),
                seconds(edge.start),
                seconds(edge.end.saturating_sub(edge.start).max(1))
            )
        };
        if let Some(edge) = plan.reveal {
            let p = progress(edge);
            if self.wipes() {
                alpha.push_str(&format!(
                    "*if(gte({p},1),1,lt(X+({wipe_x}),{width}*{p}*{p}*(3-2*{p})))"
                ));
            } else {
                alpha.push_str(&format!("*({p})"));
            }
        }
        if let Some(edge) = plan.conceal {
            alpha.push_str(&format!("*(1-{})", progress(edge)));
        }
        if alpha == "alpha(X,Y)" {
            String::new()
        } else {
            format!("geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='{alpha}',")
        }
    }
}

async fn add_picture(
    graph: &mut Composition,
    store: &Store,
    clip: &Clip,
    source: Option<&Source>,
    window: PictureWindow,
    index: usize,
    width: u32,
    height: u32,
    dir: &Path,
    rate: &str,
    source_color: SourceColor,
) -> Result<()> {
    let fx = &clip.effects;
    let length = seconds(window.shown_length);
    let take = seconds(window.source_end - window.source_start);
    let seek =
        (window.source_start / clock::TICKS_PER_SECOND).saturating_sub(1) * clock::TICKS_PER_SECOND;
    let offset = window.source_start - seek;
    let read = seconds(if fx.freeze {
        offset + clock::TICKS_PER_SECOND
    } else {
        window.source_end - seek + clock::TICKS_PER_SECOND
    });
    let label = format!("v{index}");
    let input = if clip.kind == ClipKind::Solid {
        let color = valid_color(&fx.color, "252530");
        graph.input(
            ["-f", "lavfi"],
            &format!("color=c=0x{color}:size={width}x{height}:rate={rate}:duration={length}"),
        )
    } else {
        let id = source
            .context("Picture source is unavailable.")?
            .storage_id();
        let original = store.original(id);
        let source = if original.exists() {
            original
        } else {
            store.media(id, true)
        };
        graph.input(
            [
                "-protocol_whitelist",
                "file",
                "-ss",
                &seconds(seek),
                "-t",
                &read,
            ],
            &source.to_string_lossy(),
        )
    };
    let transform =
        crate::transform::Transform::read(&fx.transform, clock::seconds(clip.duration()));
    let [scale, x, y] = transform.at(0.0);
    let scale_late = transform.zoom.is_some() || window.wipes();
    let base_scale = if scale_late { 1.0 } else { scale };
    let base_scale = base_scale.min(8192.0 / f64::from(width.max(height)));
    let fit_w = ((f64::from(width) * base_scale) as u32 / 2 * 2).clamp(2, 8192);
    let fit_h = ((f64::from(height) * base_scale) as u32 / 2 * 2).clamp(2, 8192);
    let mut filter = format!("[{input}:v]");
    if clip.kind != ClipKind::Solid {
        filter.push_str(&format!(
            "settb=1/{},setpts=PTS-{offset},",
            clock::TICKS_PER_SECOND
        ));
    }
    if fx.freeze {
        filter.push_str(&format!(
            "fps={rate}:start_time=0:round=up,trim=end_frame=1,tpad=stop_mode=clone:stop_duration={length},"
        ));
    } else if fx.reverse {
        filter.push_str(&format!(
            "fps={rate}:start_time=0:round=up,trim=duration={take},reverse,settb=1/{},",
            clock::TICKS_PER_SECOND
        ));
    }
    let clock = if let Some(ramp) = crate::retime::Ramp::read(&fx.ramp) {
        let points = ramp.points(
            clock::seconds(window.source_start),
            clock::seconds(window.source_end),
            fx.reverse,
        );
        crate::retime::expression(&points)
    } else {
        format!("PTS/{:.6}", fx.speed)
    };
    filter.push_str(&format!(
        "setpts='{clock}',fps={rate}:start_time=0:round=up,trim=duration={length},"
    ));
    if let Some(source) = source_color.rgb_filter(dir).await? {
        filter.push_str(&source);
        filter.push(',');
    }
    filter.push_str(&format!(
        "scale={fit_w}:{fit_h}:force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1,"
    ));
    if let Some(grade) = grade_filter(store, &fx.grade, dir, index, false).await? {
        filter.push_str(&grade);
    }
    let in_frames = if clip.fade_in {
        fx.fade_in_frames.max(clock::ticks(1.0 / 3.0))
    } else {
        fx.fade_in_frames
    };
    if graph.color_managed {
        filter.push_str(&source_color::managed_yuv(true));
        filter.push(',');
    } else {
        filter.push_str("format=yuva420p,");
    }
    filter.push_str(&window.alpha_filter(clip, in_frames, &format!("({width}-W)/2"), width));
    let (local_amount, edit_amount) = if let Some(zoom) = transform.zoom {
        let local = format!("(t-{})", seconds(window.at - window.shown_at));
        let edit = format!("(t-{})", seconds(window.at));
        (zoom.expression(&local), zoom.expression(&edit))
    } else {
        ("1".into(), "1".into())
    };
    if scale_late {
        let scale = format!(
            "min(1+({:.7})*({local_amount}),8192/max(iw,ih))",
            transform.scale - 1.0
        );
        filter.push_str(&format!("scale=w='max(2,trunc(iw*{scale}/2)*2)':h='max(2,trunc(ih*{scale}/2)*2)':eval=frame,setsar=1,"));
    }
    let (x, y) = if transform.zoom.is_some() {
        (
            format!("{:.7}*({edit_amount})", transform.x),
            format!("{:.7}*({edit_amount})", transform.y),
        )
    } else {
        (format!("{x:.7}"), format!("{y:.7}"))
    };
    filter.push_str(&format!(
        "settb=1/{},setpts=PTS+{}[{label}]",
        clock::TICKS_PER_SECOND,
        window.shown_at
    ));
    graph.filters.push(filter);
    let out = index + 1;
    graph.filters.push(format!("[canvas{index}][{label}]overlay=x='(main_w-overlay_w)/2+({x})*main_w':y='(main_h-overlay_h)/2+({y})*main_h':eval=frame:eof_action=pass:repeatlast=0[canvas{out}]"));
    Ok(())
}

fn fade_expression(
    variable: &str,
    curve: FadeCurve,
    length: u32,
    fade: u32,
    entering: bool,
) -> String {
    if fade == 0 {
        return "1".into();
    }
    let progress = if entering {
        format!("{variable}/{}", seconds(fade))
    } else {
        format!("({}-{variable})/{}", seconds(length), seconds(fade))
    };
    curve.expression(&progress)
}

fn picture_fades(clip: &Clip, in_frames: u32) -> String {
    let fx = &clip.effects;
    let duration = clip.duration();
    let head = in_frames.min(duration);
    let tail = fx.fade_out_frames.min(duration);
    if (head > 0 && fx.fade_in_curve != FadeCurve::Linear)
        || (tail > 0 && fx.fade_out_curve != FadeCurve::Linear)
        || u64::from(head) + u64::from(tail) > u64::from(duration)
    {
        let first = fade_expression("T", fx.fade_in_curve, duration, head, true);
        let last = fade_expression("T", fx.fade_out_curve, duration, tail, false);
        return format!(
            "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)*min({first},{last})',"
        );
    }
    let mut filter = String::new();
    if head > 0 {
        filter.push_str(&format!("fade=t=in:st=0:d={}:alpha=1,", seconds(head)));
    }
    if tail > 0 {
        filter.push_str(&format!(
            "fade=t=out:st={}:d={}:alpha=1,",
            seconds(duration - tail),
            seconds(tail)
        ));
    }
    filter
}

fn valid_color(value: &str, default: &'static str) -> String {
    let value = value.trim_start_matches('#');
    if value.len() == 6 && value.bytes().all(|c| c.is_ascii_hexdigit()) {
        value.into()
    } else {
        default.into()
    }
}

fn title_font_database() -> &'static fontdb::Database {
    use std::sync::OnceLock;
    static FONTS: OnceLock<fontdb::Database> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = fontdb::Database::new();
        fonts.load_system_fonts();
        fonts
    })
}

pub fn title_families() -> &'static [String] {
    use std::sync::OnceLock;
    static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let mut families = title_font_database()
            .faces()
            .flat_map(|f| f.families.iter().map(|(name, _)| name.clone()))
            .collect::<Vec<_>>();
        families.retain(|name| !name.starts_with('.'));
        families.sort_unstable_by_key(|name| name.to_lowercase());
        families.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
        families.insert(0, "System".into());
        families
    })
}

pub fn title_font(
    family: &str,
    weight: fontdb::Weight,
) -> Result<(&'static fontdb::Database, fontdb::ID)> {
    use fontdb::{Family, Query};
    let fonts = title_font_database();
    let requested = if family.is_empty() || family.eq_ignore_ascii_case("System") {
        if cfg!(target_os = "macos") {
            "SF Pro"
        } else {
            "DejaVu Sans"
        }
    } else {
        family
    };
    let families = [
        Family::Name(requested),
        Family::Name("SF Pro Text"),
        Family::Name("Helvetica"),
        Family::Name("DejaVu Sans"),
        Family::SansSerif,
    ];
    let id = fonts
        .query(&Query {
            families: &families,
            weight,
            ..Default::default()
        })
        .or_else(|| fonts.faces().next().map(|face| face.id))
        .context("Install a system font to render titles.")?;
    Ok((fonts, id))
}

fn title_image(
    path: &Path,
    content: &str,
    size: f32,
    color: [u8; 3],
    family: &str,
    bold: bool,
    max_width: u32,
) -> Result<()> {
    write_title_image(
        std::fs::File::create(path)?,
        content,
        TitleFont {
            size,
            color,
            family,
            bold,
        },
        max_width,
        1.0,
    )?;
    Ok(())
}

struct TitleFont<'a> {
    size: f32,
    color: [u8; 3],
    family: &'a str,
    bold: bool,
}

struct TitleLine {
    glyphs: Vec<(ab_glyph::GlyphId, f32)>,
    width: f32,
}

fn title_lines(
    content: &str,
    max_width: f32,
    max_lines: usize,
    shape: impl Fn(char, Option<ab_glyph::GlyphId>) -> (ab_glyph::GlyphId, f32, f32),
) -> Vec<TitleLine> {
    let mut lines = Vec::new();
    for paragraph in content.split('\n') {
        let mut rest = paragraph.trim_end_matches(char::is_whitespace);
        loop {
            if lines.len() == max_lines {
                return lines;
            }
            let mut glyphs = Vec::new();
            let mut width = 0.0;
            let mut break_at = None;
            let mut at_word = false;
            let mut next = "";
            for (index, ch) in rest.char_indices() {
                if ch.is_whitespace() && at_word {
                    break_at = Some((index, glyphs.len(), width));
                }
                at_word = !ch.is_whitespace();
                let (glyph, kern, advance) = shape(ch, glyphs.last().map(|&(id, _)| id));
                let x = width + kern;
                if !glyphs.is_empty() && x + advance > max_width {
                    if let Some((word_end, count, word_width)) = break_at {
                        glyphs.truncate(count);
                        width = word_width;
                        next = rest[word_end..].trim_start_matches(char::is_whitespace);
                    } else {
                        next = &rest[index..];
                    }
                    break;
                }
                glyphs.push((glyph, x));
                width = x + advance;
            }
            lines.push(TitleLine { glyphs, width });
            if next.is_empty() {
                break;
            }
            rest = next;
        }
    }
    lines
}

fn write_title_image(
    output: impl std::io::Write,
    content: &str,
    TitleFont {
        size,
        color,
        family,
        bold,
    }: TitleFont<'_>,
    max_width: u32,
    raster_scale: f32,
) -> Result<(u32, u32)> {
    use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
    let (fonts, id) = title_font(
        family,
        if bold {
            fontdb::Weight::BOLD
        } else {
            fontdb::Weight::NORMAL
        },
    )?;
    fonts
        .with_face_data(id, |data, index| -> Result<(u32, u32)> {
            let font = FontRef::try_from_slice_and_index(data, index)
                .context("The title font could not be read.")?;
            let scale = PxScale::from(
                size * font.height_unscaled()
                    / font.units_per_em().unwrap_or(font.height_unscaled()),
            );
            let raster_glyph_scale = PxScale {
                x: scale.x * raster_scale,
                y: scale.y * raster_scale,
            };
            let scaled = font.as_scaled(scale);
            let pad = (size * 0.25).ceil().max(5.0);
            let line_height = (scaled.ascent() - scaled.descent() + scaled.line_gap())
                .ceil()
                .max(size);
            let cap_height = font
                .outline_glyph(
                    scaled
                        .glyph_id('H')
                        .with_scale_and_position(scale, point(0.0, 0.0)),
                )
                .map_or(size * 0.7, |glyph| glyph.px_bounds().height());
            let baseline = pad + (scaled.ascent() - scaled.descent() + cap_height) / 2.0;
            let max_lines = ((4096.0 - baseline).max(0.0) / line_height).floor() as usize + 1;
            let shape = |ch: char, prior: Option<ab_glyph::GlyphId>| {
                let glyph = scaled.glyph_id(ch);
                let kern = prior.map_or(0.0, |previous| scaled.kern(previous, glyph));
                (glyph, kern, scaled.h_advance(glyph))
            };
            let lines = title_lines(content, max_width as f32, max_lines, shape);
            let line_width = lines
                .iter()
                .fold(0.0_f32, |width, line| width.max(line.width));
            let width = (line_width + 2.0 * pad).ceil().max(2.0) as u32;
            let height = ((lines.len().saturating_sub(1) as f32) * line_height + scaled.ascent()
                - scaled.descent()
                + 2.0 * pad)
                .ceil()
                .max(2.0) as u32;
            let raster = |n: u32| (n as f32 * raster_scale).ceil().max(2.0) as u32;
            let (pixel_width, pixel_height) = (raster(width), raster(height));
            let mut rgba = vec![0_u8; (pixel_width * pixel_height * 4) as usize];
            for (row, line) in lines.into_iter().enumerate() {
                let left = pad + (line_width - line.width) / 2.0;
                let y = baseline + row as f32 * line_height;
                for (symbol, x) in line.glyphs {
                    let glyph = symbol.with_scale_and_position(
                        raster_glyph_scale,
                        point((left + x) * raster_scale, y * raster_scale),
                    );
                    let Some(glyph) = font.outline_glyph(glyph) else {
                        continue;
                    };
                    let bounds = glyph.px_bounds();
                    glyph.draw(|x, y, coverage| {
                        let x = x as i32 + bounds.min.x as i32;
                        let y = y as i32 + bounds.min.y as i32;
                        if x >= 0 && y >= 0 && (x as u32) < pixel_width && (y as u32) < pixel_height
                        {
                            let i = ((y as u32 * pixel_width + x as u32) * 4) as usize;
                            rgba[i..i + 3].copy_from_slice(&color);
                            let alpha = (coverage * 255.0) as u8;
                            rgba[i + 3] = rgba[i + 3].saturating_add(alpha.saturating_sub(
                                (u16::from(alpha) * u16::from(rgba[i + 3]) / 255) as u8,
                            ));
                        }
                    });
                }
            }
            let mut encoder = png::Encoder::new(output, pixel_width, pixel_height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&rgba)?;
            Ok((width, height))
        })
        .context("The title font could not be loaded.")?
}

#[derive(Deserialize)]
pub(crate) struct TitlePreview {
    pub width: u32,
    pub height: u32,
    pub text: String,
    pub font: String,
    pub size: f64,
    pub bold: bool,
}

fn title_pixel_size(points: f64, height: u32) -> f32 {
    (points.clamp(5.0, 600.0) * f64::from(height) / 1080.0) as f32
}

impl TitlePreview {
    pub fn render(self) -> Result<(Vec<u8>, u32, u32)> {
        let Self {
            width,
            height,
            text,
            font,
            size,
            bold,
        } = self;
        ensure!(
            (128..=7680).contains(&width) && (128..=7680).contains(&height),
            "Choose valid sequence dimensions."
        );
        ensure!(
            text.len() <= 12000 && font.len() <= 512 && size.is_finite(),
            "Choose a valid title style."
        );
        let (width, height) = (width / 2 * 2, height / 2 * 2);
        let raster_scale = (1600.0 / width.max(height) as f32).min(1.0);
        let mut bytes = Vec::new();
        let (width, height) = write_title_image(
            &mut bytes,
            &text,
            TitleFont {
                size: title_pixel_size(size, height),
                color: [255; 3],
                family: &font,
                bold,
            },
            width * 94 / 100,
            raster_scale,
        )?;
        Ok((bytes, width, height))
    }
}

async fn add_title(
    graph: &mut Composition,
    store: &Store,
    clip: &Clip,
    window: PictureWindow,
    index: usize,
    dir: &Path,
    size: (u32, u32),
    rate: &str,
) -> Result<()> {
    let (width, height) = size;
    let filename = dir.join(format!("title-{index}.png"));
    let fx = &clip.effects;
    let size = title_pixel_size(fx.transform["size"].as_f64().unwrap_or(110.0), height);
    let x = fx.transform["x"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0);
    let y = fx.transform["y"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0);
    let color = valid_color(fx.title_color(), "FFFFFF");
    let color = u32::from_str_radix(&color, 16)?.to_be_bytes();
    let bold = fx.transform["bold"].as_bool().unwrap_or(true);
    let family = fx.transform["font"].as_str().unwrap_or("System").to_owned();
    let file = filename.clone();
    let text = fx.title.clone();
    tokio::task::spawn_blocking(move || {
        title_image(
            &file,
            &text,
            size,
            [color[1], color[2], color[3]],
            &family,
            bold,
            width * 94 / 100,
        )
    })
    .await??;
    let input = graph.input(
        [
            "-loop",
            "1",
            "-framerate",
            rate,
            "-t",
            &seconds(window.shown_length),
        ],
        &filename.to_string_lossy(),
    );
    let label = format!("t{index}");
    let fade = window.alpha_filter(
        clip,
        fx.fade_in_frames,
        &format!("{width}*{x:.5}-W/2"),
        width,
    );
    let grade = grade_filter(store, &fx.grade, dir, index, true)
        .await?
        .unwrap_or_default();
    let yuv = if graph.color_managed {
        source_color::managed_yuv(true)
    } else {
        "format=yuva420p".into()
    };
    graph.filters.push(format!(
        "[{input}:v]{grade}{yuv},{fade}settb=1/{},setpts=PTS-STARTPTS+{}[{label}]",
        clock::TICKS_PER_SECOND,
        window.shown_at
    ));
    graph.filters.push(format!("[canvas{index}][{label}]overlay=x='main_w*{x:.5}-overlay_w/2':y='main_h*{y:.5}-overlay_h/2':eof_action=pass:repeatlast=0[canvas{}]",index+1));
    Ok(())
}

pub fn timed_audio_key(clip: &Clip, source: &Source) -> Option<String> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let fx = &clip.effects;
    if fx.freeze || (!fx.reverse && fx.ramp.is_null() && fx.speed == 1.0) {
        return None;
    }
    let mut hash = DefaultHasher::new();
    (
        1_u8,
        source.storage_id(),
        clip.start,
        clip.end,
        fx.speed.to_bits(),
        fx.reverse,
    )
        .hash(&mut hash);
    fx.ramp.to_string().hash(&mut hash);
    Some(format!("{:016x}", hash.finish()))
}

fn source_audio(store: &Store, source: &Source) -> PathBuf {
    let original = store.original(source.storage_id());
    if original.exists() {
        original
    } else {
        store.media(source.storage_id(), source.video)
    }
}

fn tempo_points(clip: &Clip) -> Vec<(f64, f64)> {
    let from = clock::seconds(clip.start);
    let to = clock::seconds(clip.end);
    let native = crate::retime::Ramp::read(&clip.effects.ramp)
        .map(|ramp| ramp.points(from, to, clip.effects.reverse))
        .unwrap_or_else(|| vec![(0.0, 0.0), (to - from, clock::seconds(clip.duration()))]);
    let last = native.len() - 1;
    let mut cuts = vec![native[0]];
    // atempo needs enough neighboring samples for pitch-preserving interpolation.
    // Keep the integrated timing and coalesce short intervals near ramp marks.
    for point in &native[1..last] {
        if point.1 - cuts.last().unwrap().1 >= 0.14 && native[last].1 - point.1 >= 0.10 {
            cuts.push(*point);
        }
    }
    cuts.push((native[last].0, clock::seconds(clip.duration())));
    let mut points = vec![cuts[0]];
    let reverse_block = (native[last].0 / 96.0).max(12.0);
    for pair in cuts.windows(2) {
        let [a, b] = [pair[0], pair[1]];
        // FFmpeg buffers each reversed piece. Bound that buffer even for long takes.
        let count = if clip.effects.reverse {
            ((b.0 - a.0) / reverse_block).ceil().max(1.0) as u32
        } else {
            1
        };
        for n in 1..=count {
            let u = f64::from(n) / f64::from(count);
            points.push((a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u));
        }
    }
    points
}

const AUDIO_RATE: u32 = 48_000;

fn samples(time: f64) -> u64 {
    (time.max(0.0) * f64::from(AUDIO_RATE)).round() as u64
}

async fn render_timed_audio(original: &Path, clip: &Clip, dir: &Path) -> Result<PathBuf> {
    let mut graph = Composition {
        args: vec![],
        filters: vec![],
        count: 0,
        color_managed: false,
    };
    let points = tempo_points(clip);
    let total = points.last().context("No audio timing.")?.0;
    let varies = !clip.effects.ramp.is_null();
    let mut previous = None;
    for (n, pair) in points.windows(2).enumerate() {
        let [a, b] = [pair[0], pair[1]];
        let size = samples(b.1).saturating_sub(samples(a.1));
        if size == 0 {
            continue;
        }
        let speed = (b.0 - a.0) / (b.1 - a.1);
        let overlap = if previous.is_some() && varies {
            samples(0.006).min(size / 4).min(samples(a.1))
        } else {
            0
        };
        let lead = if (speed - 1.0).abs() > 1e-5 || overlap > 0 {
            (speed * (0.11 + overlap as f64 / f64::from(AUDIO_RATE))).min(a.0)
        } else {
            0.0
        };
        let tail = if (speed - 1.0).abs() > 1e-5 {
            (speed * 0.14).min(total - b.0)
        } else {
            0.0
        };
        let first = a.0 - lead;
        let last = b.0 + tail;
        let origin = if clip.effects.reverse {
            clock::seconds(clip.end) - last
        } else {
            clock::seconds(clip.start) + first
        };
        let input = graph.input(
            [
                "-ss",
                &format!("{:.9}", origin.max(0.0)),
                "-t",
                &format!("{:.9}", last - first),
            ],
            &original.to_string_lossy(),
        );
        let skip = samples(lead / speed).saturating_sub(overlap);
        let filtered = format!("part{n}");
        let backwards = if clip.effects.reverse {
            ",areverse"
        } else {
            ""
        };
        let time_stretch = if (speed - 1.0).abs() > 1e-5 {
            format!(",{}", tempo(speed))
        } else {
            String::new()
        };
        let stop = skip + size + overlap;
        graph.filters.push(format!(
            "[{input}:a:0]aresample={AUDIO_RATE},aformat=channel_layouts=stereo{backwards}{time_stretch},apad=whole_len={stop},atrim=start_sample={skip}:end_sample={stop},asetpts=N/SR/TB[{filtered}]"
        ));
        previous = Some(if let Some(last) = previous {
            let label = format!("joined{n}");
            let join = if overlap == 0 {
                "concat=v=0:a=1:n=2".into()
            } else {
                format!("acrossfade=ns={overlap}:c1=tri:c2=tri")
            };
            graph
                .filters
                .push(format!("[{last}][{filtered}]{join}[{label}]"));
            label
        } else {
            filtered
        });
    }
    let stream = previous.context("The selected audio range is too short.")?;
    let script = dir.join("audio.ffgraph");
    fs::write(
        &script,
        graph.filters.join(
            ";
",
        ),
    )
    .await?;
    let output = dir.join("timed.flac");
    let args = [
        "-protocol_whitelist",
        "file",
        "-filter_complex_threads",
        "1",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(graph.args)
    .chain([
        "-filter_complex_script".into(),
        script.to_string_lossy().into_owned(),
        "-map".into(),
        format!("[{stream}]"),
        "-vn".into(),
        "-c:a".into(),
        "flac".into(),
        "-sample_fmt".into(),
        "s32".into(),
        output.to_string_lossy().into_owned(),
    ]);
    ffmpeg(args).await?;
    Ok(output)
}

pub async fn timed_audio(store: &Store, source: &Source, clip: &Clip) -> Result<Option<PathBuf>> {
    let Some(key) = timed_audio_key(clip, source) else {
        return Ok(None);
    };
    let path = store.root.join("audio-cache").join(format!("{key}.flac"));
    if path.is_file() {
        return Ok(Some(path));
    }
    let _slot = store.audio_render_slot.lock().await;
    if path.is_file() {
        return Ok(Some(path));
    }
    fs::create_dir_all(path.parent().unwrap()).await?;
    let dir = temp(store).await?;
    let result = async {
        let rendered = render_timed_audio(&source_audio(store, source), clip, &dir).await?;
        fs::rename(rendered, &path).await?;
        Ok(Some(path))
    }
    .await;
    let _ = fs::remove_dir_all(dir).await;
    result
}

struct AudioEntry<'a> {
    source: &'a Source,
    clip: &'a Clip,
    at: u32,
    gain: f64,
}

async fn add_sound(
    graph: &mut Composition,
    store: &Store,
    sound: AudioEntry<'_>,
    index: usize,
) -> Result<String> {
    let clip = sound.clip;
    let fx = &clip.effects;
    let duration = clip.duration();
    let id = if let Some(file) = timed_audio(store, sound.source, clip).await? {
        graph.input([], &file.to_string_lossy())
    } else {
        graph.input(
            [
                "-ss",
                &seconds(clip.start),
                "-t",
                &seconds(clip.end - clip.start),
            ],
            &source_audio(store, sound.source).to_string_lossy(),
        )
    };
    let label = format!("a{index}");
    let gain = sound.gain;
    let mut filter = format!(
        "[{id}:a:0]aresample={AUDIO_RATE},aformat=channel_layouts=stereo,asetpts=N/SR/TB,volume={gain:.6}dB"
    );
    let head = fx.fade_in_frames.min(duration);
    let tail = fx.fade_out_frames.min(duration);
    if (head > 0 && fx.fade_in_curve != FadeCurve::Linear)
        || (tail > 0 && fx.fade_out_curve != FadeCurve::Linear)
        || u64::from(head) + u64::from(tail) > u64::from(duration)
    {
        let first = fade_expression("t", fx.fade_in_curve, duration, head, true);
        let last = fade_expression("t", fx.fade_out_curve, duration, tail, false);
        filter.push_str(&format!(",aeval=exprs='val(0)*min({first},{last})|val(1)*min({first},{last})':channel_layout=stereo"));
    } else {
        if head > 0 {
            filter.push_str(&format!(",afade=t=in:st=0:d={}", seconds(head)));
        }
        if tail > 0 {
            filter.push_str(&format!(
                ",afade=t=out:st={}:d={}",
                seconds(duration - tail),
                seconds(tail)
            ));
        }
    }
    let delay = u64::from(sound.at) * u64::from(AUDIO_RATE) / u64::from(clock::TICKS_PER_SECOND);
    filter.push_str(&format!(",adelay={delay}S:all=1[{label}]"));
    graph.filters.push(filter);
    Ok(label)
}

pub struct ExportObserver<'a> {
    pub stop: &'a CancellationToken,
    pub progress: &'a (dyn Fn(f64) + Sync),
}

fn check_export(observer: Option<&ExportObserver<'_>>) -> Result<()> {
    ensure!(
        !observer.is_some_and(|observer| observer.stop.is_cancelled()),
        "Export cancelled."
    );
    Ok(())
}

pub async fn export(
    store: &Store,
    project: &Project,
    dir: &Path,
    options: &ExportOptions,
) -> Result<PathBuf> {
    Box::pin(export_impl(
        store,
        project,
        dir,
        options,
        RenderTarget::Export,
        None,
    ))
    .await
}

pub async fn observed_export(
    store: &Store,
    project: &Project,
    dir: &Path,
    options: &ExportOptions,
    observer: &ExportObserver<'_>,
) -> Result<PathBuf> {
    export_impl(
        store,
        project,
        dir,
        options,
        RenderTarget::Export,
        Some(observer),
    )
    .await
}

pub async fn continuation_frame(
    store: &Store,
    project: &Project,
    clip: &Clip,
    dir: &Path,
) -> Result<(PathBuf, PathBuf)> {
    ensure!(
        clip.kind == ClipKind::Video,
        "Use a previously placed video clip as the starting frame."
    );
    ensure!(
        !project.library.missing.contains_key(&clip.source),
        "Locate the starting clip's media before continuing it."
    );
    let mut isolated = project.clone();
    let step = isolated.frame_step();
    let mut clip = clip.clone();
    // Keep the clip on the same frame grid without rendering the preceding gap.
    let position = project.placement(clip.id).unwrap_or(0) % step;
    let at = (position + clip.duration().saturating_sub(1)) / step * step;
    ensure!(at >= position, "The clip has no visible frame.");
    clip.position = Some(position);
    isolated.timeline = vec![clip];
    isolated.sequence.captions = None;
    let dir = dir.join(format!("anchor-{}", Uuid::new_v4()));
    fs::create_dir(&dir).await?;
    let image = export_impl(
        store,
        &isolated,
        &dir,
        &ExportOptions::default(),
        RenderTarget::Frame {
            at,
            max_dimension: 1344,
        },
        None,
    )
    .await?;
    let thumbnail = dir.join("anchor.jpg");
    extract(&image, 0, &thumbnail, true).await?;
    Ok((image, thumbnail))
}

async fn add_export_mix(
    graph: &mut Composition,
    store: &Store,
    project: &Project,
    dir: &Path,
    range: (u32, u32),
    measure: bool,
    observer: Option<&ExportObserver<'_>>,
) -> Result<()> {
    let duration = project.duration();
    let length = seconds(duration);
    let (from, to) = range;
    check_export(observer)?;
    let silent = graph.input(["-f", "lavfi", "-t", &length], "anullsrc=r=48000:cl=stereo");
    graph.filters.push(format!("[{silent}:a]anull[bed]"));
    let mut mix = vec!["bed".to_string()];
    let solo = project
        .sequence
        .tracks
        .iter()
        .any(|track| track.name.starts_with('A') && track.solo);
    let mut ducking = crate::audio::mix(store, project)
        .await?
        .into_iter()
        .map(|(name, curve)| (name, (curve, vec![])))
        .collect::<std::collections::BTreeMap<_, _>>();
    for (index, clip) in project.timeline.iter().enumerate() {
        check_export(observer)?;
        let Some(source) = project.source(clip.source) else {
            continue;
        };
        if !source.audio
            || clip.mute
            || clip.effects.freeze
            || project.library.missing.contains_key(&source.id)
        {
            continue;
        }
        let track = project
            .sequence
            .tracks
            .iter()
            .find(|track| track.name == clip.track())
            .context("Audio track is missing.")?;
        let gain = if clip.kind == ClipKind::Audio {
            if !crate::audio::audible(project, track) {
                continue;
            }
            clip.effects.gain_db + track.gain_db
        } else {
            if !matches!(clip.kind, ClipKind::Video)
                || clip.link.is_some()
                || clip.audio_detached
                || solo
            {
                continue;
            }
            clip.effects.gain_db
        };
        let entry = AudioEntry {
            source,
            clip,
            at: project.placement(clip.id).unwrap_or(0),
            gain,
        };
        let label = add_sound(graph, store, entry, index).await?;
        if let Some((_, labels)) = ducking.get_mut(clip.track()) {
            labels.push(label);
        } else {
            mix.push(label);
        }
    }
    for (index, (curve, labels)) in ducking.into_values().enumerate() {
        if labels.is_empty() {
            continue;
        }
        let path = dir.join(format!("duck-{index}.f32"));
        curve.write_control(&path, duration).await?;
        let input = graph.input(
            ["-f", "f32le", "-ar", "200", "-ac", "1"],
            &path.to_string_lossy(),
        );
        let track = format!("ducktrack{index}");
        let sources = labels.join("][");
        graph.filters.push(format!(
            "[{sources}]amix=inputs={}:normalize=0:duration=longest[{track}]",
            labels.len()
        ));
        let envelope = format!("duckenv{index}");
        graph.filters.push(format!(
            "[{input}:a]aresample=48000,pan=stereo|c0=c0|c1=c0[{envelope}]"
        ));
        let output = format!("ducked{index}");
        graph.filters.push(format!(
            "[{track}][{envelope}]amultiply,atrim=end={length}[{output}]"
        ));
        mix.push(output);
    }
    if let Some(id) = project
        .soundtrack
        .filter(|id| !project.library.missing.contains_key(id))
    {
        let src = project.source(id).context("Soundtrack missing.")?;
        let mut clip = Clip::from_source(src);
        clip.end = src.duration.min(duration);
        let entry = AudioEntry {
            source: src,
            clip: &clip,
            at: 0,
            gain: -4.4,
        };
        mix.push(add_sound(graph, store, entry, project.timeline.len()).await?);
    }
    let (limiter, output) = if measure {
        ("", "export_mix")
    } else {
        ("alimiter=limit=0.95:latency=1,", "outa")
    };
    graph.filters.push(format!("[{}]amix=inputs={}:duration=first:normalize=0,{limiter}atrim=start={}:end={},asetpts=PTS-STARTPTS,aresample=48000[{output}]",
        mix.join("]["), mix.len(), seconds(from), seconds(to)));
    Ok(())
}

const LOUDNESS_PROGRESS: f64 = 0.15;

fn export_command(dir: &Path, inputs: &[String], script: &str) -> Command {
    let mut cmd = Command::new("ffmpeg");
    cmd.current_dir(dir)
        .args([
            "-hide_banner",
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-filter_complex_threads",
            "2",
        ])
        .args(inputs)
        .args(["-filter_complex_script", script])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    cmd
}

async fn measure_export_gain(
    graph: &Composition,
    dir: &Path,
    duration: u32,
    target: f64,
    observer: Option<&ExportObserver<'_>>,
) -> Result<f64> {
    check_export(observer)?;
    let script = "measure-audio.ffgraph";
    fs::write(dir.join(script), graph.filters.join(";\n")).await?;
    let mut child = export_command(dir, &graph.args, script)
        .args([
            "-map",
            "[export_mix]",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-c:a",
            "pcm_f32le",
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("FFmpeg is required to measure export loudness.")?;
    let mut output = child.stdout.take().expect("export audio measurement pipe");
    let collect = async {
        let mut meter = loudness::Meter::default();
        let mut buffer = [0; 65536];
        let mut remainder = 0;
        let expected = clock::seconds(duration) * 48000.0;
        loop {
            let read = output.read(&mut buffer[remainder..]).await?;
            if read == 0 {
                ensure!(
                    remainder == 0,
                    "The export audio meter received a partial sample."
                );
                return Ok(meter.gain(target).unwrap_or_default());
            }
            let size = read + remainder;
            let consumed = meter.read(&buffer[..size]);
            buffer.copy_within(consumed..size, 0);
            remainder = size - consumed;
            if let Some(observer) = observer {
                (observer.progress)(
                    LOUDNESS_PROGRESS * (meter.frames() as f64 / expected).min(1.0),
                );
            }
        }
    };
    let finished = timeout(Duration::from_secs(900), async {
        tokio::try_join!(collect, async { Ok(child.wait().await?) })
    });
    let result = if let Some(observer) = observer {
        tokio::select! {
            biased;
            _ = observer.stop.cancelled() => None,
            result = finished => Some(result),
        }
    } else {
        Some(finished.await)
    };
    match result {
        Some(Ok(Ok((gain, status)))) => {
            ensure!(status.success(), "FFmpeg could not measure the export mix.");
            if let Some(observer) = observer {
                (observer.progress)(LOUDNESS_PROGRESS);
            }
            Ok(gain)
        }
        other => {
            let _ = child.kill().await;
            match other {
                None => bail!("Export cancelled."),
                Some(Err(_)) => bail!("Measuring the export mix timed out."),
                Some(Ok(Err(error))) => Err(error).context("Could not read the export mix."),
                _ => unreachable!(),
            }
        }
    }
}

enum RenderTarget {
    Export,
    Frame { at: u32, max_dimension: u32 },
}

async fn export_impl(
    store: &Store,
    project: &Project,
    dir: &Path,
    options: &ExportOptions,
    target: RenderTarget,
    observer: Option<&ExportObserver<'_>>,
) -> Result<PathBuf> {
    check_export(observer)?;
    let duration = project.duration();
    ensure!(duration > 0, "Add clips before exporting.");
    let still = matches!(target, RenderTarget::Frame { .. });
    let (from, to) = if let RenderTarget::Frame { at, .. } = target {
        ensure!(
            at < duration,
            "The requested frame is outside the timeline."
        );
        (at, (at + project.frame_step()).min(duration))
    } else if options.range_only == Some(true) {
        let (from, to) = project
            .play_range()
            .context("Set a valid in/out range first.")?;
        let to = to.min(duration);
        ensure!(to > from, "The marked range must overlap the timeline.");
        (from, to)
    } else {
        (0, duration)
    };
    let wav = !still && options.format.as_deref() == Some("audio");
    let length = seconds(duration);
    let (width, height, mut out_w, mut out_h) = fit_size(project, options)?;
    if let RenderTarget::Frame { max_dimension, .. } = target {
        let scale = (f64::from(max_dimension) / f64::from(width.max(height))).min(1.0);
        let resize = |side| ((f64::from(side) * scale / 2.0).round() as u32 * 2).max(2);
        (out_w, out_h) = (resize(width), resize(height));
    }
    let mut profiles = std::collections::HashMap::new();
    let transitions = crate::transition::layout(project);
    if !wav {
        for clip in project.timeline.iter().filter(|clip| {
            clip.kind == ClipKind::Video && !project.library.missing.contains_key(&clip.source)
        }) {
            let at = project.placement(clip.id).unwrap_or(0);
            let plan = transitions.plan(clip.id);
            if plan.render_start(at) >= to
                || plan.render_end(at, clip.duration()) <= from
                || !project
                    .sequence
                    .tracks
                    .iter()
                    .any(|track| track.name == clip.track() && !track.hidden)
            {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = profiles.entry(clip.source)
                && let Some(source) = project.source(clip.source)
            {
                check_export(observer)?;
                entry.insert(source_color::export_profile(store, source).await?);
            }
        }
    }
    let mut graph = Composition {
        args: vec![],
        filters: vec![],
        count: 0,
        color_managed: profiles.values().any(|color| color.managed_output()),
    };
    let use_audio = !still && (wav || options.include_audio.unwrap_or(true));
    let target_lufs = options.loudness.filter(|_| use_audio);
    if use_audio {
        add_export_mix(
            &mut graph,
            store,
            project,
            dir,
            (from, to),
            target_lufs.is_some(),
            observer,
        )
        .await?;
    }
    if let Some(target) = target_lufs {
        let gain = measure_export_gain(&graph, dir, to - from, target.clamp(-40.0, -5.0), observer)
            .await?;
        graph
            .filters
            .push(format!("[export_mix]volume={gain:.9}dB[outa]"));
    }
    let mut canvas = 0;
    let rate = framerate(project);
    if !wav {
        let base = graph.input(
            ["-f", "lavfi"],
            &format!("color=c=black:size={width}x{height}:rate={rate}:duration={length}"),
        );
        let signal = if graph.color_managed {
            format!("{},", source_color::SDR_SIGNAL)
        } else {
            String::new()
        };
        graph
            .filters
            .push(format!("[{base}:v]{signal}setsar=1[canvas0]"));
        for track in project
            .sequence
            .tracks
            .iter()
            .filter(|track| track.name.starts_with('V') && !track.hidden)
        {
            let mut clips = project
                .timeline
                .iter()
                .filter(|clip| {
                    clip.track() == track.name
                        && !project.library.missing.contains_key(&clip.source)
                })
                .map(|clip| (project.placement(clip.id).unwrap_or(0), clip))
                .collect::<Vec<_>>();
            clips.sort_by_key(|(pos, _)| *pos);
            for (at, clip) in clips {
                check_export(observer)?;
                let plan = transitions.plan(clip.id);
                if plan.render_start(at) >= to || plan.render_end(at, clip.duration()) <= from {
                    continue;
                }
                let window = PictureWindow::new(project, clip, at, plan);
                if clip.kind == ClipKind::Text {
                    add_title(
                        &mut graph,
                        store,
                        clip,
                        window,
                        canvas,
                        dir,
                        (width, height),
                        &rate,
                    )
                    .await?;
                } else {
                    add_picture(
                        &mut graph,
                        store,
                        clip,
                        project.source(clip.source),
                        window,
                        canvas,
                        width,
                        height,
                        dir,
                        &rate,
                        profiles.get(&clip.source).copied().unwrap_or_default(),
                    )
                    .await?;
                }
                canvas += 1;
            }
        }
        let x = options.reframe_x.unwrap_or(0.5).clamp(0.0, 1.0);
        let y = options.reframe_y.unwrap_or(0.5).clamp(0.0, 1.0);
        check_export(observer)?;
        let overlay = if let Some(track) = project.sequence.captions.as_ref().filter(|t| !t.hidden)
        {
            let blocks = crate::captions::blocks(project, store);
            let track = track.clone();
            let dir = dir.to_owned();
            let step = project.frame_step();
            tokio::task::spawn_blocking(move || {
                crate::caption_export::write(
                    &blocks,
                    &track,
                    crate::caption_export::Export {
                        from,
                        to,
                        step,
                        width: out_w,
                        height: out_h,
                        dir: &dir,
                    },
                )
            })
            .await??
        } else {
            None
        };
        let base_label = if overlay.is_some() {
            "caption_base"
        } else {
            "outv"
        };
        graph.filters.push(format!(
            "[canvas{canvas}]settb=1/{},trim=start_pts={from}:end_pts={to},setpts=PTS-STARTPTS,scale={out_w}:{out_h}:force_original_aspect_ratio=increase:force_divisible_by=2,crop={out_w}:{out_h}:x='clip(iw*{x:.8}-ow/2,0,iw-ow)':y='clip(ih*{y:.8}-oh/2,0,ih-oh)',setsar=1,fps={},format=yuv420p[{base_label}]",
            clock::TICKS_PER_SECOND,rate));
        if let Some(overlay) = overlay {
            let stream = graph.input(["-f", "concat", "-safe", "0"], "captions.ffconcat");
            let overlay_source = if graph.color_managed {
                graph.filters.push(format!(
                    "[{stream}:v]{}[caption_sdr]",
                    source_color::managed_yuv(true)
                ));
                "caption_sdr".into()
            } else {
                format!("{stream}:v")
            };
            graph.filters.push(format!("[caption_base][{overlay_source}]overlay=x='(main_w-overlay_w)/2':y={}:eof_action=pass:repeatlast=0[outv]",overlay.y));
        }
    }
    let color_managed = graph.color_managed;
    let script = dir.join("render.ffgraph");
    fs::write(&script, graph.filters.join(";\n")).await?;
    let filename = match target {
        RenderTarget::Frame { .. } => "editroom.png",
        RenderTarget::Export if wav => "editroom.wav",
        RenderTarget::Export => "editroom.mp4",
    };
    let output = dir.join(filename);
    let mut cmd = export_command(dir, &graph.args, "render.ffgraph");
    if still {
        cmd.args([
            "-map",
            "[outv]",
            "-c:v",
            "png",
            "-pix_fmt",
            "rgb24",
            "-frames:v",
            "1",
            "-update",
            "1",
        ]);
    } else if !wav {
        let codec = match options.codec.as_deref().unwrap_or("h264") {
            "h264" => "libx264",
            "hevc" => "libx265",
            _ => bail!("Choose H.264 or HEVC."),
        };
        let web = options.quality.as_deref() == Some("web");
        let bits_per_pixel = if web { 0.085 } else { 0.2 };
        let bitrate = f64::from(out_w)
            * f64::from(out_h)
            * project.sequence.fps.min(60.0)
            * bits_per_pixel
            * if codec == "libx265" { 0.62 } else { 1.0 };
        cmd.args([
            "-map",
            "[outv]",
            "-c:v",
            codec,
            "-preset",
            "veryfast",
            "-threads",
            "4",
            "-crf",
            if web { "25" } else { "18" },
            "-maxrate",
            &((bitrate * 2.0) as u64).to_string(),
            "-bufsize",
            &((bitrate * 4.0) as u64).to_string(),
            "-g",
            &((project.sequence.fps * 2.0).round() as u32).to_string(),
            "-movflags",
            "+faststart",
        ]);
        if codec == "libx265" {
            cmd.args(["-tag:v", "hvc1"]);
        }
        if color_managed {
            cmd.args(source_color::SDR_TAGS);
        }
    }
    if use_audio {
        cmd.args([
            "-map",
            "[outa]",
            "-c:a",
            if wav { "pcm_s24le" } else { "aac" },
        ]);
        if !wav {
            cmd.args([
                "-b:a",
                if options.quality.as_deref() == Some("web") {
                    "160k"
                } else {
                    "256k"
                },
            ]);
        }
    }
    check_export(observer)?;
    if let Some(observer) = observer {
        let completed = if target_lufs.is_some() {
            LOUDNESS_PROGRESS
        } else {
            0.0
        };
        encode_with_progress(&mut cmd, filename, to - from, completed, observer).await?;
    } else {
        let result = timeout(
            Duration::from_secs(900),
            cmd.arg(filename).stdout(Stdio::null()).output(),
        )
        .await?
        .context("FFmpeg is required for export.")?;
        finish_encoder(result.status, &result.stderr)?;
        if still {
            ensure!(
                output.is_file(),
                "A frame was missing from the rendered clip."
            );
        }
    }
    Ok(output)
}

async fn encode_with_progress(
    cmd: &mut Command,
    filename: &str,
    duration: u32,
    completed: f64,
    observer: &ExportObserver<'_>,
) -> Result<()> {
    let mut child = cmd
        .args(["-stats_period", "0.25", "-progress", "pipe:1", "-nostats"])
        .arg(filename)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("FFmpeg is required for export.")?;
    let mut output = BufReader::new(child.stdout.take().expect("FFmpeg progress pipe")).lines();
    let mut errors = child.stderr.take().expect("FFmpeg error pipe");
    let report = async {
        while let Some(line) = output.next_line().await? {
            if let Some(us) = line
                .strip_prefix("out_time_us=")
                .and_then(|value| value.parse::<u64>().ok())
            {
                let encode_fraction = us as f64 / (clock::seconds(duration) * 1_000_000.0);
                (observer.progress)(completed + (1.0 - completed) * encode_fraction);
            }
        }
        Ok::<_, std::io::Error>(())
    };
    let collect_errors = async {
        let mut log = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = errors.read(&mut buffer).await?;
            if count == 0 {
                return Ok::<_, std::io::Error>(log);
            }
            let keep = count.min(4096_usize.saturating_sub(log.len()));
            log.extend_from_slice(&buffer[..keep]);
        }
    };
    let result = tokio::select! {
        biased;
        _ = observer.stop.cancelled() => None,
        result = timeout(Duration::from_secs(900), async {
            tokio::try_join!(report, collect_errors, child.wait())
        }) => Some(result),
    };
    match result {
        Some(Ok(Ok((_, errors, status)))) => finish_encoder(status, &errors),
        other => {
            let _ = child.kill().await;
            match other {
                None => bail!("Export cancelled."),
                Some(Err(_)) => bail!("The export timed out."),
                Some(Ok(Err(error))) => {
                    Err(error).context("Could not read FFmpeg export progress.")
                }
                _ => unreachable!(),
            }
        }
    }
}

fn finish_encoder(status: std::process::ExitStatus, errors: &[u8]) -> Result<()> {
    if !status.success() {
        eprintln!(
            "EditCastle export failed: {}",
            String::from_utf8_lossy(errors)
                .chars()
                .take(1000)
                .collect::<String>()
        );
        bail!("FFmpeg could not render this timeline.");
    }
    Ok(())
}

pub async fn speech_audio(path: &Path, output: &Path) -> Result<()> {
    ffmpeg([
        "-protocol_whitelist",
        "file",
        "-i",
        path.to_str().unwrap(),
        "-map",
        "0:a:0",
        "-ar",
        "16000",
        "-ac",
        "1",
        "-c:a",
        "pcm_s16le",
        output.to_str().unwrap(),
    ])
    .await
}

pub async fn waveform(path: &Path, output: &Path) -> Result<()> {
    ffmpeg([
        "-protocol_whitelist",
        "file",
        "-i",
        path.to_str().unwrap(),
        "-filter_complex",
        "aformat=channel_layouts=mono,showwavespic=s=1010x70:colors=0xBFCED2:scale=sqrt",
        "-frames:v",
        "1",
        "-an",
        output.to_str().unwrap(),
    ])
    .await?;
    ensure!(output.exists(), "This file has no audio waveform.");
    Ok(())
}
