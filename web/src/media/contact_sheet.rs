use super::{extract_still_at, ffmpeg, picture_format, title_image};
use crate::{
    clock,
    editor::{Source, Store},
    source_color,
};
use anyhow::{Context, Result, ensure};
use std::{io::Cursor, path::Path};
use tokio::fs;
use uuid::Uuid;

const GAP: u32 = 4;
const JPEG: &str = "scale=out_color_matrix=bt601:out_range=full,format=yuvj420p";

pub(crate) struct ContactSheet {
    pub jpeg: Vec<u8>,
    pub times: Vec<u32>,
    pub columns: usize,
    pub rows: usize,
}

pub(crate) async fn source_contact_sheet(
    store: &Store,
    source: &Source,
    frame_count: usize,
    explicit: Vec<f64>,
    work: &Path,
) -> Result<ContactSheet> {
    let dir = work.join(format!("contact-{}", Uuid::new_v4()));
    fs::create_dir(&dir).await?;
    let result = if source.image {
        photo(store, source, &dir).await
    } else {
        video_sheet(store, source, frame_count, explicit, &dir).await
    };
    let _ = fs::remove_dir_all(dir).await;
    result
}

async fn photo(store: &Store, source: &Source, dir: &Path) -> Result<ContactSheet> {
    let original = store.original(source.storage_id());
    let input = if original.is_file() {
        original
    } else {
        source_color::playback(store, source).await?
    };
    let output = dir.join("photo.jpg");
    let filter = format!("{},{JPEG}", fit_filter(768));
    ffmpeg([
        "-protocol_whitelist",
        "file",
        "-i",
        input.to_str().context("Invalid media path.")?,
        "-vf",
        &filter,
        "-frames:v",
        "1",
        "-an",
        "-q:v",
        "3",
        output.to_str().context("Invalid preview path.")?,
    ])
    .await?;
    Ok(ContactSheet {
        jpeg: fs::read(output).await?,
        times: vec![],
        columns: 1,
        rows: 1,
    })
}

fn fit_filter(max: u32) -> String {
    format!(
        "scale=w='min(iw,{max})':h='min(ih,{max})':force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1"
    )
}

fn sample_times(duration: u32, count: usize, explicit: Vec<f64>) -> Vec<u32> {
    if explicit.is_empty() {
        return (0..count)
            .map(|i| (u64::from(duration) * (2 * i + 1) as u64 / (2 * count) as u64) as u32)
            .collect();
    }
    let limit = duration.saturating_sub(clock::ticks(0.05));
    let mut wanted = explicit
        .into_iter()
        .take(12)
        .map(|time| clock::ticks(time).min(limit))
        .collect::<Vec<_>>();
    wanted.sort_unstable();
    wanted
}

async fn video_sheet(
    store: &Store,
    source: &Source,
    frame_count: usize,
    explicit: Vec<f64>,
    dir: &Path,
) -> Result<ContactSheet> {
    ensure!(source.duration > 0, "This media has no displayable frames.");
    let max_side = picture_format(store, source)
        .await
        .map_or(360, |format| format.width.max(format.height).min(360));
    let filter = format!(",{},format=rgb24", fit_filter(max_side));
    let input = source_color::playback(store, source).await?;
    let mut pictures = Vec::new();
    for (index, time) in sample_times(source.duration, frame_count, explicit)
        .into_iter()
        .enumerate()
    {
        let picture = dir.join(format!("frame-{index}.png"));
        if extract_still_at(&input, time, &picture, &filter)
            .await
            .is_ok()
        {
            pictures.push((time, picture));
        }
    }
    let (_, first) = pictures
        .first()
        .context("Could not inspect any frames in this media.")?;
    let (width, height) = {
        let reader = png::Decoder::new(Cursor::new(fs::read(first).await?)).read_info()?;
        (reader.info().width, reader.info().height)
    };
    let columns = match pictures.len() {
        1..=4 => pictures.len(),
        5..=6 => 3,
        _ => 4,
    };
    let rows = pictures.len().div_ceil(columns);
    let mut args = vec!["-filter_complex_threads".to_owned(), "1".to_owned()];
    let mut filters = Vec::new();
    for (i, (at, picture)) in pictures.iter().enumerate() {
        let label = dir.join(format!("timestamp-{i}.png"));
        let time = format!("{:.1}s", clock::seconds(*at));
        title_image(&label, &time, 15.0, [255; 3], "Menlo", true, 160)?;
        for path in [picture, &label] {
            let name = path.to_str().context("Invalid preview path.")?;
            args.extend([
                "-protocol_whitelist".into(),
                "file".into(),
                "-i".into(),
                name.into(),
            ]);
        }
        let box_width = time.len() * 10 + 10;
        filters.push(format!(
            "[{}:v]scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=0x191919,setsar=1,drawbox=x=4:y=4:w={box_width}:h=26:color=black@0.65:t=fill[frame{i}];[frame{i}][{}:v]overlay=4:4:format=rgb[cell{i}]",
            i * 2, i * 2 + 1
        ));
    }
    if pictures.len() == 1 {
        filters.push(format!("[cell0]{JPEG}[sheet]"));
    } else {
        let inputs = (0..pictures.len())
            .map(|i| format!("[cell{i}]"))
            .collect::<String>();
        let layout = (0..pictures.len())
            .map(|i| {
                format!(
                    "{}_{}",
                    (i % columns) as u32 * (width + GAP),
                    (i / columns) as u32 * (height + GAP)
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        filters.push(format!(
            "{inputs}xstack=inputs={}:layout={layout}:fill=0x191919[grid];[grid]{JPEG}[sheet]",
            pictures.len()
        ));
    }
    let output = dir.join("sheet.jpg");
    args.extend([
        "-filter_complex".into(),
        filters.join(";"),
        "-map".into(),
        "[sheet]".into(),
        "-frames:v".into(),
        "1".into(),
        "-q:v".into(),
        "3".into(),
        output.to_str().context("Invalid preview path.")?.into(),
    ]);
    ffmpeg(args).await?;
    Ok(ContactSheet {
        jpeg: fs::read(output).await?,
        times: pictures.into_iter().map(|(time, _)| time).collect(),
        columns,
        rows,
    })
}
