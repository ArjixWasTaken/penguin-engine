//! Keyframed property animations (`"animation"` blocks on scene values).

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationMode {
    /// Restart from frame 0.
    #[default]
    Loop,
    /// Play forwards then backwards.
    Mirror,
    /// Play once and hold the last frame.
    Single,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Key {
    frame: f32,
    value: f32,
}

/// Per-channel keyframes (`c0`..`c3`) with playback options. Keys are
/// interpolated linearly, as the reference renderer does.
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub mode: AnimationMode,
    pub fps: f32,
    /// Length in frames.
    pub length: f32,
    /// Keyframe values are offsets from the base value.
    pub relative: bool,
    /// In loop mode, interpolate from the last key back to the first.
    pub wraploop: bool,
    pub start_paused: bool,
    channels: [Vec<Key>; 4],
}

fn number(v: &Value) -> Option<f32> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(*b as u8 as f32),
        _ => None,
    }
}

impl Animation {
    /// Parse an `"animation"` object; `None` when it has no keyframes.
    pub fn parse(json: &Value) -> Option<Self> {
        let opts = json.get("options");
        let opt = |k: &str| opts.and_then(|o| o.get(k));
        let mode = match opt("mode").and_then(Value::as_str) {
            Some("mirror") => AnimationMode::Mirror,
            Some("single") => AnimationMode::Single,
            _ => AnimationMode::Loop,
        };
        let flag = |v: Option<&Value>| v.and_then(Value::as_bool).unwrap_or(false);
        let mut channels: [Vec<Key>; 4] = Default::default();
        for (i, ch) in channels.iter_mut().enumerate() {
            let Some(keys) = json.get(format!("c{i}")).and_then(Value::as_array) else {
                continue;
            };
            *ch = keys
                .iter()
                .filter_map(|k| {
                    Some(Key {
                        frame: number(k.get("frame")?)?,
                        value: number(k.get("value")?)?,
                    })
                })
                .collect();
            ch.sort_by(|a, b| a.frame.total_cmp(&b.frame));
        }
        if channels.iter().all(Vec::is_empty) {
            return None;
        }
        Some(Self {
            mode,
            fps: opt("fps").and_then(number).unwrap_or(30.0),
            length: opt("length").and_then(number).unwrap_or(0.0),
            relative: flag(json.get("relative")),
            wraploop: flag(opt("wraploop")),
            start_paused: flag(opt("startpaused")),
            channels,
        })
    }

    fn channel_at(&self, keys: &[Key], time: f32) -> Option<f32> {
        let (first, last) = (keys.first()?, keys.last()?);
        if keys.len() == 1 || self.fps <= 0.0 {
            return Some(first.value);
        }
        let period = self.length / self.fps;
        if period <= 0.0 {
            return Some(first.value);
        }
        let t = match self.mode {
            AnimationMode::Loop => time.rem_euclid(period),
            AnimationMode::Mirror => {
                let u = time.rem_euclid(period * 2.0);
                if u < period { u } else { period * 2.0 - u }
            }
            AnimationMode::Single => time.clamp(0.0, period),
        };
        let frame = t * self.fps;
        if frame <= first.frame {
            return Some(first.value);
        }
        if frame >= last.frame {
            if self.wraploop && self.mode == AnimationMode::Loop && self.length > last.frame {
                let frac = ((frame - last.frame) / (self.length - last.frame)).min(1.0);
                return Some(last.value + frac * (first.value - last.value));
            }
            return Some(last.value);
        }
        let next = keys.partition_point(|k| k.frame < frame);
        let (a, b) = (keys[next - 1], keys[next]);
        let span = b.frame - a.frame;
        if span <= 0.0 {
            return Some(a.value);
        }
        Some(a.value + (frame - a.frame) / span * (b.value - a.value))
    }

    /// `base` with each animated channel replaced (or offset, when relative)
    /// at `time` seconds since the animation started.
    pub fn sample(&self, time: f32, base: &[f32]) -> Vec<f32> {
        let time = if self.start_paused { 0.0 } else { time };
        let mut out = base.to_vec();
        for (i, keys) in self.channels.iter().enumerate() {
            let Some(v) = self.channel_at(keys, time) else {
                continue;
            };
            if i >= out.len() {
                out.resize(i + 1, 0.0);
            }
            out[i] = if self.relative {
                base.get(i).copied().unwrap_or(0.0) + v
            } else {
                v
            };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anim(json: &str) -> Animation {
        Animation::parse(&serde_json::from_str(json).unwrap()).unwrap()
    }

    #[test]
    fn loops_and_interpolates() {
        let a = anim(
            r#"{"options":{"fps":10,"length":20},"c0":[{"frame":0,"value":0},{"frame":10,"value":1},{"frame":20,"value":0}]}"#,
        );
        assert_eq!(a.sample(0.5, &[9.0]), [0.5]);
        assert_eq!(a.sample(1.0, &[9.0]), [1.0]);
        assert_eq!(a.sample(2.5, &[9.0]), [0.5], "loops after 2 s");
    }

    #[test]
    fn mirror_single_and_relative() {
        let keys = r#""c1":[{"frame":0,"value":0},{"frame":10,"value":10}]"#;
        let m = anim(&format!(
            r#"{{"options":{{"fps":10,"length":10,"mode":"mirror"}},{keys}}}"#
        ));
        assert_eq!(m.sample(1.5, &[1.0, 1.0])[1], 5.0);
        let s = anim(&format!(
            r#"{{"options":{{"fps":10,"length":10,"mode":"single"}},{keys}}}"#
        ));
        assert_eq!(s.sample(5.0, &[0.0, 0.0])[1], 10.0);
        let r = anim(&format!(
            r#"{{"relative":true,"options":{{"fps":10,"length":10}},{keys}}}"#
        ));
        assert_eq!(r.sample(0.5, &[0.0, 100.0]), [0.0, 105.0]);
    }

    #[test]
    fn empty_is_none() {
        assert!(Animation::parse(&serde_json::json!({"options": {}})).is_none());
    }
}
