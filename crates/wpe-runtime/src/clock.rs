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
    /// Named events at frames, for scripts' `animationEvent`.
    pub events: Vec<(f32, String)>,
    /// Events passed since the last [`Clocks::take_events`]: name and frame.
    fired: Vec<(String, f32)>,
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
            events: Vec::new(),
            fired: Vec::new(),
            ended: false,
        }
    }

    pub fn for_animation(a: &Animation) -> Self {
        let mut clock = Self::new(
            a.fps,
            a.length,
            a.mode == AnimationMode::Single,
            !a.start_paused,
        );
        clock.events = a.events.clone();
        clock
    }

    /// Record the events between frames `from` (exclusive) and `to`.
    fn pass_events(&mut self, from: f32, to: f32) {
        if self.events.is_empty() || to <= from {
            return;
        }
        let looping = self.frames > 0.0 && !self.single;
        for (frame, name) in &self.events {
            let crossed = if looping {
                // Event positions repeat every loop.
                let first = frame + ((from - frame) / self.frames).floor() * self.frames;
                let next = if first <= from {
                    first + self.frames
                } else {
                    first
                };
                next <= to
            } else {
                *frame > from && *frame <= to
            };
            if crossed {
                self.fired.push((name.clone(), *frame));
            }
        }
    }

    fn advance(&mut self, dt: f32) {
        if !self.playing || self.fps <= 0.0 {
            return;
        }
        let before = self.frame;
        self.frame += dt * self.fps * self.rate;
        self.pass_events(before, self.frame);
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

    /// Forget animation layer `index` of `layer`, moving later layers' clocks
    /// down to their new indices.
    pub fn remove_layer_clock(&mut self, layer: i32, index: usize) {
        self.clocks.remove(&(layer, layer_key(index)));
        let mut i = index + 1;
        while let Some(clock) = self.clocks.remove(&(layer, layer_key(i))) {
            self.clocks.insert((layer, layer_key(i - 1)), clock);
            i += 1;
        }
    }

    /// The layer with a clock `key` (`thisScene.getAnimation(name)`).
    pub fn find_layer(&self, key: &str) -> Option<i32> {
        self.clocks
            .keys()
            .filter(|(_, k)| k == key)
            .map(|&(l, _)| l)
            .min()
    }

    /// Every clock key of `layer`, sorted.
    pub fn layer_keys(&self, layer: i32) -> Vec<String> {
        let mut keys: Vec<String> = self
            .clocks
            .keys()
            .filter(|(l, _)| *l == layer)
            .map(|(_, k)| k.clone())
            .collect();
        keys.sort();
        keys
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

    /// Move every clock to scene time `time`; returns the step in seconds.
    pub fn advance(&mut self, time: f32) -> f32 {
        let dt = self.last.map_or(time, |last| time - last).max(0.0);
        self.last = Some(time);
        for clock in self.clocks.values_mut() {
            clock.advance(dt);
        }
        dt
    }

    /// Clocks that reached their end since the last call: layer and key.
    pub fn take_ended(&mut self) -> Vec<(i32, String)> {
        let mut ended: Vec<(i32, String)> = self
            .clocks
            .iter_mut()
            .filter(|(_, clock)| clock.ended)
            .map(|((layer, key), clock)| {
                clock.ended = false;
                (*layer, key.clone())
            })
            .collect();
        ended.sort();
        ended
    }

    /// Animation events passed since the last call: layer, name and frame.
    pub fn take_events(&mut self) -> Vec<(i32, String, f32)> {
        let mut events: Vec<(i32, String, f32)> = self
            .clocks
            .iter_mut()
            .flat_map(|((layer, _), clock)| {
                std::mem::take(&mut clock.fired)
                    .into_iter()
                    .map(|(name, frame)| (*layer, name, frame))
            })
            .collect();
        events.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.total_cmp(&b.2)));
        events
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
    fn events_fire_as_frames_pass_and_on_every_loop() {
        let mut clocks = Clocks::default();
        let mut clock = Clock::new(10.0, 20.0, false, true);
        clock.events = vec![(5.0, "step".into()), (15.0, "land".into())];
        clocks.register(1, "a".into(), clock);
        clocks.advance(0.4);
        assert!(clocks.take_events().is_empty());
        clocks.advance(1.0);
        assert_eq!(clocks.take_events(), [(1, "step".to_owned(), 5.0)]);
        clocks.advance(2.6); // frames 10 -> 26: land, then step again in the next loop
        let names: Vec<String> = clocks.take_events().into_iter().map(|e| e.1).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"land".to_owned()) && names.contains(&"step".to_owned()));
    }

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
        assert_eq!(clocks.take_ended(), [(1, "a".to_owned())]);
        assert!(clocks.take_ended().is_empty());
    }
}
