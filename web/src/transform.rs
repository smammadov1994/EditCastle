use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[serde(rename = "in")]
    In,
    #[serde(rename = "out")]
    Out,
    #[serde(rename = "inOut", alias = "in_out")]
    InOut,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Zoom {
    pub mode: Mode,
    pub p1: f64,
    pub p2: f64,
    pub p3: f64,
    pub p4: f64,
    #[serde(default = "smooth")]
    pub smooth: bool,
}

fn smooth() -> bool {
    true
}

impl Zoom {
    fn starter(mode: Mode, duration: f64) -> Self {
        let span = (duration * 0.25).clamp(0.5, 2.0).min(duration / 2.0);
        let [p1, p2, p3, p4] = match mode {
            Mode::In => [0.0, span, duration, duration],
            Mode::Out => [0.0, 0.0, duration - span, duration],
            Mode::InOut => [0.0, span, duration - span, duration],
        };
        Self {
            mode,
            p1,
            p2,
            p3,
            p4,
            smooth: true,
        }
    }

    fn fit(&mut self, duration: f64) {
        self.p1 = self.p1.clamp(0.0, duration);
        self.p2 = self.p2.clamp(self.p1, duration);
        self.p3 = self.p3.clamp(self.p2, duration);
        self.p4 = self.p4.clamp(self.p3, duration);
    }

    fn amount(&self, at: f64) -> f64 {
        let ramp = |first: f64, last: f64| {
            if last - first <= 0.000001 {
                return f64::from(at >= first);
            }
            let k = ((at - first) / (last - first)).clamp(0.0, 1.0);
            if self.smooth {
                k * k * (3.0 - 2.0 * k)
            } else {
                k
            }
        };
        match self.mode {
            Mode::In => ramp(self.p1, self.p2),
            Mode::Out => 1.0 - ramp(self.p3, self.p4),
            Mode::InOut if at < self.p3 => ramp(self.p1, self.p2),
            Mode::InOut => 1.0 - ramp(self.p3, self.p4),
        }
    }

    pub fn expression(&self, time: &str) -> String {
        let ramp = |first: f64, last: f64| {
            if last - first <= 0.000001 {
                return format!("gte({time},{first:.8})");
            }
            let k = format!("clip(({time}-{first:.8})/{:.8},0,1)", last - first);
            if self.smooth {
                format!("({k}*{k}*(3-2*{k}))")
            } else {
                k
            }
        };
        match self.mode {
            Mode::In => ramp(self.p1, self.p2),
            Mode::Out => format!("(1-{})", ramp(self.p3, self.p4)),
            Mode::InOut => format!(
                "if(lt({time},{:.8}),{},1-{})",
                self.p3,
                ramp(self.p1, self.p2),
                ramp(self.p3, self.p4)
            ),
        }
    }
}

#[derive(Clone, Copy)]
pub struct Transform {
    pub scale: f64,
    pub x: f64,
    pub y: f64,
    pub zoom: Option<Zoom>,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
            zoom: None,
        }
    }
}

impl Transform {
    pub fn read(raw: &Value, duration: f64) -> Self {
        let mut transform = Self {
            scale: raw["scale"].as_f64().unwrap_or(1.0).clamp(0.05, 10.0),
            x: raw["x"].as_f64().unwrap_or(0.0).clamp(-2.0, 2.0),
            y: raw["y"].as_f64().unwrap_or(0.0).clamp(-2.0, 2.0),
            zoom: serde_json::from_value(raw["zoom"].clone()).ok(),
        };
        if transform.zoom.is_none() && raw["zoom"].is_string() {
            // Older browser projects kept the tool's zoom arguments verbatim.
            return transform.updated(raw, duration).unwrap_or(transform);
        }
        if let Some(zoom) = &mut transform.zoom {
            zoom.fit(duration);
        }
        transform.clamp_offset();
        transform
    }

    fn clamp_offset(&mut self) {
        if self.scale > 1.001 {
            let bound = (self.scale - 1.0) / 2.0;
            self.x = self.x.clamp(-bound, bound);
            self.y = self.y.clamp(-bound, bound);
        }
    }

    pub fn updated(mut self, args: &Value, duration: f64) -> Result<Self> {
        if args["reset"] == true {
            self = Self::default();
        }
        let scale = args["scale"].as_f64();
        if let Some(scale) = scale {
            self.scale = scale.clamp(0.05, 10.0);
        }
        if let Some(x) = args["x"].as_f64() {
            self.x = x.clamp(-2.0, 2.0);
        }
        if let Some(y) = args["y"].as_f64() {
            self.y = y.clamp(-2.0, 2.0);
        }
        let easing = args["zoom_smooth"].as_bool();
        if let Some(command) = args["zoom"].as_str() {
            if command == "off" {
                self.zoom = None;
            } else {
                let mode = match command {
                    "in" => Mode::In,
                    "out" => Mode::Out,
                    "in_out" | "inOut" => Mode::InOut,
                    _ => bail!("Choose a zoom mode of in, in_out, out, or off."),
                };
                if scale.is_none() && self.scale <= 1.01 {
                    self.scale = 2.0;
                }
                let mut zoom = Zoom::starter(mode, duration);
                let limit = if mode == Mode::InOut {
                    duration / 2.0
                } else {
                    duration
                };
                let span = args["zoom_seconds"]
                    .as_f64()
                    .map_or((zoom.p2 - zoom.p1).max(zoom.p4 - zoom.p3), |d| {
                        d.max(0.05).min(limit)
                    });
                let start = args["zoom_start"].as_f64();
                match mode {
                    Mode::In => {
                        zoom.p1 = start.unwrap_or(0.0);
                        zoom.p2 = zoom.p1 + span;
                    }
                    Mode::Out => {
                        zoom.p3 = start.unwrap_or(duration - span);
                        zoom.p4 = zoom.p3 + span;
                    }
                    Mode::InOut => {
                        zoom.p1 = start.unwrap_or(0.0);
                        zoom.p2 = zoom.p1 + span;
                        zoom.p3 = zoom.p2.max(duration - span);
                    }
                }
                self.zoom = Some(zoom);
            }
        } else if args["zoom"].is_object() {
            self.zoom = Some(serde_json::from_value(args["zoom"].clone())?);
        } else {
            ensure!(
                args["zoom"].is_null(),
                "Supply a zoom mode or four zoom points."
            );
        }
        if let Some(zoom) = &mut self.zoom {
            if let Some(smooth) = easing {
                zoom.smooth = smooth;
            }
            zoom.fit(duration);
        }
        if let Some(x) = args["focus_x"].as_f64() {
            self.x = (-(x.clamp(0.0, 1.0) - 0.5) * self.scale).clamp(-2.0, 2.0);
        }
        if let Some(y) = args["focus_y"].as_f64() {
            self.y = (-(y.clamp(0.0, 1.0) - 0.5) * self.scale).clamp(-2.0, 2.0);
        }
        self.clamp_offset();
        Ok(self)
    }

    pub fn value(&self) -> Value {
        if self.zoom.is_none() && self.scale == 1.0 && self.x == 0.0 && self.y == 0.0 {
            return Value::Null;
        }
        json!({"scale":self.scale,"x":self.x,"y":self.y,"zoom":self.zoom})
    }

    pub fn at(&self, elapsed: f64) -> [f64; 3] {
        let k = self.zoom.map_or(1.0, |zoom| zoom.amount(elapsed));
        [1.0 + (self.scale - 1.0) * k, self.x * k, self.y * k]
    }

    pub fn shift(&mut self, delta: f64, duration: f64) {
        if let Some(z) = &mut self.zoom {
            z.p1 -= delta;
            z.p2 -= delta;
            z.p3 -= delta;
            z.p4 -= delta;
            z.fit(duration);
        }
    }
}
