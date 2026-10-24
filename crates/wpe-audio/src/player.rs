//! Playback of a wallpaper's sound layers.

use std::io::Cursor;

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

/// The audio output plus one player per sound layer.
pub struct SoundOutput {
    sink: MixerDeviceSink,
    players: Vec<Player>,
}

impl SoundOutput {
    pub fn open() -> Result<Self, PlaybackError> {
        let mut sink = DeviceSinkBuilder::open_default_sink()?;
        sink.log_on_drop(false);
        Ok(Self {
            sink,
            players: Vec::new(),
        })
    }

    /// Start a sound layer from its encoded files (mp3, ogg, wav, flac, ...).
    pub fn play(
        &mut self,
        files: Vec<Vec<u8>>,
        volume: f32,
        mode: Playback,
    ) -> Result<(), PlaybackError> {
        let player = Player::connect_new(self.sink.mixer());
        player.set_volume(volume);
        match mode {
            Playback::Loop => {
                if let Some(first) = files.into_iter().next() {
                    player.append(Decoder::new_looped(Cursor::new(first))?);
                }
            }
            Playback::Single => {
                for file in files {
                    player.append(Decoder::new(Cursor::new(file))?);
                }
            }
        }
        self.players.push(player);
        Ok(())
    }

    pub fn set_paused(&self, paused: bool) {
        for p in &self.players {
            if paused { p.pause() } else { p.play() }
        }
    }
}
