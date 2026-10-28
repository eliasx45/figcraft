//! Fork: chromeless (figcraft-embed's bare canvas in Ghostex's EDITING tab): no tab strip, rail,
//! panels or toolbar, the canvas fills the window. It replaced the embed laying the tab strip and
//! rail out above and left of the drawable (which still drew the bottom toolbar).

use egui::pos2;
use figcraft_engine::Session;

use crate::layers_drag_tests::Ui;

#[test]
fn chromeless_the_canvas_fills_the_window() {
    let mut u = Ui::new(Session::new());
    u.app.ui.home = false;
    u.frame(vec![]);
    u.frame(vec![]);
    let with_chrome = u.app.canvas.rect;
    assert!(with_chrome.min.x >= 56.0 && with_chrome.min.y >= crate::tabs::HEIGHT, "rail and tab strip: {with_chrome:?}");

    u.app.ui.chromeless = true;
    u.frame(vec![]);
    u.frame(vec![]);
    let bare = u.app.canvas.rect;
    assert_eq!(bare.min, pos2(0.0, 0.0), "{bare:?}");
    assert_eq!((bare.width(), bare.height()), (1440.0, 900.0));

    // The control channel's ui.set turns it off again.
    let ctx = u.ctx.clone();
    crate::control::handle(&mut u.app, &ctx, "ui.set", &serde_json::json!({"chromeless": false})).unwrap();
    assert!(!u.app.ui.chromeless);
    u.frame(vec![]);
    u.frame(vec![]);
    assert_eq!(u.app.canvas.rect, with_chrome);
}
