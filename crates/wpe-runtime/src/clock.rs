//! Clocks for a wallpaper's animations, which scripts can play, pause, seek
//! and speed up: keyframe animations (`getAnimation(name)`), sprite sheets
//! (`getTextureAnimation()`) and puppet animation layers
//! (`getAnimationLayer(i)`). Untouched, every clock runs with scene time.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wpe_scene::{Animation, AnimationMode};

/// Clock key of a keyframe animation; `name` may be empty.
pub fn animation_key(name: &str) -> String {
    format!("animation:{name}")
}

pub const TEXTURE_ANIMATION: &str = "textureAnimation";

pub fn layer_key(index: usize) -> String {
    format!("animationLayer:{index}")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clock {
    pub fps: f32,
    /// Length in frames; 0 for endless.
    pub frames: f32,
    /// Stops at the last frame instead of looping.
    pub single: bool,
    pub playing: bool,
    /// Position in frames, growing while playing (loops wrap when read).
    pub frame: f32,
    pub rate: f32,
    /// Puppet animation layers: weight and visibility.
    pub blend: f32,
    pub visible: bool,
    /// Reached the end since the last [`Clocks::take_ended`].
    ended: bool,
}

impl Clock {
    pub fn new(fps: f32, frames: f32, single: bool, playing: bool) -> Self {
        Self {
            fps,
            frames,
            single,
            playing,
            frame: 0.0,
            rate: 1.0,
            blend: 1.0,
            visible: true,
            ended: false,
        }
    }

    pub fn for_animation(a: &Animation) -> Self {
        Self::new(
            a.fps,
            a.length,
            a.mode == AnimationMode::Single,
            !a.start_paused,
        )
    }

    fn advance(&mut self, dt: f32) {
        if !self.playing || self.fps <= 0.0 {
            return;
        }
        self.frame += dt * self.fps * self.rate;
        if self.single && self.frames > 0.0 && self.frame >= self.frames {
            self.frame = self.frames;
            self.playing = false;
            self.ended = true;
        }
    }

    /// Seconds into the animation.
    pub fn seconds(&self) -> f32 {
        if self.fps > 0.0 {
            self.frame / self.fps
        } else {
            0.0
        }
    }

    /// The current frame within one pass of the animation.
    pub fn current_frame(&self) -> f32 {
        if self.frames > 0.0 && !self.single {
            self.frame.rem_euclid(self.frames)
        } else {
            self.frame
        }
    }

    /// Stop and rewind.
    pub fn stop(&mut self) {
        self.playing = false;
        self.frame = 0.0;
    }
}

/// Every clock of a wallpaper, by layer id and key.
#[derive(Debug, Default)]
pub struct Clocks {
    clocks: HashMap<(i32, String), Clock>,
    /// Scene time of the last advance.
    last: Option<f32>,
}

/// Clocks are shared by the frame's bindings and the script host.
pub type SharedClocks = Rc<RefCell<Clocks>>;

impl Clocks {
    /// Add a clock unless one exists (clocks outlive frame rebuilds).
    pub fn register(&mut self, layer: i32, key: String, clock: Clock) {
        self.clocks.entry((layer, key)).or_insert(clock);
    }

    pub fn get(&self, layer: i32, key: &str) -> Option<&Clock> {
        self.clocks.get(&(layer, key.to_owned()))
    }

    pub fn get_mut(&mut self, layer: i32, key: &str) -> Option<&mut Clock> {
        self.clocks.get_mut(&(layer, key.to_owned()))
    }

    /// The key of `layer`'s first keyframe animation (`getAnimation()`).
    pub fn first_animation(&self, layer: i32) -> Option<String> {
        let mut keys: Vec<&String> = self
            .clocks
            .keys()
            .filter(|(l, k)| *l == layer && k.starts_with("animation:"))
            .map(|(_, k)| k)
            .collect();
        keys.sort();
        keys.first().map(|k| (*k).clone())
    }

    /// Move every clock to scene time `time`.
    pub fn advance(&mut self, time: f32) {
        let dt = self.last.map_or(time, |last| time - last).max(0.0);
        self.last = Some(time);
        for clock in self.clocks.values_mut() {
            clock.advance(dt);
        }
    }

    /// Layers with a clock that reached its end since the last call.
    pub fn take_ended(&mut self) -> Vec<i32> {
        let mut ended = Vec::new();
        for ((layer, _), clock) in &mut self.clocks {
            if std::mem::take(&mut clock.ended) {
                ended.push(*layer);
            }
        }
        ended.sort_unstable();
        ended.dedup();
        ended
    }

    /// Seconds into `animation` of `layer`, by its clock (scene semantics
    /// without one).
    pub fn time_of(&self, layer: i32, animation: &Animation, scene_time: f32) -> f32 {
        match self.get(layer, &animation_key(&animation.name)) {
            Some(clock) => clock.seconds(),
            None if animation.start_paused => 0.0,
            None => scene_time,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_play_pause_and_end() {
        let mut clocks = Clocks::default();
        clocks.register(1, "a".into(), Clock::new(10.0, 20.0, true, true));
        clocks.register(2, "b".into(), Clock::new(10.0, 20.0, false, false));
        clocks.advance(1.0);
        assert_eq!(clocks.get(1, "a").unwrap().frame, 10.0);
        assert_eq!(clocks.get(2, "b").unwrap().frame, 0.0, "paused");
        clocks.get_mut(2, "b").unwrap().playing = true;
        clocks.get_mut(2, "b").unwrap().rate = 2.0;
        clocks.advance(2.5);
        assert_eq!(clocks.get(1, "a").unwrap().frame, 20.0, "single stops");
        assert!(!clocks.get(1, "a").unwrap().playing);
        assert_eq!(clocks.get(2, "b").unwrap().frame, 30.0);
        assert_eq!(clocks.get(2, "b").unwrap().current_frame(), 10.0, "loops");
        assert_eq!(clocks.take_ended(), [1]);
        assert!(clocks.take_ended().is_empty());
    }
}
