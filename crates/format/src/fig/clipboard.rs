//! Figma's HTML clipboard envelope, observed through normal Copy on synthetic designs.
//! Its `figma` section is a bare canvas consumed by the existing `.fig` importer. Never execute
//! the surrounding HTML. Image bytes are separate; native services resolve the metadata URLs.
use std::collections::BTreeMap;

use base64::Engine as _;

use crate::FormatError;

pub const MAX_HTML: usize = 64 * 1024 * 1024;
const MAX_METADATA: usize = 1024 * 1024;

pub struct Contents {
    pub canvas: Vec<u8>,
    pub images: BTreeMap<String, String>,
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
        return Err(super::corrupt("clipboard exceeds 64 MiB"));
    }
    let canvas = section(html, "<!--(figma)", "(/figma)-->", MAX_HTML)?.ok_or_else(|| super::corrupt("no Figma clipboard data"))?;
    if !canvas.starts_with(b"fig-kiwi") {
        return Err(super::corrupt("unsupported Figma clipboard format"));
    }
    let mut images = BTreeMap::new();
    if let Some(meta) = section(html, "<!--(figmeta)", "(/figmeta)-->", MAX_METADATA)? {
        let meta: serde_json::Value = serde_json::from_slice(&meta).map_err(|_| super::corrupt("invalid clipboard metadata"))?;
        if let Some(entries) = meta.get("imageHashes").and_then(serde_json::Value::as_object) {
            if entries.len() > 256 {
                return Err(super::corrupt("clipboard has more than 256 images"));
            }
            for (hash, url) in entries {
                if hash.len() == 40
                    && hash.bytes().all(|b| b.is_ascii_hexdigit())
                    && let Some(url) = url.as_str().filter(|u| u.len() <= 16_384)
                {
                    images.insert(hash.clone(), url.to_string());
                }
            }
        }
    }
    Ok(Contents { canvas, images })
}

#[cfg(test)]
mod tests {
    use super::*;

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
