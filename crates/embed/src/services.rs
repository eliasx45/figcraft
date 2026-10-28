//! What the canvas asks of its platform: file dialogs, the clipboard, links. FigCraft's own
//! preferences are read (theme, keymap, language) but never written: the standalone FigCraft
//! owns them.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use figcraft_ui_egui::{DialogFuture, Services};
use serde_json::Value;

/// Links the canvas asked to open, for the host (it opens them in its own browser).
static URLS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The oldest link waiting for the host.
pub(crate) fn take_url() -> Option<String> {
    let mut urls = URLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (!urls.is_empty()).then(|| urls.remove(0))
}

/// FigCraft's settings folder (the desktop app's).
fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.map(|b| b.join("FigCraft"))
}

/// The person's FigCraft preferences (`state.json`'s `prefs`), read only.
pub(crate) fn read_prefs() -> Option<figcraft_engine::Prefs> {
    let text = std::fs::read_to_string(config_dir()?.join("state.json")).ok()?;
    prefs_of(&text)
}

pub(crate) fn prefs_of(text: &str) -> Option<figcraft_engine::Prefs> {
    let state: Value = serde_json::from_str(text).ok()?;
    serde_json::from_value(state.get("prefs")?.clone()).ok()
}

pub(crate) struct EmbedServices {
    clipboard: Option<arboard::Clipboard>,
}

impl EmbedServices {
    pub(crate) fn new() -> Self {
        Self { clipboard: arboard::Clipboard::new().ok() }
    }
}

fn picked(pick: impl Future<Output = Option<rfd::FileHandle>> + 'static) -> DialogFuture {
    Box::pin(async move { pick.await.map(|f| f.path().to_path_buf()) })
}

impl Services for EmbedServices {
    fn pick_open(&mut self, title: &str, exts: &[&str]) -> DialogFuture {
        picked(rfd::AsyncFileDialog::new().set_title(title).add_filter("Supported files", exts).pick_file())
    }
    fn pick_save(&mut self, title: &str, name: &str, exts: &[&str]) -> DialogFuture {
        picked(rfd::AsyncFileDialog::new().set_title(title).set_file_name(name).add_filter("File", exts).save_file())
    }
    fn pick_folder(&mut self, title: &str) -> DialogFuture {
        picked(rfd::AsyncFileDialog::new().set_title(title).pick_folder())
    }
    fn clipboard_set_text(&mut self, s: &str) {
        if let Some(c) = self.clipboard.as_mut() {
            let _ = c.set_text(s.to_string());
        }
    }
    fn clipboard_get_text(&mut self) -> Option<String> {
        self.clipboard.as_mut()?.get_text().ok()
    }
    fn clipboard_get_html(&mut self) -> Option<String> {
        self.clipboard.as_mut()?.get().html().ok()
    }
    fn clipboard_set_png(&mut self, w: u32, h: u32, rgba: &[u8]) {
        if let Some(c) = self.clipboard.as_mut() {
            let _ = c.set_image(arboard::ImageData { width: w as usize, height: h as usize, bytes: std::borrow::Cow::Borrowed(rgba) });
        }
    }
    fn clipboard_get_image(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        let img = self.clipboard.as_mut()?.get_image().ok()?;
        let (w, h) = (u32::try_from(img.width).ok()?, u32::try_from(img.height).ok()?);
        // Refuse absurd sizes (and mismatched buffers) rather than allocate.
        if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 100_000_000 || img.bytes.len() != (w as usize) * (h as usize) * 4 {
            return None;
        }
        Some((w, h, img.bytes.into_owned()))
    }
    fn open_url(&mut self, url: &str) {
        URLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(url.to_string());
    }
    fn reveal_path(&mut self, path: &Path) {
        #[cfg(target_os = "macos")]
        if let Err(e) = std::process::Command::new("open").arg("-R").arg(path).spawn() {
            log::warn!("show in Finder: {}: {e}", path.display());
        }
        #[cfg(not(target_os = "macos"))]
        let _ = path;
    }
}
