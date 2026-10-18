//! Sound files for the canvas: decode with FilmCraft's pure-Rust codecs (MP4/M4A/MOV audio with
//! its own AAC decoder; WAV, MP3, FLAC, Ogg Vorbis, AIFF through its bootstrap decoders), make the
//! peak envelope a waveform is drawn from, and read stereo PCM for playback.
//!
//! The decoders read the source at its own rate; reads at another rate are resampled here
//! (linear, by absolute sample position), as EffectCraft does: FilmCraft's MP4 reader asked for
//! another rate shifts the start of every read.

use std::sync::Arc;

/// The longest sound decoded for playback at once (seconds).
pub const MAX_PLAY_SECONDS: f64 = 30.0 * 60.0;
/// The biggest sound file accepted (bytes).
pub const MAX_FILE_BYTES: usize = 1 << 30;
/// Peak slices per second (5 ms: finer than a frame at any common rate).
pub const PEAK_RATE: u32 = 200;
/// File extensions accepted as sound.
pub const EXTENSIONS: &[&str] = &["wav", "wave", "bwf", "aif", "aiff", "mp3", "m4a", "aac", "mp4", "mov", "flac", "ogg", "oga"];

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("{0}")]
    Unsupported(String),
    #[error("can't decode the sound: {0}")]
    Decode(String),
}

/// Whether `name`'s extension is one [`open`] takes.
pub fn is_audio_name(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    EXTENSIONS.contains(&ext.as_str())
}

/// What a sound file holds.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundInfo {
    pub sample_rate: u32,
    pub channels: u16,
    /// Seconds.
    pub duration: f64,
    pub codec: String,
}

/// An opened sound.
pub struct Sound {
    #[cfg(not(target_arch = "wasm32"))]
    src: filmcraft_media::SharedSource,
    info: SoundInfo,
}

impl std::fmt::Debug for Sound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sound").field("info", &self.info).finish()
    }
}

/// Open the sound file `name` (its extension picks the decoder) from its bytes.
#[cfg(not(target_arch = "wasm32"))]
pub fn open(name: &str, bytes: Arc<[u8]>) -> Result<Sound, AudioError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(AudioError::Unsupported(format!("{name} is larger than {} MB", MAX_FILE_BYTES >> 20)));
    }
    // A name without an extension: try the containers by their contents.
    let name = if name.contains('.') { name.to_string() } else { format!("{name}.{}", sniff(&bytes)) };
    let src = filmcraft_codecs::open_bytes(&name, bytes).map_err(|e| AudioError::Unsupported(format!("{name}: {e}")))?;
    let a = src.info().audio.clone().ok_or_else(|| AudioError::Unsupported(format!("{name} has no sound")))?;
    if a.sample_rate == 0 {
        return Err(AudioError::Unsupported(format!("{name}: no sample rate")));
    }
    let duration = src.info().duration.seconds();
    let info = SoundInfo {
        sample_rate: a.sample_rate,
        channels: u16::try_from(a.channels).unwrap_or(u16::MAX),
        duration: if duration.is_finite() { duration.max(0.0) } else { 0.0 },
        codec: a.codec,
    };
    Ok(Sound { src, info })
}

/// Sound decoding isn't in the browser build yet.
#[cfg(target_arch = "wasm32")]
pub fn open(name: &str, _bytes: Arc<[u8]>) -> Result<Sound, AudioError> {
    Err(AudioError::Unsupported(format!("{name}: sound import isn't available in the browser yet")))
}

/// The extension a sound file's first bytes suggest.
#[cfg(not(target_arch = "wasm32"))]
fn sniff(b: &[u8]) -> &'static str {
    let at = |i: usize, s: &[u8]| b.get(i..i + s.len()) == Some(s);
    if at(0, b"RIFF") && at(8, b"WAVE") {
        "wav"
    } else if at(0, b"FORM") {
        "aiff"
    } else if at(4, b"ftyp") {
        "m4a"
    } else if at(0, b"fLaC") {
        "flac"
    } else if at(0, b"OggS") {
        "ogg"
    } else if at(0, b"ID3") || b.first() == Some(&0xFF) && b.get(1).is_some_and(|v| v & 0xE0 == 0xE0) {
        "mp3"
    } else {
        "wav"
    }
}

impl Sound {
    pub fn info(&self) -> &SoundInfo {
        &self.info
    }

    /// `frames` frames from sample `start` at the source's own rate, one Vec per channel (silence
    /// where the read fails or runs past the end).
    #[cfg(not(target_arch = "wasm32"))]
    fn read_native(&self, start: i64, frames: usize) -> Vec<Vec<f32>> {
        let ch = usize::from(self.info.channels.max(1));
        match self.src.audio(start.max(0), frames, self.info.sample_rate) {
            Ok(b) if !b.channels.is_empty() => {
                let mut c = b.channels;
                for v in &mut c {
                    v.resize(frames, 0.0);
                }
                c
            }
            _ => vec![vec![0.0; frames]; ch],
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn read_native(&self, _start: i64, frames: usize) -> Vec<Vec<f32>> {
        vec![vec![0.0; frames]; usize::from(self.info.channels.max(1))]
    }

    /// The peak envelope: the loudest sample (any channel) of each `1 / rate` second slice,
    /// scaled to 0..=255, for the whole sound (at most two million slices).
    pub fn peaks(&self, rate: u32) -> Vec<u8> {
        let native = self.info.sample_rate;
        if rate == 0 || native == 0 {
            return Vec::new();
        }
        let total = (self.info.duration * f64::from(native)).round().max(0.0) as u64;
        let slices = ((self.info.duration * f64::from(rate)).ceil().max(0.0) as u64).min(2_000_000);
        let mut out = vec![0u8; usize::try_from(slices).unwrap_or(0)];
        // Read about a second at a time.
        let chunk = u64::from(native).max(1);
        let mut at = 0u64;
        while at < total {
            let n = chunk.min(total - at);
            let bufs = self.read_native(i64::try_from(at).unwrap_or(i64::MAX), usize::try_from(n).unwrap_or(0));
            for c in &bufs {
                for (i, v) in c.iter().enumerate() {
                    let slice = (at + i as u64) * u64::from(rate) / u64::from(native);
                    let level = (v.abs().min(1.0) * 255.0).round() as u8;
                    if let Some(p) = usize::try_from(slice).ok().and_then(|s| out.get_mut(s))
                        && level > *p
                    {
                        *p = level;
                    }
                }
            }
            at += n;
        }
        out
    }

    /// Stereo PCM, interleaved L R L R…, from media second `from` to `to` at `rate` Hz. Mono is
    /// copied to both sides; more channels mix down (odd channels left, even right). At most
    /// [`MAX_PLAY_SECONDS`].
    pub fn stereo(&self, from: f64, to: f64, rate: u32) -> Vec<f32> {
        let native = self.info.sample_rate;
        if rate == 0 || native == 0 || !(from.is_finite() && to.is_finite()) || to <= from {
            return Vec::new();
        }
        let secs = (to - from).min(MAX_PLAY_SECONDS);
        let frames = (secs * f64::from(rate)).round().max(0.0) as usize;
        let s0 = (from * f64::from(rate)).round() as i64;
        // Output sample k sits at source position (s0 + k) × native / rate.
        let (native_i, rate_i) = (i128::from(native), i128::from(rate));
        let first = (i128::from(s0) * native_i).div_euclid(rate_i);
        let last = ((i128::from(s0) + frames as i128) * native_i).div_euclid(rate_i) + 1;
        let (Ok(first64), Ok(count)) = (i64::try_from(first), usize::try_from(last - first + 1)) else { return Vec::new() };
        let skip = usize::try_from(first64.saturating_neg()).unwrap_or(0);
        let mut src = self.read_native(first64.max(0), count.saturating_sub(skip));
        for c in &mut src {
            let mut v = vec![0.0; skip];
            v.append(c);
            *c = v;
        }
        let mix = |i: usize| -> (f32, f32) {
            match src.len() {
                0 => (0.0, 0.0),
                1 => {
                    let v = src.first().and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                    (v, v)
                }
                n => {
                    let (mut l, mut r) = (0.0, 0.0);
                    for (k, c) in src.iter().enumerate() {
                        let v = c.get(i).copied().unwrap_or(0.0);
                        if k % 2 == 0 { l += v } else { r += v }
                    }
                    let (nl, nr) = (n.div_ceil(2) as f32, (n / 2).max(1) as f32);
                    (l / nl, r / nr)
                }
            }
        };
        let native_rate = native == rate;
        let mut out = Vec::with_capacity(frames * 2);
        for k in 0..frames as i128 {
            let pos = (i128::from(s0) + k) * native_i;
            let i = usize::try_from(pos.div_euclid(rate_i) - first).unwrap_or(0);
            let (a, b) = (mix(i), if native_rate { mix(i) } else { mix(i + 1) });
            let f = (pos.rem_euclid(rate_i) as f64 / f64::from(rate)) as f32;
            out.push(a.0 + (b.0 - a.0) * f);
            out.push(a.1 + (b.1 - a.1) * f);
        }
        out
    }
}

/// A 16-bit PCM WAV file of `seconds` of a sine tone at `freq` Hz, its loudness ramping from
/// silence up to `peak` (0..1) and back: test and demo material.
pub fn tone_wav(seconds: f64, freq: f64, rate: u32, peak: f32) -> Vec<u8> {
    let frames = (seconds.max(0.0) * f64::from(rate)).round() as u32;
    let mut pcm = Vec::with_capacity(frames as usize * 2);
    for i in 0..frames {
        let t = f64::from(i) / f64::from(rate);
        let env = (std::f64::consts::PI * f64::from(i) / f64::from(frames.max(1))).sin() as f32 * peak;
        let v = ((t * freq * std::f64::consts::TAU).sin() as f32 * env * 32767.0) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    let mut w = Vec::with_capacity(44 + pcm.len());
    let len = u32::try_from(pcm.len()).unwrap_or(u32::MAX);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36u32.saturating_add(len)).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&len.to_le_bytes());
    w.extend_from_slice(&pcm);
    w
}

#[cfg(test)]
mod tests;
