//! Only the image URLs explicitly carried by a Figma copy. No login, cookies or API token.
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use figcraft_ui_egui::clipboard::Fetched;

/// All images of one paste together. A storyboard page of phone screenshots named 376 images.
const MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// One image (the `.fig` import's own cap).
const MAX_IMAGE: u64 = 64 * 1024 * 1024;
/// Downloads at a time: a page of photos one after another took longer than any sane wait.
const WORKERS: usize = 8;
const PER_IMAGE: Duration = Duration::from_secs(30);
const OVERALL: Duration = Duration::from_secs(300);

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

/// One image: its bytes, or a short reason (never the signed URL, which must not be logged).
fn get(agent: &ureq::Agent, url: &str, budget: &AtomicU64) -> Result<Vec<u8>, String> {
    let mut response = agent.get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => format!("the server answered {code}"),
        ureq::Error::Timeout(_) => "timed out".into(),
        _ => "couldn't connect".into(),
    })?;
    let left = budget.load(Ordering::Relaxed);
    if left == 0 {
        return Err("over the 1 GB limit for one paste".into());
    }
    let bytes = response.body_mut().with_config().limit(left.min(MAX_IMAGE)).read_to_vec().map_err(|e| match e {
        ureq::Error::BodyExceedsLimit(_) => "too large".to_string(),
        ureq::Error::Timeout(_) => "timed out".into(),
        _ => "the download broke off".into(),
    })?;
    let len = bytes.len() as u64;
    budget.try_update(Ordering::Relaxed, Ordering::Relaxed, |b| b.checked_sub(len)).map_err(|_| "over the 1 GB limit for one paste".to_string())?;
    Ok(bytes)
}

pub fn fetch(images: BTreeMap<String, String>) -> Option<Receiver<Fetched>> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("figcraft-clipboard-images".into())
        .spawn(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder().https_only(true).max_redirects(0).timeout_global(Some(PER_IMAGE)).build().into();
            let started = Instant::now();
            let budget = Arc::new(AtomicU64::new(MAX_BYTES));
            let out = Arc::new(Mutex::new(Fetched::default()));
            let mut queue = VecDeque::new();
            for (hash, url) in images.into_iter().take(figcraft_format::fig::clipboard::MAX_IMAGES) {
                if allowed(&url, &hash) {
                    queue.push_back((hash, url));
                } else if let Ok(mut o) = out.lock() {
                    o.failed.insert(hash, "not a Figma image link".into());
                }
            }
            let queue = Arc::new(Mutex::new(queue));
            let workers: Vec<_> = (0..WORKERS)
                .filter_map(|i| {
                    let (agent, budget, out, queue) = (agent.clone(), budget.clone(), out.clone(), queue.clone());
                    std::thread::Builder::new()
                        .name(format!("figcraft-clipboard-image-{i}"))
                        .spawn(move || {
                            while let Some((hash, url)) = queue.lock().ok().and_then(|mut q| q.pop_front()) {
                                let result = if started.elapsed() >= OVERALL {
                                    Err("not started within five minutes".into())
                                } else {
                                    get(&agent, &url, &budget)
                                };
                                if let Ok(mut o) = out.lock() {
                                    match result {
                                        Ok(bytes) => o.images.insert(hash, bytes).map(drop),
                                        Err(why) => o.failed.insert(hash, why).map(drop),
                                    };
                                }
                            }
                        })
                        .ok()
                })
                .collect();
            for w in workers {
                let _ = w.join();
            }
            // Images no worker reached (none could start) count as failed too.
            let leftover: Vec<_> = queue.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default();
            let fetched = out.lock().map(|mut o| {
                for (hash, _) in leftover {
                    o.failed.insert(hash, "couldn't start a download".into());
                }
                std::mem::take(&mut *o)
            });
            let _ = tx.send(fetched.unwrap_or_default());
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
