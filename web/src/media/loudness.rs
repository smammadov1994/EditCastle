const STEP: usize = 4800;
const SAMPLE_BYTES: usize = 8;

#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

const WEIGHTING: [Biquad; 2] = [
    Biquad {
        b0: 1.53512485958697,
        b1: -2.69169618940638,
        b2: 1.19839281085285,
        a1: -1.69065929318241,
        a2: 0.73248077421585,
        z1: 0.0,
        z2: 0.0,
    },
    Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: -1.99004745483398,
        a2: 0.99007225036621,
        z1: 0.0,
        z2: 0.0,
    },
];

pub(super) struct Meter {
    weighting: [[Biquad; 2]; 2],
    steps: Vec<f64>,
    power: f64,
    count: usize,
    peak: f32,
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            weighting: [WEIGHTING; 2],
            steps: Vec::new(),
            power: 0.0,
            count: 0,
            peak: 0.0,
        }
    }
}

impl Meter {
    // The export mix comes from FFmpeg as 48 kHz, interleaved stereo f32le.
    pub(super) fn read(&mut self, bytes: &[u8]) -> usize {
        let consumed = bytes.len() / SAMPLE_BYTES * SAMPLE_BYTES;
        for frame in bytes[..consumed].chunks_exact(SAMPLE_BYTES) {
            let [left, right] = [
                f32::from_le_bytes(frame[..4].try_into().expect("left sample")),
                f32::from_le_bytes(frame[4..].try_into().expect("right sample")),
            ];
            self.peak = self.peak.max(left.abs()).max(right.abs());
            for (filters, sample) in self.weighting.iter_mut().zip([left, right]) {
                let shelf = filters[0].run(f64::from(sample));
                let weighted = filters[1].run(shelf);
                self.power += weighted * weighted;
            }
            self.count += 1;
            if self.count == STEP {
                self.steps.push(self.power / STEP as f64);
                self.power = 0.0;
                self.count = 0;
            }
        }
        consumed
    }

    pub(super) fn frames(&self) -> usize {
        self.steps.len() * STEP + self.count
    }

    pub(super) fn gain(self, target: f64) -> Option<f64> {
        let lufs = |power: f64| -0.691 + 10.0 * power.max(1e-12).log10();
        // Measure 400 ms blocks at 100 ms intervals with the native meter's gates.
        let blocks = self
            .steps
            .windows(4)
            .map(|window| window.iter().sum::<f64>() / 4.0);
        let average = |powers: &mut dyn Iterator<Item = f64>| {
            let (sum, count) =
                powers.fold((0.0, 0), |(sum, count), power| (sum + power, count + 1));
            (count > 0).then(|| sum / f64::from(count))
        };
        let absolute = blocks.filter(|&power| lufs(power) > -70.0);
        let relative_gate = lufs(average(&mut absolute.clone())?) - 10.0;
        let integrated = lufs(average(
            &mut absolute.filter(|&power| lufs(power) > relative_gate),
        )?);
        let peak_db = 20.0 * f64::from(self.peak).log10();
        Some((target - integrated).min(-1.0 - peak_db))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(level: f32, peak: Option<f32>) -> Meter {
        let mut meter = Meter::default();
        for frame in 0..48_000 {
            let sine = (std::f32::consts::TAU * frame as f32 / 48.0).sin();
            let sample = if frame == 24_000 {
                peak.unwrap_or(sine * level)
            } else {
                sine * level
            };
            let mut stereo = [0; SAMPLE_BYTES];
            stereo[..4].copy_from_slice(&sample.to_le_bytes());
            stereo[4..].copy_from_slice(&sample.to_le_bytes());
            meter.read(&stereo);
        }
        meter
    }

    #[test]
    fn quieter_signal_receives_proportionally_more_gain() {
        let quiet = tone(0.1, None).gain(-23.0).unwrap();
        let loud = tone(0.2, None).gain(-23.0).unwrap();
        assert!((quiet - loud - 20.0 * 2.0_f64.log10()).abs() < 1e-6);
    }

    #[test]
    fn transient_limits_gain_before_clipping() {
        let peak = 0.95_f32;
        let gain = tone(0.025, Some(peak)).gain(-14.0).unwrap();
        assert!((gain + 20.0 * f64::from(peak).log10() + 1.0).abs() < 1e-9);
    }

    #[test]
    fn silence_has_no_loudness_adjustment() {
        assert!(tone(0.0, None).gain(-14.0).is_none());
    }
}
