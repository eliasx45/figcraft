//! Native Figma paste, with image loading kept outside engine commands and off the UI thread.
//!
//! The layers paste at once, with each image as a missing-image placeholder under its own hash;
//! the images download in the background and fill those placeholders in when they arrive
//! (`image.relinkMissing`, its own undo step). Nothing the user does meanwhile can lose the paste.
use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, TryRecvError};

use base64::Engine as _;
use serde_json::json;

use crate::FigApp;
use crate::i18n::{tr, trf};

/// The images of a Figma copy as the platform loaded them: bytes by hash, and why the others
/// couldn't be loaded (by hash; short reasons such as "timed out", never a signed URL).
#[derive(Debug, Default)]
pub struct Fetched {
    pub images: BTreeMap<String, Vec<u8>>,
    pub failed: BTreeMap<String, String>,
}

/// One paste's images on their way.
struct Job {
    /// The document the layers were pasted into.
    uid: u64,
    /// Images the copy names.
    wanted: usize,
    images: Receiver<Fetched>,
    /// Downloaded, waiting for the user to come back to the document.
    arrived: Option<Fetched>,
}

/// The pastes whose images are still loading (several can be under way at once).
#[derive(Default)]
pub struct Pending {
    jobs: Vec<Job>,
}

/// Returns true even on invalid recognized data: never paste an old internal selection instead.
pub fn paste(app: &mut FigApp, at: serde_json::Value) -> bool {
    if app.session.ds().is_ok_and(|d| d.text_edit.is_some()) {
        return false;
    }
    let Some(html) = app.services.clipboard_get_html().filter(|h| figcraft_format::fig::clipboard::recognizes(h)) else { return false };
    let data = match figcraft_format::fig::clipboard::decode(&html) {
        Ok(data) => data,
        // Design data this path can't read (no Figma metadata, say) goes on to the plain design
        // layer paste ([`crate::menus::paste`]); anything else is refused, never pasted stale.
        Err(_) if figcraft_engine::cmd::edit::design_clipboard_buffer(&html).is_some() => return false,
        Err(e) => {
            app.toast(e.to_string());
            return true;
        }
    };
    let Some(uid) = app.session.ds().ok().map(|d| d.uid) else { return true };
    let receiver = if data.images.is_empty() { None } else { app.services.clipboard_images(data.images.clone()) };
    let mut params = at;
    params["html"] = json!(html);
    params["imagesPending"] = json!(receiver.is_some());
    // `run`, not `run_user`: the params hold the whole copy, too big to clone for the report.
    let Some(result) = app.run("edit.pasteFigma", params) else { return true };
    crate::import_report::capture(app, "edit.pasteFigma", &json!({}), &result);
    let missing = result.get("missingImages").and_then(serde_json::Value::as_array).map_or(0, Vec::len);
    if let Some(images) = receiver.filter(|_| missing > 0) {
        app.clipboard_pending.get_or_insert_with(Pending::default).jobs.push(Job { uid, wanted: missing, images, arrived: None });
        app.toast(trf("Loading {n} copied images…", &[("n", &missing)]));
    }
    true
}

/// Fill a paste's images in: the active document is the one the layers went into.
fn fill(app: &mut FigApp, wanted: usize, fetched: Fetched) {
    let images: BTreeMap<_, _> = fetched.images.into_iter().map(|(k, b)| (k, base64::engine::general_purpose::STANDARD.encode(b))).collect();
    let loaded = if images.is_empty() {
        0
    } else {
        app.run("image.relinkMissing", json!({"images": images})).and_then(|r| r.get("relinked").and_then(serde_json::Value::as_u64)).unwrap_or(0)
    };
    let loaded = usize::try_from(loaded).unwrap_or(0).min(wanted);
    if loaded == wanted {
        app.toast(trf("Loaded all {n} copied images", &[("n", &wanted)]));
        return;
    }
    // What didn't load, and why, in the report (a toast would vanish).
    let mut why: BTreeMap<&str, usize> = BTreeMap::new();
    for reason in fetched.failed.values() {
        *why.entry(reason.as_str()).or_default() += 1;
    }
    let unexplained = (wanted - loaded).saturating_sub(fetched.failed.len());
    if unexplained > 0 {
        why.insert("not loaded", unexplained);
    }
    let why: Vec<String> = why.into_iter().map(|(r, n)| format!("{n} {r}")).collect();
    let note = format!(
        "{} of {wanted} image(s) couldn't be loaded ({}); they show a missing-image placeholder. Paste again to retry (copy again in the other app first if its links expired), or File ▸ Relink missing images… fills them in from a .fig copy of the file or a folder of the images",
        wanted - loaded,
        why.join(", ")
    );
    let result = json!({"warnings": [note], "summary": {"name": tr("Pasted Figma layers"), "images": loaded}});
    crate::import_report::capture(app, "edit.pasteFigma", &json!({}), &result);
}

pub fn poll(app: &mut FigApp, ctx: &egui::Context) {
    let Some(mut pending) = app.clipboard_pending.take() else { return };
    let active = app.session.ds().ok().map(|d| d.uid);
    let mut done = Vec::new();
    for (i, job) in pending.jobs.iter_mut().enumerate() {
        if job.arrived.is_none() {
            match job.images.try_recv() {
                Ok(f) => job.arrived = Some(f),
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => job.arrived = Some(Fetched::default()),
            }
        }
        // A closed document needs no images; another tab's wait until it is in front again.
        if active == Some(job.uid) || !app.session.docs.iter().any(|d| d.uid == job.uid) {
            done.push(i);
        }
    }
    let mut finished = Vec::new();
    for i in done.into_iter().rev() {
        let job = pending.jobs.remove(i);
        if active == Some(job.uid) {
            finished.push(job);
        }
    }
    if !pending.jobs.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        app.clipboard_pending = Some(pending);
    }
    for job in finished {
        fill(app, job.wanted, job.arrived.unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use figcraft_doc::{Document, NodeKind};
    use figcraft_engine::Session;
    use std::io::Read;

    struct Clipboard(String);
    impl Services for Clipboard {
        fn clipboard_get_html(&mut self) -> Option<String> {
            Some(self.0.clone())
        }
        fn clipboard_get_text(&mut self) -> Option<String> {
            Some("Figma text fallback".into())
        }
        fn clipboard_get_image(&mut self) -> Option<(u32, u32, Vec<u8>)> {
            panic!("rich Figma data must win over its preview image")
        }
    }
    fn html() -> String {
        let mut d = Document::new("Clipboard test");
        d.create(d.first_page().unwrap(), NodeKind::Rectangle, "Native rectangle", 0.0, 0.0, 100.0, 50.0).unwrap();
        let (bytes, _) = figcraft_format::fig::export(&d, None).unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut canvas = Vec::new();
        z.by_name("canvas.fig").unwrap().read_to_end(&mut canvas).unwrap();
        format!("<!--(figma){}(/figma)-->", base64::engine::general_purpose::STANDARD.encode(canvas))
    }
    #[test]
    fn rich_clipboard_takes_priority_and_failure_never_pastes_stale_internal_content() {
        let mut app = FigApp::new(Session::new(), Box::new(Clipboard(html())));
        crate::menus::invoke(&mut app, "edit.pasteFigma");
        assert_eq!(app.session.doc().unwrap().find_by_name("Native rectangle", None).len(), 1);
        app.run("edit.copy", json!({}));
        let before = app.session.doc().unwrap().clone();
        app.services = Box::new(Clipboard("<!--(figma)broken(/figma)-->".into()));
        crate::menus::paste(&mut app, None, None);
        assert_eq!(app.session.doc().unwrap(), &before);
    }
    #[test]
    fn layers_paste_at_once_and_late_images_fill_their_own_document_only() {
        let mut d = Document::new("Clipboard test");
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([30, 140, 80, 255])).write_to(&mut encoded, image::ImageFormat::Png).unwrap();
        let png = encoded.into_inner();
        let (hash, _, _) = figcraft_format::add_image(&mut d, png.clone()).unwrap();
        let r = d.create(d.first_page().unwrap(), NodeKind::Rectangle, "Photo", 0.0, 0.0, 100.0, 50.0).unwrap();
        d.get_mut(r).unwrap().fills = vec![figcraft_color::Paint::image(hash.clone(), figcraft_color::ImageScaleMode::Fill)];
        let (bytes, _) = figcraft_format::fig::export(&d, None).unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut canvas = Vec::new();
        z.by_name("canvas.fig").unwrap().read_to_end(&mut canvas).unwrap();
        let html = format!("<!--(figma){}(/figma)-->", base64::engine::general_purpose::STANDARD.encode(canvas));

        let mut app = FigApp::new(Session::new(), Box::new(Clipboard(html.clone())));
        let uid = app.session.ds().unwrap().uid;
        let out = app.run("edit.pasteFigma", json!({"html": html, "imagesPending": true})).unwrap();
        assert!(out["warnings"].as_array().unwrap().is_empty(), "no missing-image note while they load: {out}");
        let key = out["missingImages"][0]["hash"].as_str().unwrap().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        app.clipboard_pending = Some(Pending { jobs: vec![Job { uid, wanted: 1, images: rx, arrived: None }] });
        // The user goes on working meanwhile, in another tab too.
        app.run("shape.rectangle", json!({"width": 10, "height": 10}));
        app.run("file.new", json!({"name": "Other design"}));
        let other = app.session.doc().unwrap().clone();
        tx.send(Fetched { images: BTreeMap::from([(key.clone(), png.clone())]), failed: BTreeMap::new() }).unwrap();
        poll(&mut app, &egui::Context::default());
        assert_eq!(app.session.doc().unwrap(), &other, "another document is never filled");
        assert!(app.clipboard_pending.is_some(), "the images wait for their document");
        let back = app.session.docs.iter().position(|d| d.uid == uid).unwrap();
        app.run("doc.switch", json!({"index": back}));
        poll(&mut app, &egui::Context::default());
        assert!(app.clipboard_pending.is_none());
        let doc = app.session.doc().unwrap();
        let photo = doc.get(doc.find_by_name("Photo", None)[0]).unwrap();
        let figcraft_color::PaintKind::Image(img) = &photo.fills[0].kind else { panic!("image fill") };
        assert_eq!(doc.images[&img.image].bytes.as_ref(), &png);
    }
}
