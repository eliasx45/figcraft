//! AVFoundation on macOS: the only `unsafe` code in FigCraft (docs/adr/0001-video-ffi.md).
//!
//! An `AVURLAsset` reads the file; an `AVAssetReader` with a track output asks for BGRA frames
//! already scaled to the card, which the Apple media engine decodes (H.264, HEVC, ProRes). Rules:
//! every `unsafe` block has a `// SAFETY:` comment, nothing here panics, every failure is an
//! [`Error`], and each call runs in its own autorelease pool (the readers live on worker threads
//! that have none).

use std::path::Path;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAsset, AVAssetReader, AVAssetReaderStatus, AVAssetReaderTrackOutput, AVAssetTrack, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo, AVURLAsset,
};
use objc2_core_foundation::CFString;
use objc2_core_media::{CMTime, CMTimeRange, kCMTimePositiveInfinity};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelBufferHeightKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString, NSURL};

use crate::{Error, Frame, Info};

/// Biggest picture side accepted from a file (pixels).
const MAX_DIM: u32 = 16_384;

fn describe(e: &NSError) -> String {
    e.localizedDescription().to_string()
}

fn asset(path: &Path) -> Result<Retained<AVURLAsset>, Error> {
    if !path.is_file() {
        return Err(Error::Open(format!("{} isn't there", path.display())));
    }
    let s = path.to_str().ok_or_else(|| Error::Open("the file's path isn't valid text".into()))?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(s));
    // SAFETY: a file URL that exists, no options; AVURLAsset only reads the file.
    Ok(unsafe { AVURLAsset::URLAssetWithURL_options(&url, None) })
}

/// The asset's tracks of `kind` (`AVMediaTypeVideo` / `AVMediaTypeAudio`).
fn tracks(asset: &AVAsset, kind: Option<&'static AVMediaType>) -> Option<Retained<NSArray<AVAssetTrack>>> {
    let kind = kind?;
    // SAFETY: a live asset and one of AVFoundation's media type constants. The synchronous form
    // loads the file's track table on this (worker or command) thread, which is what we want.
    #[allow(deprecated)]
    Some(unsafe { asset.tracksWithMediaType(kind) })
}

fn video_track(asset: &AVAsset) -> Result<Retained<AVAssetTrack>, Error> {
    // SAFETY: reading an immutable framework constant.
    let kind = unsafe { AVMediaTypeVideo };
    tracks(asset, kind).and_then(|t| t.firstObject()).ok_or_else(|| Error::Open("there is no picture in the file".into()))
}

fn dim(v: f64) -> u32 {
    if v.is_finite() { (v.abs().round() as u32).clamp(1, MAX_DIM) } else { 1 }
}

fn seconds(t: CMTime) -> f64 {
    // SAFETY: CMTimeGetSeconds is a pure function of the value.
    let s = unsafe { t.seconds() };
    if s.is_finite() { s } else { 0.0 }
}

pub fn probe(path: &Path) -> Result<Info, Error> {
    autoreleasepool(|_| {
        let asset = asset(path)?;
        let video = video_track(&asset)?;
        // SAFETY: property reads on a live track and asset.
        let (size, fps, duration) = unsafe { (video.naturalSize(), video.nominalFrameRate(), asset.duration()) };
        // SAFETY: reading an immutable framework constant.
        let audio = unsafe { AVMediaTypeAudio };
        let sound = tracks(&asset, audio).is_some_and(|t| t.count() > 0);
        let duration = seconds(duration);
        if duration <= 0.0 {
            return Err(Error::Open("the video has no length".into()));
        }
        let fps = f64::from(fps);
        Ok(Info { duration, width: dim(size.width), height: dim(size.height), fps: if fps.is_finite() && fps > 0.0 { fps } else { 30.0 }, sound })
    })
}

/// CoreVideo's key constants are CFStrings, toll-free bridged to NSString.
fn ns(s: &CFString) -> &NSString {
    // SAFETY: CFString and NSString are toll-free bridged: the same object, either type.
    unsafe { &*(s as *const CFString).cast::<NSString>() }
}

pub struct Reader {
    reader: Retained<AVAssetReader>,
    output: Retained<AVAssetReaderTrackOutput>,
    done: bool,
}

impl Reader {
    pub fn open(path: &Path, from: f64, until: f64, max_side: u32) -> Result<Reader, Error> {
        autoreleasepool(|_| {
            let asset = asset(path)?;
            let track = video_track(&asset)?;
            // SAFETY: a property read on a live track.
            let size = unsafe { track.naturalSize() };
            let (w, h) = crate::fit(dim(size.width), dim(size.height), max_side);
            // SAFETY: an asset made above; the reader only reads it.
            let reader = unsafe { AVAssetReader::assetReaderWithAsset_error(&asset) }.map_err(|e| Error::Open(describe(&e)))?;
            // SAFETY: reading immutable framework constants.
            let keys = unsafe { [ns(kCVPixelBufferPixelFormatTypeKey), ns(kCVPixelBufferWidthKey), ns(kCVPixelBufferHeightKey)] };
            let format = i32::try_from(kCVPixelFormatType_32BGRA).unwrap_or(0x4247_5241);
            let values = [NSNumber::numberWithInt(format), NSNumber::numberWithInt(w as i32), NSNumber::numberWithInt(h as i32)];
            let objects: [&AnyObject; 3] = [&values[0], &values[1], &values[2]];
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &objects);
            // SAFETY: a video track of this reader's asset and pixel buffer attributes (BGRA, the
            // size to scale to), which a track output accepts as its output settings.
            let output = unsafe { AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(&track, Some(&settings)) };
            // SAFETY: setters and adding the output before reading starts, as the API requires.
            unsafe {
                output.setAlwaysCopiesSampleData(false);
                if !reader.canAddOutput(&output) {
                    return Err(Error::Open("can't read the picture track".into()));
                }
                reader.addOutput(&output);
            }
            // SAFETY: CMTimeMakeWithSeconds is a pure function; the infinity constant is immutable.
            let range = unsafe {
                let start = CMTime::with_seconds(from, 600);
                let duration = if until.is_finite() && until > from { CMTime::with_seconds(until - from, 600) } else { kCMTimePositiveInfinity };
                CMTimeRange { start, duration }
            };
            // SAFETY: the time range is set before reading starts, as the API requires.
            let started = unsafe {
                reader.setTimeRange(range);
                reader.startReading()
            };
            if !started {
                // SAFETY: a property read on the reader that failed to start.
                let why = unsafe { reader.error() }.map_or_else(|| "the decoder refused it".to_string(), |e| describe(&e));
                return Err(Error::Decode(why));
            }
            Ok(Reader { reader, output, done: false })
        })
    }

    pub fn next(&mut self) -> Option<Frame> {
        if self.done {
            return None;
        }
        autoreleasepool(|_| {
            loop {
                // SAFETY: a property read on a live reader.
                if unsafe { self.reader.status() } != AVAssetReaderStatus::Reading {
                    self.done = true;
                    return None;
                }
                // SAFETY: the output was added to this reader and reading started (status is
                // Reading), the condition under which copyNextSampleBuffer may be called.
                let Some(sample) = (unsafe { self.output.copyNextSampleBuffer() }) else {
                    self.done = true;
                    return None;
                };
                // SAFETY: property reads on a sample buffer the output just handed us.
                let (time, image) = unsafe { (sample.presentation_time_stamp(), sample.image_buffer()) };
                let Some(image) = image else { continue };
                return match copy_rgba(&image) {
                    Some((width, height, rgba)) => Some(Frame { time: seconds(time), width, height, rgba }),
                    None => {
                        self.done = true;
                        None
                    }
                };
            }
        })
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if !self.done {
            // SAFETY: cancelling a live reader is always allowed.
            unsafe { self.reader.cancelReading() };
        }
    }
}

/// A BGRA pixel buffer's pixels as RGBA rows without padding.
fn copy_rgba(pb: &CVPixelBuffer) -> Option<(u32, u32, Vec<u8>)> {
    if CVPixelBufferGetPixelFormatType(pb) != kCVPixelFormatType_32BGRA {
        return None;
    }
    // SAFETY: locks the buffer for reading before its base address is touched; unlocked below on
    // every path.
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return None;
    }
    let out = (|| {
        let (w, h, stride) = (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb), CVPixelBufferGetBytesPerRow(pb));
        let base = CVPixelBufferGetBaseAddress(pb) as *const u8;
        let row = w.checked_mul(4)?;
        if base.is_null() || w == 0 || h == 0 || stride < row || w > MAX_DIM as usize || h > MAX_DIM as usize {
            return None;
        }
        let len = stride.checked_mul(h)?;
        // SAFETY: the locked buffer holds `h` rows of `stride` bytes at `base`, alive until the
        // unlock below.
        let src = unsafe { std::slice::from_raw_parts(base, len) };
        let mut rgba = Vec::with_capacity(row * h);
        for line in src.chunks_exact(stride).take(h) {
            for px in line.get(..row)?.chunks_exact(4) {
                rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        Some((w as u32, h as u32, rgba))
    })();
    // SAFETY: matches the lock above.
    unsafe { CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) };
    out
}
