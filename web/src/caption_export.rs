use crate::{
    captions::{Block, CaptionTrack},
    clock,
};
use ab_glyph::{Font, FontRef, Glyph, PxScale, ScaleFont, point};
use anyhow::{Context, Result, ensure};
use fontdb::Weight;
use serde::Deserialize;
use std::{collections::HashMap, fmt::Write, path::Path};

struct Layout {
    glyphs: Vec<(Glyph, usize)>,
    top: u32,
    height: u32,
}

pub struct Overlay {
    pub y: u32,
}

struct Rgba([u8; 4]);

impl Rgba {
    fn read(text: &str, fallback: [u8; 4]) -> Self {
        let hex = text.trim_start_matches('#');
        let color = u32::from_str_radix(hex, 16).ok();
        Self(match (hex.len(), color) {
            (6, Some(rgb)) => ((rgb << 8) | 255).to_be_bytes(),
            (8, Some(rgba)) => rgba.to_be_bytes(),
            _ => fallback,
        })
    }
}

#[derive(Clone, Copy)]
struct Segment {
    start: u32,
    end: u32,
    block: usize,
    lit: Option<usize>,
}

fn segments(blocks: &[Block], highlight: bool, from: u32, to: u32, step: u32) -> Vec<Segment> {
    let output_frame = |tick: u32| tick.saturating_sub(from).div_ceil(step) * step;
    let mut spans = Vec::new();
    for (block_index, block) in blocks.iter().enumerate() {
        let limit = blocks
            .get(block_index + 1)
            .map_or(block.end, |next| block.end.min(next.start));
        let stop = limit.min(to);
        if block.start >= stop || stop <= from {
            continue;
        }
        let mut append = |start: u32, end: u32, lit: Option<usize>| {
            let start = output_frame(start.max(from));
            let end = output_frame(end.min(stop));
            if start < end {
                spans.push(Segment {
                    start,
                    end,
                    block: block_index,
                    lit,
                });
            }
        };
        if highlight {
            for (i, word) in block.words.iter().enumerate() {
                let start = if i == 0 { block.start } else { word.start };
                let next = block.words.get(i + 1).map_or(stop, |w| w.start);
                append(start, next, Some(i));
            }
        } else {
            append(block.start, stop, None);
        }
    }
    spans
}

fn layout<'a>(
    font: &FontRef,
    words: impl Iterator<Item = &'a str>,
    size: f32,
    position: f64,
    band_width: u32,
    frame: (u32, u32),
) -> Layout {
    let (width, height) = frame;
    let scale = PxScale::from(
        size * font.height_unscaled() / font.units_per_em().unwrap_or(font.height_unscaled()),
    );
    let face = font.as_scaled(scale);
    let gap = face.h_advance(face.glyph_id(' ')) + size * 0.06;
    let mut rows = Vec::new();
    let mut current = Vec::new();
    let mut right = 0.0;
    for (i, text) in words.enumerate() {
        let mut word = Vec::new();
        let mut x = 0.0;
        let mut prev = None;
        for ch in text.chars() {
            let id = face.glyph_id(ch);
            if let Some(previous) = prev {
                x += face.kern(previous, id);
            }
            word.push((id, x));
            x += face.h_advance(id);
            prev = Some(id);
        }
        if !current.is_empty() && right + gap + x > width as f32 * 0.86 {
            rows.push((std::mem::take(&mut current), right));
            right = 0.0;
        }
        let left = if current.is_empty() {
            right
        } else {
            right + gap
        };
        current.extend(word.into_iter().map(|(id, at)| (id, left + at, i)));
        right = left + x;
    }
    rows.push((current, right));
    let line = (face.height() + face.line_gap()).ceil().max(size);
    let pad = (size * 0.3).ceil().max(2.0);
    let height = ((rows.len() as f32 * line + pad * 2.0).ceil() as u32)
        .min(height)
        .max(1);
    let top = (f64::from(frame.1) * position - f64::from(height) / 2.0)
        .round()
        .clamp(0.0, f64::from(frame.1 - height)) as u32;
    let baseline = pad + (line - face.height()) / 2.0 + face.ascent();
    let glyphs = rows
        .into_iter()
        .enumerate()
        .flat_map(|(row, (items, width))| {
            let origin = (band_width as f32 - width) / 2.0;
            let y = baseline + row as f32 * line;
            items
                .into_iter()
                .map(move |(id, x, i)| (id.with_scale_and_position(scale, point(x + origin, y)), i))
        })
        .collect();
    Layout {
        glyphs,
        top,
        height,
    }
}

fn png(output: impl std::io::Write, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let mut encoder = png::Encoder::new(output, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}

fn blend(rgba: &mut [u8], color: &Rgba, coverage: f32) {
    let src = f32::from(color.0[3]) * coverage / 255.0;
    let dst = f32::from(rgba[3]) / 255.0;
    let a = src + dst * (1.0 - src);
    if a <= 0.0 {
        return;
    }
    for (index, value) in rgba.iter_mut().take(3).enumerate() {
        *value = ((f32::from(color.0[index]) * src + f32::from(*value) * dst * (1.0 - src)) / a)
            .round() as u8;
    }
    rgba[3] = (255.0 * a).round() as u8;
}

struct Mask {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn mask(font: &FontRef, layout: &Layout, width: u32, radius: i32, scale: f32) -> Mask {
    let width = (width as f32 * scale).ceil().max(2.0) as u32;
    let height = (layout.height as f32 * scale).ceil().max(1.0) as u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let radius = if radius > 0 {
        (radius as f32 * scale).round().max(1.0) as i32
    } else {
        0
    };
    let spans = (-radius..=radius)
        .map(|dy| (dy, ((radius * radius - dy * dy) as f32).sqrt() as i32))
        .collect::<Vec<_>>();
    let index = |x: i32, y: i32| {
        (x >= 0 && y >= 0 && x < width as i32 && y < height as i32)
            .then(|| (y as usize * width as usize + x as usize) * 4)
    };
    for (glyph, word) in &layout.glyphs {
        let mut glyph = glyph.clone();
        glyph.scale.x *= scale;
        glyph.scale.y *= scale;
        glyph.position.x *= scale;
        glyph.position.y *= scale;
        let Some(shape) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = shape.px_bounds();
        shape.draw(|x, y, coverage| {
            if coverage <= 0.0 {
                return;
            }
            let x = x as i32 + bounds.min.x as i32;
            let y = y as i32 + bounds.min.y as i32;
            let alpha = (coverage * 255.0) as u8;
            if let Some(at) = index(x, y) {
                // The RGB channels carry coverage, outline coverage, and a one-based word index.
                let word = (*word + 1).min(255) as u8;
                if rgba[at + 2] == word {
                    rgba[at] = alpha
                        .saturating_add((u16::from(rgba[at]) * u16::from(255 - alpha) / 255) as u8);
                } else if rgba[at] <= alpha {
                    rgba[at] = alpha;
                    rgba[at + 2] = word;
                }
            }
            if radius > 0 {
                for (dy, span) in &spans {
                    for dx in -*span..=*span {
                        if let Some(at) = index(x + dx, y + dy) {
                            rgba[at + 1] = rgba[at + 1].max(alpha);
                        }
                    }
                }
            }
        });
    }
    Mask {
        width,
        height,
        rgba,
    }
}

struct Palette {
    text: Rgba,
    accent: Rgba,
    border: Rgba,
}

impl Palette {
    fn read(track: &CaptionTrack) -> Self {
        Self {
            text: Rgba::read(&track.text_color, [255; 4]),
            accent: Rgba::read(&track.highlight_color, [255, 214, 64, 255]),
            border: Rgba::read(&track.outline_color, [0, 0, 0, 255]),
        }
    }

    fn paint(&self, canvas: &mut [u8], mask: &Mask, lit: Option<usize>) {
        for (pixel, levels) in canvas.chunks_exact_mut(4).zip(mask.rgba.chunks_exact(4)) {
            let [fill, outline, word, _] = <[u8; 4]>::try_from(levels).expect("caption mask pixel");
            if outline > 0 {
                pixel[..3].copy_from_slice(&self.border.0[..3]);
                pixel[3] = (u16::from(outline) * u16::from(self.border.0[3]) / 255) as u8;
            }
            if fill > 0 {
                let color = if lit.map(|i| i + 1) == Some(usize::from(word)) {
                    &self.accent
                } else {
                    &self.text
                };
                blend(pixel, color, f32::from(fill) / 255.0);
            }
        }
    }
}

fn band(width: u32) -> u32 {
    ((f64::from(width) * 0.90).ceil() as u32)
        .next_multiple_of(2)
        .min(width)
        .max(2)
}

fn point_size(size: f64, width: u32, height: u32) -> f32 {
    (size.clamp(28.0, 140.0) * f64::from(width.min(height)) / 1080.0).max(1.0) as f32
}

fn stroke_radius(outline: bool, size: f32) -> i32 {
    if outline {
        (size * 0.105).round().max(1.0) as i32
    } else {
        0
    }
}

#[derive(Deserialize)]
pub(crate) struct Preview {
    pub words: Vec<String>,
    pub width: u32,
    pub height: u32,
    pub size: f64,
    pub outline: bool,
}

impl Preview {
    pub fn render(self) -> Result<(Vec<u8>, u32, u32)> {
        let Self {
            words,
            width,
            height,
            size,
            outline,
        } = self;
        ensure!(
            (2..=16384).contains(&width) && (2..=16384).contains(&height),
            "Choose valid caption frame dimensions."
        );
        ensure!(
            !words.is_empty()
                && words.len() <= 254
                && words.iter().map(String::len).sum::<usize>() <= 12000
                && size.is_finite(),
            "Choose valid caption words and size."
        );
        let (fonts, id) = crate::media::title_font("System", Weight(900))?;
        fonts
            .with_face_data(id, |data, index| -> Result<_> {
                let face = FontRef::try_from_slice_and_index(data, index)
                    .context("Read the caption font.")?;
                let size = point_size(size, width, height);
                let band_width = band(width);
                let layout = layout(
                    &face,
                    words.iter().map(String::as_str),
                    size,
                    0.5,
                    band_width,
                    (width, height),
                );
                let scale = (1200.0 / width.max(height) as f32).min(1.0);
                let mut mask = mask(
                    &face,
                    &layout,
                    band_width,
                    stroke_radius(outline, size),
                    scale,
                );
                // Opaque mask pixels keep their data channels intact when decoded by a canvas.
                for pixel in mask.rgba.chunks_exact_mut(4) {
                    pixel[3] = 255;
                }
                let mut bytes = Vec::new();
                png(&mut bytes, mask.width, mask.height, &mask.rgba)?;
                Ok((bytes, band_width, layout.height))
            })
            .context("The caption font could not be loaded.")?
    }
}

pub struct Export<'a> {
    pub from: u32,
    pub to: u32,
    pub step: u32,
    pub width: u32,
    pub height: u32,
    pub dir: &'a Path,
}

pub fn write(
    blocks: &[Block],
    track: &CaptionTrack,
    output: Export<'_>,
) -> Result<Option<Overlay>> {
    let Export {
        from,
        to,
        step,
        width,
        height,
        dir,
    } = output;
    let spans = segments(blocks, track.highlight, from, to, step);
    if spans.is_empty() {
        return Ok(None);
    }
    let font = crate::media::title_font("System", Weight(900))?;
    font.0
        .with_face_data(font.1, |data, index| -> Result<Option<Overlay>> {
            let face =
                FontRef::try_from_slice_and_index(data, index).context("Read the caption font.")?;
            let size = point_size(track.size, width, height);
            let band_width = band(width);
            let mut layouts = HashMap::new();
            for span in &spans {
                layouts.entry(span.block).or_insert_with(|| {
                    layout(
                        &face,
                        blocks[span.block]
                            .words
                            .iter()
                            .map(|word| word.text.as_str()),
                        size,
                        track.position,
                        band_width,
                        (width, height),
                    )
                });
            }
            let y = layouts.values().map(|l| l.top).min().unwrap_or(0);
            let band_height = layouts
                .values()
                .map(|l| l.top + l.height - y)
                .max()
                .unwrap_or(1)
                .max(2);
            let palette = Palette::read(track);
            let radius = stroke_radius(track.outline, size);
            let mut rgba = vec![0u8; (band_width * band_height * 4) as usize];
            let blank = "caption-blank.png";
            png(
                std::fs::File::create(dir.join(blank))?,
                band_width,
                band_height,
                &rgba,
            )?;
            let mut previous = 0;
            let mut manifest = String::from("ffconcat version 1.0\n");
            let mut images = HashMap::new();
            let mut current_mask: Option<(usize, Mask)> = None;
            let micros = |ticks| {
                (u64::from(ticks) * 1_000_000 + u64::from(clock::TICKS_PER_SECOND) / 2)
                    / u64::from(clock::TICKS_PER_SECOND)
            };
            let mut frame = |name: &str, at, end| {
                let time = micros(end) - micros(at);
                writeln!(
                    manifest,
                    "file '{name}'\noption framerate {}\nduration {}.{:06}",
                    clock::TICKS_PER_SECOND,
                    time / 1_000_000,
                    time % 1_000_000
                )
                .expect("write caption image duration");
            };
            for span in spans {
                if span.start > previous {
                    frame(blank, previous, span.start);
                }
                let key = (span.block, span.lit);
                let image = if let Some(name) = images.get(&key) {
                    name
                } else {
                    let name = format!("caption-{}.png", images.len());
                    rgba.fill(0);
                    let layout = &layouts[&span.block];
                    if !current_mask
                        .as_ref()
                        .is_some_and(|(block, _)| *block == span.block)
                    {
                        current_mask =
                            Some((span.block, mask(&face, layout, band_width, radius, 1.0)));
                    }
                    let offset = ((layout.top - y) * band_width * 4) as usize;
                    palette.paint(
                        &mut rgba[offset..],
                        &current_mask.as_ref().expect("caption mask").1,
                        span.lit,
                    );
                    png(
                        std::fs::File::create(dir.join(&name))?,
                        band_width,
                        band_height,
                        &rgba,
                    )?;
                    images.entry(key).or_insert(name)
                };
                frame(image, span.start, span.end);
                previous = span.end;
            }
            if previous < to - from {
                frame(blank, previous, to - from);
            }
            writeln!(
                manifest,
                "file '{blank}'\noption framerate {}",
                clock::TICKS_PER_SECOND
            )?;
            std::fs::write(dir.join("captions.ffconcat"), manifest)?;
            Ok(Some(Overlay { y }))
        })
        .context("The caption font could not be loaded.")?
}
