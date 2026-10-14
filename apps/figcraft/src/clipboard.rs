//! Only the image URLs explicitly carried by a Figma copy. No login, cookies or API token.
use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

const MAX_BYTES: u64 = 64 * 1024 * 1024;

fn allowed(raw: &str, hash: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else { return false };
    hash.len() == 40
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
        && url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
        && matches!(url.host_str(), Some("s3-alpha-sig.figma.com" | "s3-alpha.figma.com"))
        && url.path().strip_prefix("/img/").is_some_and(|p| p.replace('/', "") == hash)
}

pub fn fetch(images: BTreeMap<String, String>) -> Option<Receiver<BTreeMap<String, Vec<u8>>>> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("figcraft-clipboard-images".into())
        .spawn(move || {
            let agent: ureq::Agent =
                ureq::Agent::config_builder().https_only(true).max_redirects(0).timeout_global(Some(Duration::from_secs(10))).build().into();
            let started = Instant::now();
            let mut out = BTreeMap::new();
            let mut remaining = MAX_BYTES;
            for (hash, url) in images.into_iter().take(256) {
                if started.elapsed() >= Duration::from_secs(30) || remaining == 0 {
                    break;
                }
                if !allowed(&url, &hash) {
                    continue;
                }
                // Never log signed URLs. Failures become the shared importer's missing-image warnings.
                let Ok(mut response) = agent.get(&url).call() else { continue };
                if !response.status().is_success() {
                    continue;
                }
                if let Ok(bytes) = response.body_mut().with_config().limit(remaining).read_to_vec() {
                    remaining = remaining.saturating_sub(bytes.len() as u64);
                    out.insert(hash, bytes);
                }
            }
            let _ = tx.send(out);
        })
        .ok()?;
    Some(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_figma_image_cdn_urls_for_the_named_image_are_fetched() {
        let h = "d889202b2f921ab1f2254b48fc31ae26554bcff5";
        let good = "https://s3-alpha-sig.figma.com/img/d889/202b/2f921ab1f2254b48fc31ae26554bcff5?Expires=123";
        assert!(allowed(good, h));
        for bad in [
            good.replace("https:", "http:"),
            good.replace("s3-alpha-sig.figma.com", "127.0.0.1"),
            good.replace("s3-alpha-sig.figma.com", "s3-alpha-sig.figma.com.evil.test"),
            good.replace("https://", "https://user@"),
            good.replace("/img/", "/other/"),
        ] {
            assert!(!allowed(&bad, h));
        }
        assert!(!allowed(good, "0000000000000000000000000000000000000000"));
    }
}
