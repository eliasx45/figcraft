//! Native Figma paste, with image loading kept outside engine commands and off the UI thread.
use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, TryRecvError};

use base64::Engine as _;
use serde_json::json;

use crate::FigApp;
use crate::i18n::tr;

/// The images of a Figma copy as the platform loaded them: bytes by hash, and why the others
/// couldn't be loaded (by hash; short reasons such as "timed out", never a signed URL).
#[derive(Debug, Default)]
pub struct Fetched {
    pub images: BTreeMap<String, Vec<u8>>,
    pub failed: BTreeMap<String, String>,
}

pub struct Pending {
    html: String,
    uid: u64,
    revision: u64,
    params: serde_json::Value,
    images: Receiver<Fetched>,
}

/// Returns true even on invalid recognized data: never paste an old internal selection instead.
pub fn paste(app: &mut FigApp, at: serde_json::Value) -> bool {
    if app.session.ds().is_ok_and(|d| d.text_edit.is_some()) {
        return false;
    }
    let Some(html) = app.services.clipboard_get_html().filter(|h| figcraft_format::fig::clipboard::recognizes(h)) else { return false };
    if app.clipboard_pending.is_some() {
        app.toast(tr("Copied images are still loading…"));
        return true;
    }
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
    // Freeze the destination at the moment of Paste. Switching tabs/pages or editing while the
    // images load cancels the operation rather than writing into a different design.
    let Some(ds) = app.session.ds().ok() else { return true };
    let uid = ds.uid;
    let revision = ds.revision;
    let params = json!({"page": ds.page, "selection": app.session.selection(), "at": at});
    if !data.images.is_empty()
        && let Some(images) = app.services.clipboard_images(data.images)
    {
        app.clipboard_pending = Some(Pending { html, uid, revision, params, images });
        app.toast(tr("Loading copied images…"));
    } else {
        finish(app, &html, Fetched::default(), at);
    }
    true
}

fn finish(app: &mut FigApp, html: &str, fetched: Fetched, mut params: serde_json::Value) {
    let images: BTreeMap<_, _> = fetched.images.into_iter().map(|(k, b)| (k, base64::engine::general_purpose::STANDARD.encode(b))).collect();
    params["html"] = json!(html);
    params["images"] = json!(images);
    params["imageFailures"] = json!(fetched.failed);
    // `run`, not `run_user`: the params hold every image, too big to clone for the report.
    if let Some(r) = app.run("edit.pasteFigma", params) {
        crate::import_report::capture(app, "edit.pasteFigma", &json!({}), &r);
    }
}

pub fn poll(app: &mut FigApp, ctx: &egui::Context) {
    let Some(p) = app.clipboard_pending.take() else { return };
    let images = match p.images.try_recv() {
        Ok(images) => images,
        Err(TryRecvError::Empty) => {
            app.clipboard_pending = Some(p);
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        Err(TryRecvError::Disconnected) => Fetched::default(),
    };
    let same = app.session.ds().is_ok_and(|d| {
        d.uid == p.uid && d.revision == p.revision && json!(d.page) == p.params["page"] && json!(app.session.selection()) == p.params["selection"]
    });
    if same {
        finish(app, &p.html, images, p.params.get("at").cloned().unwrap_or_else(|| json!({})));
    } else {
        app.toast(tr("The paste destination changed while images loaded. Paste again."));
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
    fn late_image_reply_cannot_paste_into_another_document() {
        let mut app = FigApp::new(Session::new(), Box::new(Clipboard(String::new())));
        let (tx, rx) = std::sync::mpsc::channel();
        let ds = app.session.ds().unwrap();
        app.clipboard_pending = Some(Pending {
            html: html(),
            uid: ds.uid,
            revision: ds.revision,
            params: json!({"page":ds.page,"selection":app.session.selection()}),
            images: rx,
        });
        app.run("file.new", json!({"name":"Other design"}));
        let before = app.session.doc().unwrap().clone();
        tx.send(Fetched::default()).unwrap();
        poll(&mut app, &egui::Context::default());
        assert_eq!(app.session.doc().unwrap(), &before);
        assert!(app.clipboard_pending.is_none());
    }
}
