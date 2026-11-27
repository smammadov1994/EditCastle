use serde_json::Value;

pub struct Ramp {
    pub from: f64,
    pub to: f64,
    pub ease_in: f64,
    pub ease_out: f64,
    pub start_speed: f64,
    pub end_speed: f64,
    pub middle_speed: f64,
}

impl Ramp {
    pub fn read(value: &Value) -> Option<Self> {
        Some(Self {
            from: value["from"].as_f64()?,
            to: value["to"].as_f64()?,
            ease_in: value["ease_in"].as_f64()?,
            ease_out: value["ease_out"].as_f64()?,
            start_speed: value["start_speed"].as_f64()?.clamp(0.05, 100.0),
            end_speed: value["end_speed"].as_f64()?.clamp(0.05, 100.0),
            middle_speed: value["speed"].as_f64()?.clamp(0.05, 100.0),
        })
    }

    pub fn mirrored(self, length: f64) -> Self {
        Self {
            from: length - self.to,
            to: length - self.from,
            ease_in: self.ease_out,
            ease_out: self.ease_in,
            start_speed: self.end_speed,
            end_speed: self.start_speed,
            middle_speed: self.middle_speed,
        }
    }

    pub fn value(&self) -> Value {
        serde_json::json!({"from":self.from,"to":self.to,"ease_in":self.ease_in,"ease_out":self.ease_out,
            "start_speed":self.start_speed,"end_speed":self.end_speed,"speed":self.middle_speed})
    }

    fn marks(&self) -> [f64; 4] {
        [
            self.from - self.ease_in,
            self.from,
            self.to,
            self.to + self.ease_out,
        ]
    }

    pub fn speed(&self, source: f64) -> f64 {
        let blend =
            |a: f64, b: f64, u: f64| (a.ln() + (b.ln() - a.ln()) * u * u * (3.0 - 2.0 * u)).exp();
        let [lead, enter, leave, tail] = self.marks();
        if source < enter {
            if source <= lead {
                self.start_speed
            } else {
                blend(
                    self.start_speed,
                    self.middle_speed,
                    (source - lead) / (enter - lead),
                )
            }
        } else if source > leave {
            if source >= tail {
                self.end_speed
            } else {
                blend(
                    self.middle_speed,
                    self.end_speed,
                    (source - leave) / (tail - leave),
                )
            }
        } else {
            self.middle_speed
        }
    }

    pub fn span(&self, from: f64, to: f64) -> f64 {
        if from > to {
            return -self.span(to, from);
        }
        let mut start = from;
        let mut total = 0.0;
        for stop in self.marks().into_iter().chain([to]) {
            if stop <= start {
                continue;
            }
            let end = stop.min(to);
            let a = self.speed(start);
            let b = self.speed(end);
            let middle = self.speed((start + end) / 2.0);
            if (a - b).abs() < 1e-12 && (a - middle).abs() < 1e-12 {
                total += (end - start) / a;
            } else {
                let h = (end - start) / 24.0;
                let sum = (1..24)
                    .map(
                        |i| if i % 2 == 0 { 2.0 } else { 4.0 } / self.speed(start + (i as f64) * h),
                    )
                    .sum::<f64>();
                total += (1.0 / a + 1.0 / b + sum) * h / 3.0;
            }
            start = end;
            if stop >= to {
                break;
            }
        }
        total
    }

    pub fn source(&self, start: f64, elapsed: f64) -> f64 {
        let mut low = start.min(start + elapsed * 100.0);
        let mut high = start.max(start + elapsed * 100.0);
        for _ in 0..35 {
            let mid = (low + high) / 2.0;
            if self.span(start, mid) < elapsed {
                low = mid;
            } else {
                high = mid;
            }
        }
        (low + high) / 2.0
    }

    pub fn points(&self, start: f64, end: f64, reversed: bool) -> Vec<(f64, f64)> {
        let [left, enter, leave, right] = self.marks();
        let mut cuts = vec![start, end];
        for bounds in [(left, enter), (leave, right)] {
            for s in 0..=12 {
                cuts.push((bounds.0 + (bounds.1 - bounds.0) * (s as f64) / 12.0).clamp(start, end));
            }
        }
        cuts.sort_by(|a, b| a.total_cmp(b));
        cuts.dedup_by(|a, b| (*a - *b).abs() <= 1e-7);
        if reversed {
            cuts.reverse();
            cuts.into_iter()
                .map(|time| (end - time, self.span(time, end)))
                .collect()
        } else {
            cuts.into_iter()
                .map(|time| (time - start, self.span(start, time)))
                .collect()
        }
    }
}

pub fn expression(points: &[(f64, f64)]) -> String {
    let mut terms = String::new();
    let mut closes = String::new();
    for (index, bounds) in points.windows(2).enumerate() {
        let [a, b] = [bounds[0], bounds[1]];
        let rate = (b.1 - a.1) / (b.0 - a.0);
        let line = format!("{:.8}+(T-{:.8})*{rate:.8}", a.1, a.0);
        if index + 2 < points.len() {
            terms.push_str(&format!("if(lt(T,{:.8}),{line},", b.0));
            closes.push(')');
        } else {
            terms.push_str(&line);
        }
    }
    format!("({terms}{closes})/TB")
}
