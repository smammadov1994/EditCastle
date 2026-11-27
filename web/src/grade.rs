use anyhow::{Result, ensure};
use serde_json::{Map, Value, json};
use std::{fmt::Write as _, path::Path};

type Rgb = [f64; 3];
pub const GRID: usize = 32;
pub const LOOKS: [&str; 8] = [
    "Warm film",
    "Golden hour",
    "Film print",
    "Teal and orange",
    "Pastel",
    "Bleach bypass",
    "Mono",
    "Cool night",
];
const ADJUSTMENTS: [(&str, f64, f64); 6] = [
    ("look_intensity", 0.0, 1.0),
    ("exposure", -2.0, 2.0),
    ("contrast", -50.0, 50.0),
    ("saturation", -100.0, 100.0),
    ("temperature", -50.0, 50.0),
    ("tint", -50.0, 50.0),
];
pub const WHEELS: [&str; 3] = ["shadows", "midtones", "highlights"];

pub fn look_id(name: &str) -> Option<uuid::Uuid> {
    name.strip_prefix("lut:")?.parse().ok()
}

pub fn adjusted(
    previous: &Value,
    args: &Value,
    allowed: &[crate::editor::LookupTable],
) -> Result<Value> {
    let mut result = if args["reset"] == true || args.get("replace").is_some() {
        Map::new()
    } else {
        previous.as_object().cloned().unwrap_or_default()
    };
    let raw = args.get("replace").unwrap_or(args);
    ensure!(raw.is_object() || raw.is_null(), "Supply a grade object.");
    if let Some(raw) = raw.get("look") {
        if raw.is_null()
            || raw
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case("none"))
        {
            result.remove("look");
        } else if let Some(name) = raw.as_str() {
            let builtin = LOOKS
                .iter()
                .copied()
                .chain(["Faded"])
                .find(|look| look.eq_ignore_ascii_case(name));
            let custom = look_id(name).is_some_and(|id| allowed.iter().any(|lut| lut.id == id));
            ensure!(
                builtin.is_some() || custom,
                "Choose a look from list_looks."
            );
            result.insert("look".into(), json!(builtin.unwrap_or(name)));
        } else {
            anyhow::bail!("Choose a look from list_looks.");
        }
    }
    for (key, min, max) in ADJUSTMENTS {
        if let Some(value) = raw.get(key) {
            let value = value
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("Use a number for {key}."))?;
            let value = value.clamp(min, max);
            if value == if key == "look_intensity" { 1.0 } else { 0.0 } {
                result.remove(key);
            } else {
                result.insert(key.into(), json!(value));
            }
        }
    }
    for wheel in WHEELS {
        if let Some(point) = raw.get(wheel) {
            let (Some(x), Some(y)) = (point["x"].as_f64(), point["y"].as_f64()) else {
                anyhow::bail!("Use x and y coordinates for the {wheel} wheel.");
            };
            let x = x.clamp(-1.0, 1.0);
            let y = y.clamp(-1.0, 1.0);
            let radius = x.hypot(y).max(1.0);
            if x == 0.0 && y == 0.0 {
                result.remove(wheel);
            } else {
                result.insert(wheel.into(), json!({"x":x/radius,"y":y/radius}));
            }
        }
    }
    if result.is_empty() {
        Ok(Value::Null)
    } else {
        Ok(Value::Object(result))
    }
}

pub fn is_neutral(grade: &Value) -> bool {
    let has_look = grade["look"]
        .as_str()
        .is_some_and(|name| !name.eq_ignore_ascii_case("none"))
        && number(grade, "look_intensity", 1.0) > 0.0;
    !has_look
        && ADJUSTMENTS
            .iter()
            .all(|(key, _, _)| *key == "look_intensity" || number(grade, key, 0.0) == 0.0)
        && WHEELS.iter().all(|wheel| {
            number(&grade[wheel], "x", 0.0) == 0.0 && number(&grade[wheel], "y", 0.0) == 0.0
        })
}

#[derive(Default)]
struct Recipe {
    exposure: f64,
    contrast: f64,
    saturation: f64,
    temperature: f64,
    tint: f64,
    shadows: Rgb,
    midtones: Rgb,
    highlights: Rgb,
    lift: Rgb,
    fade: f64,
}

fn luma([r, g, b]: Rgb) -> f64 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}
fn clamp(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}
fn offset([x, y]: [f64; 2]) -> Rgb {
    let strength = (x.hypot(y)).min(1.0) * 0.22;
    let angle = y.atan2(x);
    let third = std::f64::consts::TAU / 3.0;
    [angle, angle - third, angle + third].map(|a| a.cos() * strength)
}
fn number(value: &Value, key: &str, default: f64) -> f64 {
    value[key].as_f64().unwrap_or(default)
}
fn wheel(value: &Value, key: &str) -> Rgb {
    let point = &value[key];
    offset([number(point, "x", 0.0), number(point, "y", 0.0)])
}

impl Recipe {
    fn user(v: &Value) -> Self {
        Self {
            exposure: number(v, "exposure", 0.0).clamp(-2.0, 2.0),
            contrast: number(v, "contrast", 0.0).clamp(-50.0, 50.0),
            saturation: number(v, "saturation", 0.0).clamp(-100.0, 100.0),
            temperature: number(v, "temperature", 0.0).clamp(-50.0, 50.0),
            tint: number(v, "tint", 0.0).clamp(-50.0, 50.0),
            shadows: wheel(v, "shadows"),
            midtones: wheel(v, "midtones"),
            highlights: wheel(v, "highlights"),
            ..Default::default()
        }
    }
    fn apply(&self, input: Rgb) -> Rgb {
        let gain = 2.0_f64.powf(self.exposure);
        let t = self.temperature / 50.0 * 0.12;
        let tint = self.tint / 50.0 * 0.10;
        let mut rgb = [
            input[0] * gain + t,
            input[1] * gain - tint,
            input[2] * gain - t,
        ];
        let light = clamp(luma(rgb));
        let middle = 4.0 * light * (1.0 - light);
        for (i, value) in rgb.iter_mut().enumerate() {
            *value += self.shadows[i] * (1.0 - light)
                + self.midtones[i] * middle
                + self.highlights[i] * light
                + self.lift[i];
            *value = (*value - 0.5) * (1.0 + self.contrast / 50.0 * 0.6) + 0.5;
        }
        let faded = fade(rgb, self.fade);
        let light = luma(faded);
        faded.map(|x| light + (x - light) * (1.0 + self.saturation / 100.0))
    }
}

fn soft_contrast(rgb: Rgb, gain: f64) -> Rgb {
    rgb.map(|x| {
        let x = clamp(x);
        let soft = x * x * (3.0 - 2.0 * x);
        clamp(x + gain * (soft - x) * 2.0)
    })
}
fn fade(rgb: Rgb, f: f64) -> Rgb {
    rgb.map(|x| f + (1.0 - f) * x)
}
fn saturate(rgb: Rgb, amount: f64) -> Rgb {
    let l = luma(rgb);
    rgb.map(|x| clamp(l + amount * (x - l)))
}
fn tint(rgb: Rgb, changes: Rgb, mix: f64) -> Rgb {
    std::array::from_fn(|i| clamp(rgb[i] + changes[i] * mix))
}
fn weighted(rgb: Rgb, shadows: Rgb, highlights: Rgb) -> Rgb {
    let l = clamp(luma(rgb));
    tint(tint(rgb, shadows, (1.0 - l).powi(2)), highlights, l * l)
}

fn hue_band(rgb: Rgb, center: f64, width: f64, shift: f64, sat: f64, bright: f64) -> Rgb {
    let [r, g, b] = rgb;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let diff = max - min;
    let h = if diff <= 1e-6 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / diff % 6.0)
    } else if max == g {
        60.0 * ((b - r) / diff + 2.0)
    } else {
        60.0 * ((r - g) / diff + 4.0)
    };
    let h = h.rem_euclid(360.0);
    let s = if max > 0.0 { diff / max } else { 0.0 };
    let d = (h - center).abs();
    let d = d.min(360.0 - d);
    if d >= width {
        return rgb;
    }
    let weight = (0.5 + 0.5 * (d / width * std::f64::consts::PI).cos()) * (s * 2.5).min(1.0);
    let h = (h + shift * weight).rem_euclid(360.0);
    let s = clamp(s * (1.0 + (sat - 1.0) * weight));
    let v = clamp(max * (1.0 + (bright - 1.0) * weight));
    let c = v * s;
    let x = c * (1.0 - (h / 60.0 % 2.0 - 1.0).abs());
    let m = v - c;
    let output = match (h / 60.0) as u32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    output.map(|channel| channel + m)
}

fn film_print(rgb: Rgb) -> Rgb {
    let c = tint(rgb.map(clamp), [0.1260, 0.0872, 0.1130], 1.0).map(|x| clamp(x).powf(0.9273));
    let c = hue_band(c, 28.0, 22.0, 4.3723, 1.1754, 1.0);
    let c = hue_band(c, 110.0, 60.0, -6.7912, 0.8993, 1.0);
    let c = hue_band(c, 210.0, 45.0, -3.0058, 1.0943, 1.0);
    let c = weighted(c, [-0.2338, -0.1982, -0.2908], [-0.1016, -0.0275, -0.0710]);
    let c = saturate(soft_contrast(c, 0.1846), 0.8248);
    fade(hue_band(c, 356.0, 20.0, 0.0, 1.0324, 1.0304), 0.035)
}

fn apply_look(look: &str, c: Rgb) -> Rgb {
    match look {
        "warm film" => Recipe {
            contrast: 8.0,
            saturation: -8.0,
            temperature: 18.0,
            lift: [0.02, 0.0, -0.02],
            fade: 0.03,
            ..Default::default()
        }
        .apply(c),
        "golden hour" => saturate(
            soft_contrast(weighted(c, [-0.012, 0.0, 0.03], [0.085, 0.04, -0.06]), 0.08),
            1.08,
        ),
        "film print" => film_print(c),
        "teal and orange" => Recipe {
            contrast: 12.0,
            saturation: 10.0,
            shadows: [-0.05, 0.01, 0.05],
            highlights: [0.06, 0.01, -0.05],
            ..Default::default()
        }
        .apply(c),
        "pastel" => saturate(
            weighted(
                fade(soft_contrast(c.map(|x| clamp(x).powf(0.8)), -0.36), 0.14),
                [0.03, 0.0, 0.05],
                [0.01, 0.0, 0.012],
            ),
            0.8,
        ),
        "bleach bypass" => tint(
            soft_contrast(saturate(c.map(clamp), 0.42), 0.42),
            [-0.01, 0.0, 0.008],
            1.0,
        ),
        "mono" => Recipe {
            contrast: 14.0,
            saturation: -100.0,
            ..Default::default()
        }
        .apply(c),
        "cool night" => Recipe {
            exposure: -0.25,
            contrast: 6.0,
            saturation: -15.0,
            temperature: -22.0,
            ..Default::default()
        }
        .apply(c),
        "faded" => Recipe {
            contrast: -18.0,
            saturation: -20.0,
            fade: 0.07,
            ..Default::default()
        }
        .apply(c),
        _ => c,
    }
}

pub struct Cube {
    size: usize,
    rgb: Vec<Rgb>,
}

impl Cube {
    pub fn parse(text: &str) -> Result<Self> {
        use anyhow::ensure;
        let mut size = 0;
        let mut rgb = vec![];
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let tokens = line.split_whitespace().collect::<Vec<_>>();
            if tokens[0] == "LUT_3D_SIZE" {
                ensure!(tokens.len() == 2, "Invalid LUT size.");
                size = tokens[1].parse::<usize>()?;
                ensure!(
                    (2..=64).contains(&size),
                    "Use a 3D LUT up to 64 points per axis."
                );
            } else if tokens[0].as_bytes()[0].is_ascii_digit()
                || tokens[0].starts_with(['-', '+', '.'])
            {
                ensure!(tokens.len() == 3, "Invalid 3D LUT values.");
                let red = tokens[0].parse::<f64>()?;
                let green = tokens[1].parse::<f64>()?;
                let blue = tokens[2].parse::<f64>()?;
                ensure!(
                    [red, green, blue]
                        .iter()
                        .all(|value| value.is_finite() && value.abs() <= 16.0),
                    "The LUT contains invalid values."
                );
                rgb.push([red, green, blue]);
                ensure!(rgb.len() <= 64_usize.pow(3), "The LUT is too large.");
            }
        }
        ensure!(
            size >= 2 && rgb.len() == size.pow(3),
            "The file is missing valid 3D LUT values."
        );
        Ok(Self { size, rgb })
    }

    fn sample(&self, p: Rgb) -> Rgb {
        let scales = p.map(|v| clamp(v) * (self.size - 1) as f64);
        let lows = scales.map(|s| (s.floor() as usize).min(self.size - 2));
        let w: Rgb = std::array::from_fn(|i| scales[i] - lows[i] as f64);
        let mut result = [0.0; 3];
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    let offsets = [r, g, b];
                    let gain = (0..3)
                        .map(|i| if offsets[i] == 0 { 1.0 - w[i] } else { w[i] })
                        .product::<f64>();
                    let index = ((lows[2] + b) * self.size + lows[1] + g) * self.size + lows[0] + r;
                    for (channel, value) in result.iter_mut().enumerate() {
                        *value += gain * self.rgb[index][channel];
                    }
                }
            }
        }
        result
    }
}

pub fn build(grade: &Value, custom: Option<&Path>) -> Result<Vec<[f32; 3]>> {
    let cube = custom
        .map(|path| Cube::parse(&std::fs::read_to_string(path)?))
        .transpose()?;
    let look = grade["look"].as_str().unwrap_or_default().to_lowercase();
    let intensity = number(grade, "look_intensity", 1.0).clamp(0.0, 1.0);
    let user = Recipe::user(grade);
    let mut data = Vec::with_capacity(GRID.pow(3));
    for b in 0..GRID {
        for g in 0..GRID {
            for r in 0..GRID {
                let rgb = [r, g, b].map(|v| v as f64 / (GRID - 1) as f64);
                let looked = cube
                    .as_ref()
                    .map(|table| table.sample(rgb))
                    .unwrap_or_else(|| apply_look(&look, rgb));
                let mixed = std::array::from_fn(|i| rgb[i] + intensity * (looked[i] - rgb[i]));
                data.push(user.apply(mixed).map(|x| clamp(x) as f32));
            }
        }
    }
    Ok(data)
}

pub fn bake(path: &Path, grade: &Value, custom: Option<&Path>) -> Result<()> {
    let mut text = format!("LUT_3D_SIZE {GRID}\n");
    text.reserve(900_000);
    for [r, g, b] in build(grade, custom)? {
        writeln!(text, "{r:.6} {g:.6} {b:.6}")?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

pub fn lookup_bytes(grade: &Value, custom: Option<&Path>) -> Result<Vec<u8>> {
    Ok(build(grade, custom)?
        .into_iter()
        .flatten()
        .map(|component| (component * 255.0).round() as u8)
        .collect())
}
