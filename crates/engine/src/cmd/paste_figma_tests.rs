use super::*;
use std::io::Read;

fn html(doc: &Document) -> String {
    let (bytes, _) = figcraft_format::fig::export(doc, None).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut canvas = Vec::new();
    archive.by_name("canvas.fig").unwrap().read_to_end(&mut canvas).unwrap();
    format!("<span data-buffer=\"<!--(figma){}(/figma)-->\"></span>", base64::engine::general_purpose::STANDARD.encode(canvas))
}

fn fixture() -> Document {
    let mut doc = Document::new("Synthetic clipboard");
    let p = doc.first_page().unwrap();
    let frame = doc.create(p, NodeKind::Frame, "Copied frame", 20.0, 30.0, 240.0, 160.0).unwrap();
    doc.get_mut(frame).unwrap().clip = true;
    let r = doc.create(frame, NodeKind::Rectangle, "Editable rectangle", 10.0, 12.0, 60.0, 40.0).unwrap();
    doc.get_mut(r).unwrap().fills = vec![figcraft_color::Paint::solid(figcraft_color::Color::from_hex("123456").unwrap())];
    let t = doc
        .create(frame, NodeKind::Text(TextData { characters: "Retype me".into(), ..Default::default() }), "Editable text", 10.0, 75.0, 180.0, 40.0)
        .unwrap();
    doc.get_mut(t).unwrap().opacity = 0.75;
    doc
}

#[test]
fn clipboard_uses_file_import_mapping_and_is_one_undoable_paste() {
    let source = fixture();
    let clip = html(&source);
    let mut s = Session::new();
    let before = s.doc().unwrap().clone();
    let out = s.execute(CMD, &json!({"html": clip, "x": 100, "y": 120})).unwrap();
    assert!(out["warnings"].as_array().unwrap().is_empty(), "{out}");
    assert_eq!(out["ids"].as_array().unwrap().len(), 1);
    let root = NodeId(out["ids"][0].as_u64().unwrap());
    let doc = s.doc().unwrap();
    assert_eq!(doc.pages.len(), before.pages.len(), "paste must not append a page");
    let b = doc.world_bounds(root).unwrap();
    assert_eq!((b.x0, b.y0), (100.0, 120.0));
    assert_eq!(doc.subtree(root).len(), 3);
    let text = doc.find_by_name("Editable text", None)[0];
    assert!(matches!(&doc.get(text).unwrap().kind, NodeKind::Text(t) if t.characters == "Retype me"));
    let after = doc.clone();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.doc().unwrap(), &before);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(s.doc().unwrap(), &after);
    s.execute("node.set", &json!({"id": text, "props":{"characters":"Edited after paste"}})).unwrap();
    assert!(matches!(&s.doc().unwrap().get(text).unwrap().kind, NodeKind::Text(t) if t.characters == "Edited after paste"));
    let saved = figcraft_format::save_bytes(s.doc().unwrap(), None).unwrap();
    let (reopened, _) = figcraft_format::load_bytes(&saved).unwrap();
    assert!(matches!(&reopened.get(text).unwrap().kind, NodeKind::Text(t) if t.characters == "Edited after paste"));
}

#[test]
fn malformed_clipboard_does_not_paste_old_internal_layers_or_change_document() {
    let mut s = Session::new();
    s.execute("shape.rectangle", &json!({"width": 40,"height":30})).unwrap();
    s.execute("edit.copy", &json!({})).unwrap();
    let before = s.doc().unwrap().clone();
    let old_clip = s.clipboard.nodes.clone();
    assert!(s.execute(CMD, &json!({"html":"<!--(figma)invalid(/figma)-->"})).is_err());
    assert_eq!(s.doc().unwrap(), &before);
    assert_eq!(s.clipboard.nodes, old_clip);
    // A failure after resources start merging must also roll back.
    assert!(s.execute(CMD, &json!({"html":html(&fixture()),"parent":999999})).is_err());
    assert_eq!(s.doc().unwrap(), &before);
}

#[test]
fn resource_ids_and_links_between_roots_survive_collisions_and_repeated_pastes() {
    let mut src = fixture();
    let root = src.get(src.first_page().unwrap()).unwrap().children[0];
    src.styles.push(Style {
        id: "same-id".into(),
        name: "Source red".into(),
        description: String::new(),
        kind: StyleKind::Paint { paints: vec![figcraft_color::Paint::solid(figcraft_color::Color::from_hex("FF0000").unwrap())] },
        bound: BTreeMap::new(),
    });
    src.get_mut(root).unwrap().styles.fill = Some("same-id".into());
    src.get_mut(root).unwrap().fills = match &src.styles[0].kind {
        StyleKind::Paint { paints } => paints.clone(),
        _ => vec![],
    };
    let other = src.create(src.first_page().unwrap(), NodeKind::Rectangle, "Other root", 300.0, 0.0, 50.0, 50.0).unwrap();
    src.get_mut(root).unwrap().interactions = vec![proto::Interaction {
        trigger: Default::default(),
        actions: vec![proto::Action::SwapOverlay { dest: other }],
        transition: Default::default(),
        duration_ms: 0,
        easing: Default::default(),
        delay_ms: 0,
    }];
    let mut dest = Document::new("Destination");
    let mut collision = src.styles[0].clone();
    collision.name = "Do not replace".into();
    dest.styles.push(collision.clone());
    let page = dest.first_page().unwrap();
    for _ in 0..2 {
        let ids = insert(&mut dest, &mut src.clone(), &[root, other], page, Affine::IDENTITY, &mut vec![], &mut BTreeSet::new()).unwrap();
        let n = dest.get(ids[0]).unwrap();
        assert_ne!(n.styles.fill.as_deref(), Some("same-id"));
        assert_eq!(n.interactions[0].actions[0].dest(), Some(ids[1]));
        assert_eq!(dest.styles[0], collision);
    }
    assert_eq!(dest.styles.len(), 3);
}

#[test]
fn variable_aliases_modes_and_component_sources_are_remapped() {
    let mut src = fixture();
    let root = src.get(src.first_page().unwrap()).unwrap().children[0];
    src.variables=serde_json::from_value(json!({"collections":[{"id":"theme","name":"Theme","modes":[{"id":"light","name":"Light"}]}],
        "variables":[{"id":"red","name":"Red","collection":"theme","resolvedType":"color","values":{"light":{"type":"color","value":{"r":1,"g":0,"b":0,"a":1}}},"description":""},
        {"id":"accent","name":"Accent","collection":"theme","resolvedType":"color","values":{"light":{"type":"alias","value":"red"}},"description":""}]})).unwrap();
    src.get_mut(root).unwrap().bound.insert("fills/0/color".into(), "accent".into());
    src.get_mut(root).unwrap().var_modes.insert("theme".into(), "light".into());
    let main = src
        .create(
            src.first_page().unwrap(),
            NodeKind::Component { description: String::new(), props: BTreeMap::new() },
            "Main",
            300.0,
            0.0,
            100.0,
            50.0,
        )
        .unwrap();
    let main_child = src.create(main, NodeKind::Rectangle, "Main child", 0.0, 0.0, 10.0, 10.0).unwrap();
    let instance = src
        .create(src.first_page().unwrap(), NodeKind::Instance { component: main, props: BTreeMap::new() }, "Instance", 400.0, 0.0, 100.0, 50.0)
        .unwrap();
    let instance_child = src.create(instance, NodeKind::Rectangle, "Instance child", 0.0, 0.0, 10.0, 10.0).unwrap();
    src.get_mut(instance_child).unwrap().source = Some(main_child);
    let mut dest = Document::new("Destination");
    dest.variables = src.variables.clone();
    let page = dest.first_page().unwrap();
    let ids = insert(&mut dest, &mut src, &[root, main, instance], page, Affine::IDENTITY, &mut vec![], &mut BTreeSet::new()).unwrap();
    let root = dest.get(ids[0]).unwrap();
    let bound = root.bound.get("fills/0/color").unwrap();
    assert_ne!(bound, "accent");
    assert!(matches!(dest.variables.resolve(bound,&root.var_modes),Some(VariableValue::Color(c)) if c.r==1.0));
    assert!(matches!(dest.get(ids[2]).unwrap().kind,NodeKind::Instance {component,..} if component==ids[1]));
    let from = dest.get(ids[1]).unwrap().children[0];
    let to = dest.get(ids[2]).unwrap().children[0];
    assert_eq!(dest.get(to).unwrap().source, Some(from));
}

#[test]
fn clipboard_image_bytes_use_the_file_resolver_and_are_saved_with_the_design() {
    let mut source = fixture();
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 2, image::Rgba([230, 40, 80, 255])).write_to(&mut encoded, image::ImageFormat::Png).unwrap();
    let png = encoded.into_inner();
    let (hash, _, _) = figcraft_format::add_image(&mut source, png.clone()).unwrap();
    let rect = source.find_by_name("Editable rectangle", None)[0];
    source.get_mut(rect).unwrap().fills = vec![figcraft_color::Paint::image(hash, figcraft_color::ImageScaleMode::Fill)];
    let html = html(&source);
    let contents = figcraft_format::fig::clipboard::decode(&html).unwrap();
    let (missing, warnings) = figcraft_format::fig::import(&contents.canvas, "Clipboard").unwrap();
    assert!(!warnings.is_empty());
    let image_key = missing
        .nodes()
        .flat_map(|n| &n.fills)
        .find_map(|p| match &p.kind {
            figcraft_color::PaintKind::Image(i) => Some(i.image.clone()),
            _ => None,
        })
        .unwrap();
    let mut s = Session::new();
    let out = s.execute(CMD, &json!({"html":html,"images":{image_key:base64::engine::general_purpose::STANDARD.encode(&png)}})).unwrap();
    assert!(out["warnings"].as_array().unwrap().is_empty(), "{out}");
    let doc = s.doc().unwrap();
    let r = doc.get(doc.find_by_name("Editable rectangle", None)[0]).unwrap();
    let figcraft_color::PaintKind::Image(image) = &r.fills[0].kind else { panic!("image fill") };
    assert_eq!(doc.images.get(&image.image).unwrap().bytes.as_ref(), &png);
    let (saved, _) = figcraft_format::load_bytes(&figcraft_format::save_bytes(doc, None).unwrap()).unwrap();
    assert_eq!(saved.images.get(&image.image).unwrap().bytes.as_ref(), &png);
}

/// A copy whose rectangle paints a 2×2 PNG: (clipboard HTML, the image's key on the clipboard,
/// the PNG, the whole `.fig` file the copy came from).
fn image_copy() -> (String, String, Vec<u8>, Vec<u8>) {
    let mut source = fixture();
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 2, image::Rgba([30, 140, 80, 255])).write_to(&mut encoded, image::ImageFormat::Png).unwrap();
    let png = encoded.into_inner();
    let (hash, _, _) = figcraft_format::add_image(&mut source, png.clone()).unwrap();
    let rect = source.find_by_name("Editable rectangle", None)[0];
    source.get_mut(rect).unwrap().fills = vec![figcraft_color::Paint::image(hash, figcraft_color::ImageScaleMode::Fill)];
    let html = html(&source);
    let contents = figcraft_format::fig::clipboard::decode(&html).unwrap();
    let (imported, _) = figcraft_format::fig::import(&contents.canvas, "Clipboard").unwrap();
    let key = imported
        .nodes()
        .flat_map(|n| &n.fills)
        .find_map(|p| match &p.kind {
            figcraft_color::PaintKind::Image(i) => Some(i.image.clone()),
            _ => None,
        })
        .unwrap();
    let (fig, _) = figcraft_format::fig::export(&source, None).unwrap();
    (html, key, png, fig)
}

fn rect_image(s: &Session) -> String {
    let doc = s.doc().unwrap();
    let r = doc.get(doc.find_by_name("Editable rectangle", None)[0]).unwrap();
    let figcraft_color::PaintKind::Image(image) = &r.fills[0].kind else { panic!("image fill") };
    image.image.clone()
}

#[test]
fn images_that_did_not_load_keep_their_hash_are_reported_and_relink_from_the_fig_file() {
    let (html, key, png, fig) = image_copy();
    let mut s = Session::new();
    let out = s.execute(CMD, &json!({"html": html, "imageFailures": {key.clone(): "timed out"}})).unwrap();
    // The headline says how much came across and why, then how to get the rest.
    let first = out["warnings"][0].as_str().unwrap();
    assert!(first.starts_with("1 of 1 image(s) couldn't be loaded (1 timed out)"), "{out}");
    assert!(first.contains("Relink missing images"), "{first}");
    assert_eq!(out["warnings"].as_array().unwrap().len(), 1, "the importer's own image note isn't repeated: {out}");
    assert_eq!(out["missingImages"], json!([{"hash": key, "reason": "timed out"}]));
    assert_eq!(out["summary"]["layers"], 3);
    assert_eq!(out["summary"]["images"], 0);
    // The paint keeps the copy's own hash, on a recognisable placeholder (not a plain grey).
    assert_eq!(rect_image(&s), key);
    assert!(figcraft_format::rest::is_missing_image(&s.doc().unwrap().images[&key].bytes));

    let before = s.doc().unwrap().clone();
    let r = s.execute("image.relinkMissing", &json!({"data": base64::engine::general_purpose::STANDARD.encode(&fig)})).unwrap();
    assert_eq!(r, json!({"relinked": 1, "missing": 0}));
    let doc = s.doc().unwrap();
    assert_eq!(doc.images[&rect_image(&s)].bytes.as_ref(), &png);
    assert!(!doc.images.values().any(|i| figcraft_format::rest::is_missing_image(&i.bytes)), "the placeholder goes");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.doc().unwrap(), &before);
}

#[test]
fn relink_from_a_folder_matches_files_by_name_or_by_content() {
    let dir = std::env::temp_dir().join(format!("figcraft-relink-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (html, key, png, _) = image_copy();
    for by_name in [false, true] {
        for f in std::fs::read_dir(&dir).unwrap() {
            std::fs::remove_file(f.unwrap().path()).unwrap();
        }
        std::fs::write(dir.join("unrelated.png"), b"not an image").unwrap();
        std::fs::write(dir.join(if by_name { format!("{key}.png") } else { "Exported photo.png".into() }), &png).unwrap();
        let mut s = Session::new();
        s.execute(CMD, &json!({"html": html})).unwrap();
        let r = s.execute("image.relinkMissing", &json!({"path": dir.to_string_lossy()})).unwrap();
        assert_eq!(r, json!({"relinked": 1, "missing": 0}), "by name: {by_name}");
        assert_eq!(s.doc().unwrap().images[&rect_image(&s)].bytes.as_ref(), &png);
        // Nothing left to relink is not an error.
        assert_eq!(s.execute("image.relinkMissing", &json!({"path": dir.to_string_lossy()})).unwrap(), json!({"relinked": 0, "missing": 0}));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn relink_refuses_junk_without_touching_the_document() {
    let (html, ..) = image_copy();
    let mut s = Session::new();
    s.execute(CMD, &json!({"html": html})).unwrap();
    let before = s.doc().unwrap().clone();
    for p in [json!({}), json!({"data": "@@"}), json!({"data": "AAAA"}), json!({"path": "/nonexistent/figcraft/relink.fig"})] {
        assert!(s.execute("image.relinkMissing", &p).is_err(), "{p}");
        assert_eq!(s.doc().unwrap(), &before);
    }
}
