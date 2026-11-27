mod quicktime;

use crate::{
    editor::{Source, Store, private_write},
    media,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use uuid::Uuid;

const GRID: u32 = 64;
pub const SDR_TAGS: &[&str] = &[
    "-color_range",
    "tv",
    "-colorspace",
    "bt709",
    "-color_primaries",
    "bt709",
    "-color_trc",
    "bt709",
];
pub const SDR_SIGNAL: &str =
    "setparams=range=limited:colorspace=bt709:color_primaries=bt709:color_trc=bt709";
const RGB_SIGNAL: &str =
    "setparams=range=full:colorspace=gbr:color_primaries=bt709:color_trc=bt709";

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Profile {
    #[default]
    Standard,
    Hlg,
    Pq,
    AppleLog,
    AppleLog2,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Matrix {
    #[default]
    Rec2020,
    Rec709,
    Rec601,
    Smpte240,
}

impl Matrix {
    fn scale(self) -> &'static str {
        match self {
            Self::Rec2020 => "bt2020",
            Self::Rec709 => "bt709",
            Self::Rec601 => "bt601",
            Self::Smpte240 => "smpte240m",
        }
    }
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct SourceColor {
    profile: Profile,
    matrix: Matrix,
    full_range: bool,
    rgb: bool,
    #[serde(default)]
    pub baked: bool,
}

impl SourceColor {
    pub fn needs_conversion(self) -> bool {
        self.profile != Profile::Standard
    }

    pub fn managed_output(self) -> bool {
        self.needs_conversion() || self.baked
    }

    pub async fn inspect(path: &Path) -> Result<Self> {
        let data = media::details(path).await?;
        Self::inspect_details(path, &data).await
    }

    pub(crate) async fn inspect_details(path: &Path, data: &Value) -> Result<Self> {
        let video = data["streams"].as_array().and_then(|streams| {
            streams.iter().find(|stream| {
                stream["codec_type"] == "video" && stream["disposition"]["attached_pic"] != 1
            })
        });
        let Some(video) = video else {
            return Ok(Self::default());
        };
        let tag = |name: &str| video[name].as_str().unwrap_or("");
        let found = apple_tags(video).or_else(|| apple_tags(&data["format"]));
        let transfer = tag("color_transfer");
        let apple = if found.is_none()
            && data["format"]["format_name"]
                .as_str()
                .is_some_and(|format| format.split(',').any(|name| matches!(name, "mov" | "mp4")))
        {
            let path = path.to_owned();
            tokio::task::spawn_blocking(move || {
                quicktime::log_profile(&path).or_else(|| apple_track(&path))
            })
            .await?
        } else {
            None
        };
        let profile = found.or(apple).unwrap_or(match transfer {
            "arib-std-b67" => Profile::Hlg,
            "smpte2084" => Profile::Pq,
            _ => Profile::Standard,
        });
        let matrix = match tag("color_space") {
            "bt709" => Matrix::Rec709,
            "bt470bg" | "smpte170m" | "fcc" => Matrix::Rec601,
            "smpte240m" => Matrix::Smpte240,
            _ => Matrix::Rec2020,
        };
        let pixel = tag("pix_fmt");
        Ok(Self {
            profile,
            matrix,
            full_range: matches!(tag("color_range"), "pc" | "jpeg"),
            rgb: tag("color_space") == "gbr"
                || ["gbr", "rgb", "bgr", "rgba", "argb", "abgr"]
                    .iter()
                    .any(|prefix| pixel.starts_with(prefix)),
            baked: false,
        })
    }

    pub async fn rgb_filter(self, dir: &Path) -> Result<Option<String>> {
        if !self.needs_conversion() {
            return Ok(None);
        }
        let name = self.profile.cube_in(dir).await?;
        let input = if self.rgb {
            "format=gbrp16le".into()
        } else {
            let range = if self.full_range { "full" } else { "limited" };
            format!(
                "scale=in_color_matrix={}:in_range={range}:out_range=full,format=gbrp16le",
                self.matrix.scale()
            )
        };
        Ok(Some(format!(
            "{input},lut3d=file={name}:interp=trilinear,{RGB_SIGNAL},sidedata=mode=delete"
        )))
    }

    pub async fn proxy_filter(self, dir: &Path) -> Result<Option<String>> {
        Ok(self.rgb_filter(dir).await?.map(|input| format!("{input},scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2:out_color_matrix=bt709:out_range=limited,format=yuv420p,{SDR_SIGNAL}")))
    }
}

fn apple_log(value: &str) -> Option<Profile> {
    let value = value.trim().to_ascii_lowercase();
    if matches!(
        value.as_str(),
        "com.apple.apple-wide-gamut.apple-log" | "apple-log2" | "apple-log-2" | "apple log 2"
    ) {
        Some(Profile::AppleLog2)
    } else if matches!(
        value.as_str(),
        "com.apple.rec2020.apple-log" | "apple-log" | "apple log"
    ) {
        Some(Profile::AppleLog)
    } else {
        None
    }
}

fn apple_tags(properties: &Value) -> Option<Profile> {
    properties["tags"]
        .as_object()?
        .iter()
        .find_map(|(name, value)| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "logtransferfunction"
                    | "log_transfer_function"
                    | "com.apple.quicktime.log-transfer-function"
                    | "com.apple.quicktime.logtransferfunction"
            )
            .then(|| apple_log(value.as_str()?))
            .flatten()
        })
}

#[cfg(target_os = "macos")]
fn apple_track(path: &Path) -> Option<Profile> {
    use objc2::{
        msg_send,
        rc::{Retained, autoreleasepool},
        runtime::{AnyClass, AnyObject},
    };
    use objc2_foundation::{NSArray, NSDictionary, NSString, NSURL};
    use std::ffi::c_void;

    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {
        static AVMediaTypeVideo: *const NSString;
    }
    #[link(name = "CoreMedia", kind = "framework")]
    unsafe extern "C" {
        fn CMFormatDescriptionGetExtensions(description: *const c_void) -> *const c_void;
    }

    autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?));
        // Match AVFoundation's native QuickTime/ProRes log detection. FFprobe need not expose
        // the proprietary log extension in the stream's public tags.
        unsafe {
            let asset: Retained<AnyObject> = msg_send![AnyClass::get(c"AVURLAsset")?, URLAssetWithURL: &*url, options: std::ptr::null::<AnyObject>()];
            let tracks: Retained<NSArray<AnyObject>> =
                msg_send![&*asset,tracksWithMediaType:&*AVMediaTypeVideo];
            let track = tracks.firstObject()?;
            let descriptions: Retained<NSArray<AnyObject>> = msg_send![&*track, formatDescriptions];
            let first = descriptions.firstObject()?;
            let dictionary = CMFormatDescriptionGetExtensions((&*first as *const AnyObject).cast())
                .cast::<NSDictionary<NSString, AnyObject>>()
                .as_ref()?;
            let value = dictionary
                .objectForKey(&NSString::from_str("LogTransferFunction"))?
                .downcast::<NSString>()
                .ok()?;
            apple_log(&value.to_string())
        }
    })
}

#[cfg(not(target_os = "macos"))]
fn apple_track(_path: &Path) -> Option<Profile> {
    None
}

fn color_path(store: &Store, id: Uuid) -> PathBuf {
    store.media(id, true).with_extension("source-color-v1.json")
}

pub fn cache(store: &Store, id: Uuid, color: SourceColor) -> Result<()> {
    private_write(&color_path(store, id), &serde_json::to_vec(&color)?)
}

pub async fn discard(store: &Store, id: Uuid) {
    let _ = tokio::fs::remove_file(color_path(store, id)).await;
    let sidecar = store.media(id, true).with_extension("rec709-v1.mp4");
    for path in [
        store
            .media(id, true)
            .with_extension("auto-edit-sdr-frames-v1.json"),
        sidecar.with_extension("jpg"),
        sidecar.with_extension("thumb-v2.jpg"),
        sidecar.with_extension("scene-tags.json"),
        sidecar,
    ] {
        let _ = tokio::fs::remove_file(path).await;
    }
}

pub fn ready(store: &Store, source: &Source) -> PathBuf {
    let base = store.media(source.storage_id(), source.video);
    let sdr = base.with_extension("rec709-v1.mp4");
    if source.video && !source.image && sdr.is_file() {
        sdr
    } else {
        base
    }
}

pub async fn original_profile(store: &Store, source: &Source, path: &Path) -> Result<SourceColor> {
    if !source.video || source.image {
        return Ok(SourceColor::default());
    }
    let id = source.storage_id();
    if let Ok(data) = tokio::fs::read(color_path(store, id)).await
        && let Ok(color) = serde_json::from_slice(&data)
    {
        return Ok(color);
    }
    let color = SourceColor::inspect(path).await?;
    let _ = cache(store, id, color);
    Ok(color)
}

pub async fn export_profile(store: &Store, source: &Source) -> Result<SourceColor> {
    let original = store.original(source.storage_id());
    if original.is_file() {
        let mut color = original_profile(store, source, &original).await?;
        color.baked = false;
        return Ok(color);
    }
    let path = store.media(source.storage_id(), source.video);
    let color = original_profile(store, source, &path).await?;
    // A recovered project may have its already normalized playback copy but no camera original.
    Ok(if color.baked {
        SourceColor {
            baked: color.needs_conversion(),
            ..SourceColor::default()
        }
    } else {
        color
    })
}

pub async fn playback(store: &Store, source: &Source) -> Result<PathBuf> {
    let base = store.media(source.storage_id(), source.video);
    let ready = ready(store, source);
    if !source.video || source.image || ready != base {
        return Ok(ready);
    }
    let original = store.original(source.storage_id());
    let raw = if original.is_file() {
        original
    } else {
        base.clone()
    };
    let color = original_profile(store, source, &raw).await?;
    if !color.needs_conversion() || color.baked {
        return Ok(base);
    }
    let dest = base.with_extension("rec709-v1.mp4");
    let guard = proxy_lock(&dest).lock_owned().await;
    if !dest.is_file() {
        let root = store.root.clone();
        let target = dest.clone();
        // A browser request can time out before a long legacy clip has re-encoded.
        // Keep the single conversion running so its next request can reuse the result.
        tokio::spawn(async move {
            static LEGACY_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
            let _guard = guard;
            let _slot = LEGACY_SLOTS.acquire().await?;
            let work = root.join(format!("work-{}", Uuid::new_v4()));
            tokio::fs::create_dir(&work).await?;
            let output = work.join("preview.mp4");
            let result = async {
                media::encode_video_proxy(&root, &raw, &output, color).await?;
                tokio::fs::rename(&output, &target)
                    .await
                    .context("Install Rec.709 playback media.")?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            let _ = tokio::fs::remove_dir_all(work).await;
            result
        })
        .await??;
    }
    Ok(dest)
}

fn proxy_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    type Locks = Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>;
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Locks::default)
        .lock()
        .expect("Rec.709 proxy locks");
    locks.retain(|_, entry| entry.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(path.to_owned(), Arc::downgrade(&lock));
    lock
}

pub fn managed_yuv(alpha: bool) -> String {
    let format = if alpha { "yuva420p" } else { "yuv420p" };
    format!("scale=out_color_matrix=bt709:out_range=limited,format={format},{SDR_SIGNAL}")
}

impl Profile {
    fn slug(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Hlg => "hlg",
            Self::Pq => "pq",
            Self::AppleLog => "apple-log",
            Self::AppleLog2 => "apple-log-2",
        }
    }

    fn decode(self, v: f64) -> f64 {
        match self {
            Self::Standard => v,
            Self::Hlg => {
                let (a, b, c) = (0.17883277, 0.28466892, 0.55991073);
                (if v <= 0.5 {
                    v * v / 3.0
                } else {
                    (((v - c) / a).exp() + b) / 12.0
                }) / 0.2647
            }
            Self::Pq => {
                let (m1, m2, c1, c2, c3) =
                    (0.1593017578125, 78.84375, 0.8359375, 18.8515625, 18.6875);
                let p = v.max(0.0).powf(1.0 / m2);
                ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1) * 10000.0 / 203.0
            }
            Self::AppleLog | Self::AppleLog2 => {
                let r0 = -0.05641088;
                let (rt, c, b, g, base) = (0.01, 47.28711236, 0.00964052, 0.08550479, 0.69336945);
                let threshold = c * (rt - r0) * (rt - r0);
                if v >= threshold {
                    ((v - base) / g).exp2() - b
                } else if v > 0.0 {
                    (v / c).sqrt() + r0
                } else {
                    r0
                }
            }
        }
    }

    fn convert(self, values: [f64; 3]) -> [f64; 3] {
        if self == Self::Standard {
            return values;
        }
        // The native matrices are derived from D65 and the published source primaries.
        let matrix = if self == Self::AppleLog2 {
            [
                [1.7072803771736, -0.5190199199320, -0.1882604572416],
                [-0.1250107461084, 1.3146623654822, -0.1896516193738],
                [-0.0436243333879, -0.1912143717325, 1.2348387051204],
            ]
        } else {
            [
                [1.6604910021084, -0.5876411387885, -0.0728498633199],
                [-0.1245504745216, 1.1328998971260, -0.0083494226044],
                [-0.0181507633549, -0.1005788980080, 1.1187296613629],
            ]
        };
        let linear = values.map(|value| self.decode(value));
        let c = matrix
            .map(|row| (row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2]).max(0.0));
        let peak = c.into_iter().fold(0.0_f64, f64::max);
        let knee = 0.9;
        let scale = if peak > knee {
            let eased = knee + (1.0 - knee) * (1.0 - (-(peak - knee) / (1.0 - knee)).exp());
            eased / peak
        } else {
            1.0
        };
        c.map(|v| {
            let v = (v * scale).clamp(0.0, 1.0);
            if v < 0.018 {
                4.5 * v
            } else {
                1.099 * v.powf(0.45) - 0.099
            }
        })
    }

    async fn cube_in(self, dir: &Path) -> Result<String> {
        let filename = format!("source-color-{}-v1.cube", self.slug());
        let path = dir.join(&filename);
        if !path.is_file() {
            tokio::task::spawn_blocking(move || {
                static DATA: [OnceLock<Vec<u8>>; 4] = [const { OnceLock::new() }; 4];
                let index = match self {
                    Self::Hlg => 0,
                    Self::Pq => 1,
                    Self::AppleLog => 2,
                    Self::AppleLog2 => 3,
                    Self::Standard => unreachable!(),
                };
                let data = DATA[index].get_or_init(|| {
                    let mut bytes = Vec::new();
                    writeln!(bytes, "LUT_3D_SIZE {GRID}").expect("cube header");
                    for b in 0..GRID {
                        for g in 0..GRID {
                            for r in 0..GRID {
                                let [r, g, b] = self
                                    .convert([r, g, b].map(|v| f64::from(v) / f64::from(GRID - 1)));
                                writeln!(bytes, "{r:.8} {g:.8} {b:.8}").expect("cube entry");
                            }
                        }
                    }
                    bytes
                });
                private_write(&path, data)
            })
            .await??;
        }
        Ok(filename)
    }
}

pub fn replay(input: &str) -> Option<[f64; 3]> {
    let mut words = input.split(',');
    let profile = match words.next()? {
        "hlg" => Profile::Hlg,
        "pq" => Profile::Pq,
        "apple-log" => Profile::AppleLog,
        "apple-log-2" => Profile::AppleLog2,
        _ => return None,
    };
    let mut rgb = [0.0; 3];
    for value in &mut rgb {
        *value = words.next()?.parse::<f64>().ok()?;
        if !(0.0..=1.0).contains(value) {
            return None;
        }
    }
    if words.next().is_some() {
        return None;
    }
    Some(profile.convert(rgb))
}
