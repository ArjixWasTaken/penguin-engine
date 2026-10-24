//! Sound layers: their files, the play state scripts give them, and
//! playback once audio is enabled.

use std::collections::HashMap;

use wpe_assets::scene::SceneObject;
use wpe_audio::{Playback, SoundOutput, TrackId};

use crate::WallpaperFiles;

/// What scripts do to a sound layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SoundCommand {
    Play,
    Pause,
    /// Pause and rewind.
    Stop,
    Volume(f32),
}

/// A sound layer's state as scripts see it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundState {
    pub playing: bool,
    pub volume: f32,
}

struct SoundLayer {
    id: i32,
    files: Vec<Vec<u8>>,
    mode: Playback,
    state: SoundState,
}

/// Every sound layer of a wallpaper and, when audio is on, their players.
pub struct Sounds {
    layers: Vec<SoundLayer>,
    output: Option<(SoundOutput, HashMap<i32, TrackId>)>,
    /// Master volume, scaling every layer's.
    master: f32,
    paused: bool,
}

impl Sounds {
    pub fn load(files: &WallpaperFiles, props: &wpe_scene::UserProperties) -> Self {
        let layers = files
            .scene
            .objects
            .iter()
            .filter_map(|obj| match obj {
                SceneObject::Sound(s) => Some(s),
                _ => None,
            })
            .filter_map(|s| {
                let files_data: Vec<Vec<u8>> = s
                    .sound
                    .iter()
                    .filter_map(|path| {
                        files
                            .vfs
                            .read(path)
                            .inspect_err(|e| tracing::warn!("sound {path}: {e}"))
                            .ok()
                            .map(|b| b.into_owned())
                    })
                    .collect();
                let mode = match s.playbackmode.as_deref() {
                    Some("single") => Playback::Single,
                    _ => Playback::Loop,
                };
                (!files_data.is_empty()).then_some(SoundLayer {
                    id: s.common.id,
                    files: files_data,
                    mode,
                    state: SoundState {
                        playing: !s.startsilent && props.flag(&s.common.visible),
                        volume: props.value(&s.volume),
                    },
                })
            })
            .collect();
        Self {
            layers,
            output: None,
            master: 1.0,
            paused: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Each sound layer's id and state.
    pub fn states(&self) -> HashMap<i32, SoundState> {
        self.layers.iter().map(|l| (l.id, l.state)).collect()
    }

    /// Open the audio output and play the layers that are playing, or close it.
    pub fn enable(&mut self, enabled: bool) {
        if !enabled {
            self.output = None;
            return;
        }
        if self.output.is_some() || self.layers.is_empty() {
            return;
        }
        let mut out = match SoundOutput::open() {
            Ok(out) => out,
            Err(e) => {
                tracing::warn!("audio output unavailable: {e}");
                return;
            }
        };
        let mut tracks = HashMap::new();
        for l in &self.layers {
            let playing = l.state.playing && !self.paused;
            match out.add(
                l.files.clone(),
                l.state.volume * self.master,
                l.mode,
                playing,
            ) {
                Ok(track) => {
                    tracks.insert(l.id, track);
                }
                Err(e) => tracing::warn!("sound layer {}: {e}", l.id),
            }
        }
        self.output = Some((out, tracks));
    }

    pub fn apply(&mut self, id: i32, command: SoundCommand) {
        let Some(layer) = self.layers.iter_mut().find(|l| l.id == id) else {
            return;
        };
        match command {
            SoundCommand::Play => layer.state.playing = true,
            SoundCommand::Pause | SoundCommand::Stop => layer.state.playing = false,
            SoundCommand::Volume(v) => layer.state.volume = v,
        }
        let Some((out, tracks)) = &self.output else {
            return;
        };
        let Some(&track) = tracks.get(&id) else {
            return;
        };
        match command {
            SoundCommand::Play if self.paused => {}
            SoundCommand::Play => {
                if let Err(e) = out.resume(track) {
                    tracing::warn!("sound layer {id}: {e}");
                }
            }
            SoundCommand::Pause => out.pause(track),
            SoundCommand::Stop => out.stop(track),
            SoundCommand::Volume(v) => out.set_volume(track, v * self.master),
        }
    }

    /// Scale every layer's volume by `master`.
    pub fn set_master(&mut self, master: f32) {
        self.master = master;
        if let Some((out, tracks)) = &self.output {
            for l in &self.layers {
                if let Some(&track) = tracks.get(&l.id) {
                    out.set_volume(track, l.state.volume * master);
                }
            }
        }
    }

    /// Pause every layer, or resume the ones that are playing.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        let Some((out, tracks)) = &self.output else {
            return;
        };
        for l in &self.layers {
            let Some(&track) = tracks.get(&l.id) else {
                continue;
            };
            if paused {
                out.pause(track);
            } else if l.state.playing
                && let Err(e) = out.resume(track)
            {
                tracing::warn!("sound layer {}: {e}", l.id);
            }
        }
    }
}
