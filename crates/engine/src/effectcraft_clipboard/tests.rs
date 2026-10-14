use super::*;

fn add(doc: &mut Document, parent: NodeId, kind: NodeKind, name: &str) -> NodeId {
    let id = doc.alloc_id();
    let mut n = Node::new(id, kind, name, 100.0, 40.0);
    n.fills = vec![Paint::solid(Color::new(0.2, 0.4, 0.6, 0.8))];
    doc.insert(n, parent, None).unwrap();
    id
}

#[test]
fn export_keeps_resolved_affines_paint_order_and_unicode_styles() {
    let mut doc = Document::new("Synthetic clipboard test");
    let page = doc.first_page().unwrap();
    let frame = add(&mut doc, page, NodeKind::Frame, "Frame");
    let a = add(&mut doc, frame, NodeKind::Rectangle, "Back");
    let mut text = TextData { characters: "A😀B".into(), ..Default::default() };
    text.runs.push(StyleRun { start: 1, end: 2, style: TextStyle { font_size: 31.0, ..Default::default() } });
    let b = add(&mut doc, frame, NodeKind::Text(text), "Text");
    doc.get_mut(frame).unwrap().transform = Affine::translate((12.34567, 8.76543)) * Affine::rotate(0.3);
    doc.get_mut(a).unwrap().transform = Affine::translate((20.12345, 4.56789));
    let before = doc.clone();
    let payload = export(&doc, &[b, frame, frame, a]).unwrap();
    assert_eq!(payload["format"], "figcraft.clipboard");
    assert_eq!(payload["nodes"].as_array().unwrap().len(), 1);
    let root = &payload["nodes"][0];
    assert_eq!(root["children"][0]["name"], "Back");
    assert_eq!(root["children"][0]["absoluteTransform"], json!(matrix(doc.world_transform(a))));
    let text = &root["children"][1];
    assert_eq!(text["characters"], "A😀B");
    let ids = text["characterStyleOverrides"].as_array().unwrap();
    assert_eq!(ids.len(), 4);
    assert_eq!(ids[1], ids[2]);
    let style_id = ids[1].as_u64().unwrap().to_string();
    assert_eq!(text["styleOverrideTable"][style_id]["fontSize"], 31.0);
    assert_eq!(doc, before);
}

#[test]
fn command_returns_snapshot_without_changing_regular_copy_or_undo() {
    let mut s = Session::new();
    let created = s.execute("shape.rectangle", &json!({"x": 10, "y": 20, "width": 80, "height": 30})).unwrap();
    let undo = s.undo_depth();
    let before = s.doc().unwrap().clone();
    let payload = s.execute(CMD, &json!({"ids": [created["id"]]})).unwrap();
    assert_eq!(payload["nodes"][0]["type"], "RECTANGLE");
    assert_eq!(s.undo_depth(), undo);
    assert_eq!(*s.doc().unwrap(), before);
    assert!(s.clipboard.nodes.is_empty());
    s.execute("edit.copy", &json!({})).unwrap();
    assert!(!s.clipboard.nodes.is_empty());
    assert!(s.execute(CMD, &json!({"ids": []})).is_err());
    assert!(s.execute(CMD, &json!({"ids": [999999]})).is_err());
}

#[test]
fn native_shapes_shadows_and_unsupported_paints_are_explicit() {
    let mut doc = Document::new("Synthetic");
    let page = doc.first_page().unwrap();
    let star = add(&mut doc, page, NodeKind::Star { count: 5, inner_ratio: 0.4 }, "Star");
    let n = doc.get_mut(star).unwrap();
    n.fills.push(Paint::image("not-a-file", figcraft_color::ImageScaleMode::Fill));
    n.effects.push(Effect { kind: EffectKind::DropShadow(Shadow::default()), visible: true });
    let v = export(&doc, &[star]).unwrap();
    assert_eq!(v["nodes"][0]["type"], "VECTOR");
    assert!(v["nodes"][0]["vectorPaths"][0]["data"].as_str().unwrap().starts_with('M'));
    assert_eq!(v["nodes"][0]["fills"].as_array().unwrap().len(), 1);
    assert_eq!(v["nodes"][0]["effects"][0]["type"], "DROP_SHADOW");
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("image/pattern")));
    assert!(!serde_json::to_string(&v).unwrap().contains("not-a-file"));
}

#[test]
fn refuses_oversized_deep_cyclic_and_nonfinite_sources() {
    let mut doc = Document::new("Synthetic");
    let page = doc.first_page().unwrap();
    let root = add(&mut doc, page, NodeKind::Frame, "Root");
    doc.get_mut(root).unwrap().name = "x".repeat(MAX_BYTES);
    assert!(export(&doc, &[root]).is_err());
    doc.get_mut(root).unwrap().name = "Root".into();
    for f in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        doc.get_mut(root).unwrap().width = f;
        assert!(export(&doc, &[root]).is_err());
    }
    doc.get_mut(root).unwrap().width = 100.0;
    doc.get_mut(root).unwrap().children.push(root);
    assert!(export(&doc, &[root]).is_err());
    doc.get_mut(root).unwrap().children.clear();
    let mut parent = root;
    for _ in 0..=MAX_DEPTH {
        parent = add(&mut doc, parent, NodeKind::Frame, "Nested");
    }
    assert!(export(&doc, &[root]).is_err());
}

#[test]
fn roots_and_auto_layout_children_keep_paint_order() {
    let mut doc = Document::new("Synthetic");
    let page = doc.first_page().unwrap();
    let frame = add(&mut doc, page, NodeKind::Frame, "Frame");
    let a = add(&mut doc, frame, NodeKind::Rectangle, "A");
    let b = add(&mut doc, frame, NodeKind::Rectangle, "B");
    doc.get_mut(frame).unwrap().layout = Some(AutoLayout { reverse_z: true, ..Default::default() });
    let v = export(&doc, &[frame]).unwrap();
    assert_eq!(v["nodes"][0]["children"][0]["id"], b.to_string());
    assert_eq!(v["nodes"][0]["children"][1]["id"], a.to_string());
    let v = export(&doc, &[a, b]).unwrap();
    assert_eq!(v["nodes"][0]["id"], b.to_string());
    assert_eq!(v["nodes"][1]["id"], a.to_string());
}

#[test]
fn render_bounds_include_shadow_overflow_for_unclipped_frames() {
    let mut doc = Document::new("Synthetic");
    let page = doc.first_page().unwrap();
    let frame = add(&mut doc, page, NodeKind::Frame, "Frame");
    doc.get_mut(frame).unwrap().clip = false;
    let shape = add(&mut doc, frame, NodeKind::Rectangle, "Shadow");
    let n = doc.get_mut(shape).unwrap();
    n.transform = Affine::translate((90.0, 0.0));
    n.effects.push(Effect { kind: EffectKind::DropShadow(Shadow { offset: (20.0, 0.0), radius: 10.0, ..Default::default() }), visible: true });
    let v = export(&doc, &[frame]).unwrap();
    let child = &v["nodes"][0]["children"][0];
    assert!(child["absoluteRenderBounds"]["width"].as_f64().unwrap() > child["absoluteBoundingBox"]["width"].as_f64().unwrap());
    assert!(v["nodes"][0]["absoluteRenderBounds"]["width"].as_f64().unwrap() > 200.0);
}

#[test]
fn junk_parameters_never_become_internal_errors() {
    for params in [Value::Null, json!([]), json!({"ids": [null, -1, 1e308, {}, "bad"]}), json!({"ids": "bad"}), json!({"id": [1]})] {
        let mut session = Session::new();
        session.execute("shape.rectangle", &json!({"width": 20, "height": 10})).unwrap();
        let before = session.doc().unwrap().clone();
        let result = session.execute(CMD, &params);
        assert!(!matches!(result, Err(crate::EngineError::Internal { .. })));
        assert_eq!(*session.doc().unwrap(), before);
    }
}
