// The edit clock is independent of the video model. All supported frame grids
// and 48 kHz audio have an integral step on this clock.
pub const TICKS_PER_SECOND: u32 = 240_000;
pub const MAX_TIME: u32 = 7200 * TICKS_PER_SECOND;
pub const EDIT_RATES: [f64; 11] = [
    23.976, 24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0, 100.0, 119.88, 120.0,
];
pub const MODEL_FRAME: u32 = TICKS_PER_SECOND / crate::FPS;

pub fn ticks(seconds: f64) -> u32 {
    (seconds.clamp(0.0, 7200.0) * f64::from(TICKS_PER_SECOND)).round() as u32
}

pub fn seconds(ticks: u32) -> f64 {
    f64::from(ticks) / f64::from(TICKS_PER_SECOND)
}

pub fn step(fps: f64) -> u32 {
    // Editroom exposes rounded NTSC labels. Preserve the exact broadcast rate.
    let rate = match fps {
        23.976 => 24_000.0 / 1001.0,
        29.97 => 30_000.0 / 1001.0,
        59.94 => 60_000.0 / 1001.0,
        119.88 => 120_000.0 / 1001.0,
        fps if fps.is_finite() && fps > 0.0 => fps,
        _ => 24.0,
    };
    (f64::from(TICKS_PER_SECOND) / rate).round().max(1.0) as u32
}

pub fn snap_seconds(seconds: f64, step: u32) -> u32 {
    let frames =
        (seconds.clamp(0.0, 7200.0) * (f64::from(TICKS_PER_SECOND) / f64::from(step))).round();
    ((frames as u64) * u64::from(step)).min(u64::from(MAX_TIME)) as u32
}
