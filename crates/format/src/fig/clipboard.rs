//! Figma's HTML clipboard envelope, observed through normal Copy on synthetic designs.
//! Its `figma` section is a bare canvas consumed by the existing `.fig` importer. Never execute
//! the surrounding HTML. Image bytes are separate; native services resolve the metadata URLs.
use std::collections::BTreeMap;

use base64::Engine as _;

use crate::FormatError;

/// The design data in the HTML (the plain design-layer paste reads up to the same).
pub const MAX_HTML: usize = 256 * 1024 * 1024;
const MAX_METADATA: usize = 16 * 1024 * 1024;
/// Images a copy may name. A storyboard page of about 40 frames named 376; past this they are left
/// out (counted in [`Contents::dropped`]), never the whole copy.
pub const MAX_IMAGES: usize = 4096;

pub struct Contents {
    pub canvas: Vec<u8>,
    pub images: BTreeMap<String, String>,
    /// Image links left out: past [`MAX_IMAGES`], or not a hash and an address.
    pub dropped: usize,
}

pub fn recognizes(html: &str) -> bool {
    html.contains("<!--(figma)")
}

fn section(html: &str, start: &str, end: &str, limit: usize) -> Result<Option<Vec<u8>>, FormatError> {
    let Some((_, tail)) = html.split_once(start) else { return Ok(None) };
    let (data, _) = tail.split_once(end).ok_or_else(|| super::corrupt("truncated clipboard section"))?;
    if data.len() > limit {
        return Err(super::corrupt("clipboard section is too large"));
    }
    base64::engine::general_purpose::STANDARD.decode(data).map(Some).map_err(|_| super::corrupt("invalid clipboard base64"))
}

pub fn decode(html: &str) -> Result<Contents, FormatError> {
    if html.len() > MAX_HTML {
        return Err(super::corrupt("clipboard exceeds 256 MiB"));
    }
    let canvas = section(html, "<!--(figma)", "(/figma)-->", MAX_HTML)?.ok_or_else(|| super::corrupt("no Figma clipboard data"))?;
    if !canvas.starts_with(b"fig-kiwi") {
        return Err(super::corrupt("unsupported Figma clipboard format"));
    }
    let mut images = BTreeMap::new();
    let mut dropped = 0;
    if let Some(meta) = section(html, "<!--(figmeta)", "(/figmeta)-->", MAX_METADATA)? {
        let meta: serde_json::Value = serde_json::from_slice(&meta).map_err(|_| super::corrupt("invalid clipboard metadata"))?;
        if let Some(entries) = meta.get("imageHashes").and_then(serde_json::Value::as_object) {
            for (hash, url) in entries {
                if images.len() < MAX_IMAGES
                    && hash.len() == 40
                    && hash.bytes().all(|b| b.is_ascii_hexdigit())
                    && let Some(url) = url.as_str().filter(|u| u.len() <= 16_384)
                {
                    images.insert(hash.to_ascii_lowercase(), url.to_string());
                } else {
                    dropped += 1;
                }
            }
        }
    }
    Ok(Contents { canvas, images, dropped })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_named_by_the_sha1_of_their_bytes_and_the_placeholder_is_recognised() {
        assert_eq!(crate::fig::image_hash(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let png = crate::rest::missing_image_png().unwrap();
        assert!(crate::rest::is_missing_image(png));
        assert!(!crate::rest::is_missing_image(&png[..png.len() - 1]));
        // Not one flat colour: a missing image must not pass for a grey shape.
        let img = image::load_from_memory(png).unwrap().to_rgba8();
        assert!(img.pixels().any(|p| p != img.get_pixel(0, 0)));
    }

    #[test]
    fn envelope_is_data_not_html_and_rejects_truncated_or_wrong_formats() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"fig-kiwi\0\0\0\0");
        let html = format!("<meta charset='utf-8'><span data-buffer=\"<!--(figma){b64}(/figma)-->\"></span>");
        assert!(recognizes(&html));
        assert_eq!(decode(&html).unwrap().canvas, b"fig-kiwi\0\0\0\0");
        for bad in ["<p>ordinary text</p>", "<!--(figma)AA==", "<!--(figma)@@(/figma)-->", "<!--(figma)YWJj(/figma)-->"] {
            assert!(decode(bad).is_err());
        }
    }
}
