//! Playback of a wallpaper's sound layers.

use std::io::Cursor;
use std::sync::Arc;

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player};

#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("opening the audio output: {0}")]
    Output(#[from] rodio::DeviceSinkError),
    #[error("decoding the sound: {0}")]
    Decode(#[from] rodio::decoder::DecoderError),
}

/// How a sound layer cycles through its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Playback {
    /// Repeat the first file forever.
    Loop,
    /// Play each file once, in order.
    Single,
}

/// One sound layer's player and what it plays.
struct Track {
    player: Player,
    files: Vec<Arc<[u8]>>,
    mode: Playback,
}

impl Track {
    /// Queue the layer's files from the start.
    fn queue(&self) -> Result<(), PlaybackError> {
        match self.mode {
            Playback::Loop => {
                if let Some(first) = self.files.first() {
                    self.player
                        .append(Decoder::new_looped(Cursor::new(Arc::clone(first)))?);
                }
            }
            Playback::Single => {
                for file in &self.files {
                    self.player.append(Decoder::new(Cursor::new(Arc::clone(file)))?);
                }
            }
        }
        Ok(())
    }
}

/// Handle to a sound layer added to a [`SoundOutput`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackId(usize);

/// The audio output plus one player per sound layer.
pub struct SoundOutput {
    sink: MixerDeviceSink,
    tracks: Vec<Option<Track>>,
}

impl SoundOutput {
    pub fn open() -> Result<Self, PlaybackError> {
        let mut sink = DeviceSinkBuilder::open_default_sink()?;
        sink.log_on_drop(false);
        Ok(Self {
            sink,
            tracks: Vec::new(),
        })
    }

    /// Add a sound layer from its encoded files (mp3, ogg, wav, flac, ...),
    /// playing or paused at the start.
    pub fn add(
        &mut self,
        files: impl IntoIterator<Item = impl Into<Arc<[u8]>>>,
        volume: f32,
        mode: Playback,
        playing: bool,
    ) -> Result<TrackId, PlaybackError> {
        let player = Player::connect_new(self.sink.mixer());
        player.set_volume(volume);
        if !playing {
            player.pause();
        }
        let track = Track {
            player,
            files: files.into_iter().map(Into::into).collect(),
            mode,
        };
        track.queue()?;
        self.tracks.push(Some(track));
        Ok(TrackId(self.tracks.len() - 1))
    }

    /// Start a sound layer playing right away.
    pub fn play(
        &mut self,
        files: impl IntoIterator<Item = impl Into<Arc<[u8]>>>,
        volume: f32,
        mode: Playback,
    ) -> Result<TrackId, PlaybackError> {
        self.add(files, volume, mode, true)
    }

    /// Resume, or start over when the layer finished or was stopped.
    pub fn resume(&self, id: TrackId) -> Result<(), PlaybackError> {
        let Some(Some(track)) = self.tracks.get(id.0) else {
            return Ok(());
        };
        if track.player.empty() {
            track.queue()?;
        }
        track.player.play();
        Ok(())
    }

    pub fn pause(&self, id: TrackId) {
        if let Some(Some(track)) = self.tracks.get(id.0) {
            track.player.pause();
        }
    }

    /// Stop and rewind: the next [`Self::resume`] starts from the beginning.
    pub fn stop(&self, id: TrackId) {
        if let Some(Some(track)) = self.tracks.get(id.0) {
            track.player.clear();
        }
    }

    pub fn set_volume(&self, id: TrackId, volume: f32) {
        if let Some(Some(track)) = self.tracks.get(id.0) {
            track.player.set_volume(volume);
        }
    }

    /// Whether the layer is audible: not paused and with something queued.
    pub fn is_playing(&self, id: TrackId) -> bool {
        self.tracks
            .get(id.0)
            .and_then(|t| t.as_ref())
            .is_some_and(|t| !t.player.is_paused() && !t.player.empty())
    }

    pub fn set_paused(&self, paused: bool) {
        for t in self.tracks.iter().flatten() {
            if paused {
                t.player.pause()
            } else {
                t.player.play()
            }
        }
    }

    /// Remove a track and release its resources.
    pub fn remove(&mut self, id: TrackId) {
        if let Some(slot) = self.tracks.get_mut(id.0)
            && let Some(track) = slot.take()
        {
            track.player.clear();
        }
    }

    /// Clear all tracks and release resources.
    pub fn clear(&mut self) {
        for track in self.tracks.drain(..).flatten() {
            track.player.clear();
        }
    }
}

impl Drop for SoundOutput {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_output_track_lifecycle() {
        let Ok(mut output) = SoundOutput::open() else {
            // Audio sink may not be available in headless environment
            return;
        };
        output.clear();
    }
}
