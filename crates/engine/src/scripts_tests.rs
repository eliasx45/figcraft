//! Scripts: a run is one undo step that redo brings back, a failure rolls everything back and can
//! be fixed with edits, scripts can't open dialogs or run scripts, and the layer model reads and
//! writes the real document. The tasks below are the kind agents run: rename by rule, colour
//! audit, variants from data, a grid layout, a report.

use serde_json::{Value, json};

use crate::Session;
use crate::components::tests::{ex, idv, node};
use figcraft_doc::NodeId;

fn script(s: &mut Session, source: &str) -> Value {
    ex(s, "script.run", json!({"source": source}))
}

fn ok(out: &Value) {
    assert_eq!(out["ok"], json!(true), "script failed: {out}");
}

fn rect(s: &mut Session, name: &str, x: f64, fill: &str) -> NodeId {
    idv(&ex(s, "node.create", json!({"type": "rectangle", "x": x, "y": 0, "width": 40, "height": 40, "name": name, "props": {"fills": [fill]}})))
}

fn undo_labels(s: &Session) -> Vec<String> {
    s.ds().map(|ds| ds.history.undo.iter().map(|e| e.label.clone()).collect()).unwrap_or_default()
}

#[test]
fn a_run_is_one_undo_step_and_redo_brings_it_back() {
    let mut s = Session::new();
    let steps = undo_labels(&s).len();
    let out = ex(
        &mut s,
        "script.run",
        json!({"label": "grid", "source": "for (let i = 0; i < 12; i++) fc.createRectangle({x: i * 50, y: 0, width: 40, height: 40, name: 'Cell ' + i}); return fc.currentPage.children.length"}),
    );
    ok(&out);
    assert_eq!(out["result"], json!(12));
    assert_eq!(out["calls"], json!(12));
    assert_eq!(out["changes"]["counts"]["created"], json!(12));
    assert_eq!(out["changes"]["createdTop"].as_array().map(Vec::len), Some(12));
    let labels = undo_labels(&s);
    assert_eq!(labels.len(), steps + 1, "{labels:?}");
    assert_eq!(labels.last().map(String::as_str), Some("Script: grid"));

    ex(&mut s, "edit.undo", json!({}));
    let page = s.ds().unwrap().page;
    assert!(node(&s, page).children.is_empty());
    // Undone by mistake: redo brings the whole run back.
    ex(&mut s, "edit.redo", json!({}));
    assert_eq!(node(&s, page).children.len(), 12);
}

#[test]
fn a_failed_run_rolls_back_and_can_be_fixed_with_edits() {
    let mut s = Session::new();
    let a = rect(&mut s, "A", 0.0, "#FF0000");
    let steps = undo_labels(&s).len();
    let src = format!("fc.getNodeById({}).name = 'Renamed';\nfc.createEllipse({{width: 10, height: 10}});\nthrow new Error('stop here');", a.0);
    let out = script(&mut s, &src);
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["rolledBack"], json!(true));
    assert_eq!(out["error"]["message"], json!("stop here"));
    assert_eq!(out["error"]["line"], json!(3));
    // Nothing it did is left, and no undo step was added.
    assert_eq!(node(&s, a).name, "A");
    assert_eq!(node(&s, s.ds().unwrap().page).children.len(), 1);
    assert_eq!(undo_labels(&s).len(), steps);

    // The fix goes as edits to the failed script.
    let id = out["editId"].as_str().unwrap().to_string();
    let fixed = ex(&mut s, "script.run", json!({"editId": id, "edits": [{"find": "throw new Error('stop here');", "replace": "return 'done'"}]}));
    ok(&fixed);
    assert_eq!(fixed["result"], json!("done"));
    assert_eq!(node(&s, a).name, "Renamed");

    // Bad edits are refused with a reason.
    let out = script(&mut s, "let x = 1; let y = 1; null.boom;");
    let id = out["editId"].as_str().unwrap().to_string();
    for (edits, why) in [
        (json!([{"find": "nope", "replace": ""}]), "doesn't occur"),
        (json!([{"find": "= 1", "replace": "= 2"}]), "occurs 2 times"),
        (json!([]), "empty"),
        (json!("x"), "array"),
    ] {
        let e = s.execute("script.run", &json!({"editId": id, "edits": edits})).unwrap_err().to_string();
        assert!(e.contains(why), "{e}");
    }
    let e = s.execute("script.run", &json!({"editId": "s999", "edits": [{"find": "a", "replace": "b"}]})).unwrap_err().to_string();
    assert!(e.contains("no failed script"), "{e}");
}

#[test]
fn scripts_cannot_open_dialogs_or_run_scripts() {
    let mut s = Session::new();
    let out = script(&mut s, "fc.run('plugins.manage', {})");
    assert_eq!(out["ok"], json!(false));
    assert!(out["error"]["message"].as_str().unwrap().contains("dialog"), "{out}");
    assert!(s.ui_requests.is_empty());
    let out = script(&mut s, "fc.run('script.run', {source: 'return 1'})");
    assert!(out["error"]["message"].as_str().unwrap().contains("scripts can't run scripts"), "{out}");
    // Junk parameters are refused, not crashes.
    for p in [json!({}), json!({"source": ""}), json!({"source": 5}), json!({"source": "1", "timeout": -1}), json!({"source": "1", "editId": "s1"})] {
        assert!(s.execute("script.run", &p).is_err(), "{p}");
    }
}

#[test]
fn the_layer_model_reads_and_writes_the_document() {
    let mut s = Session::new();
    let frame = idv(&ex(&mut s, "node.create", json!({"type": "frame", "x": 0, "y": 0, "width": 300, "height": 200, "name": "Card"})));
    let t = idv(&ex(&mut s, "node.create", json!({"type": "text", "x": 10, "y": 10, "text": "Hello", "parent": frame.0})));
    let out = script(
        &mut s,
        &format!(
            r#"
            const card = fc.getNodeById({f});
            const t = card.findOne(n => n.type === 'TEXT');
            t.characters = 'Hello, world';
            card.fills = ['#336699'];
            card.cornerRadius = 12;
            const r = fc.createRectangle({{parent: card, x: 20, y: 60, width: 50, height: 20, name: 'Chip'}});
            fc.selection = [r];
            return {{text: t.characters, kids: card.children.map(c => c.name), parent: r.parent === card, page: fc.currentPage.type,
                    fill: card.fills[0].color, radius: card.cornerRadius, sel: fc.selection.map(n => n.id), id: t.id}};
            "#,
            f = frame.0
        ),
    );
    ok(&out);
    let r = &out["result"];
    assert_eq!(r["text"], json!("Hello, world"));
    assert_eq!(r["kids"].as_array().map(Vec::len), Some(2));
    assert_eq!(r["parent"], json!(true));
    assert_eq!(r["page"], json!("PAGE"));
    assert_eq!(r["fill"].as_str().map(str::to_ascii_lowercase), Some("#336699".into()));
    assert_eq!(r["radius"].as_f64(), Some(12.0));
    assert_eq!(r["id"], json!(t.0));
    assert_eq!(s.selection().len(), 1);
    assert_eq!(out["changes"]["counts"]["created"], json!(1));
}

/// Task 1: rename by rule (text layers named after their content, frames numbered).
#[test]
fn task_rename_by_rule() {
    let mut s = Session::new();
    for i in 0..5 {
        let f = idv(&ex(&mut s, "node.create", json!({"type": "frame", "x": i * 120, "y": 0, "width": 100, "height": 100})));
        ex(&mut s, "node.create", json!({"type": "text", "x": 5, "y": 5, "text": format!("Label {i}"), "parent": f.0}));
    }
    let out = script(
        &mut s,
        r#"
        let n = 0;
        for (const f of fc.currentPage.children) {
            if (f.type !== 'FRAME') continue;
            f.name = 'Card/' + String(++n).padStart(2, '0');
            for (const t of f.findAll(x => x.type === 'TEXT')) t.name = t.characters.toLowerCase().replace(/\s+/g, '-');
        }
        return fc.findAll().map(x => x.name);
        "#,
    );
    ok(&out);
    let names: Vec<&str> = out["result"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(names.contains(&"Card/01") && names.contains(&"Card/05") && names.contains(&"label-4"), "{names:?}");
}

/// Task 2: colour audit and fix (fills off the palette move to its nearest colour).
#[test]
fn task_colour_audit_and_fix() {
    let mut s = Session::new();
    let ok_one = rect(&mut s, "ok", 0.0, "#FF0000");
    let off = rect(&mut s, "off", 50.0, "#EE1111");
    let blue = rect(&mut s, "blue", 100.0, "#0000FE");
    let out = ex(
        &mut s,
        "script.run",
        json!({"args": {"palette": ["#ff0000", "#0000ff"]}, "source": r#"
            const hex = c => [1, 3, 5].map(i => parseInt(c.slice(i, i + 2), 16));
            const dist = (a, b) => hex(a).reduce((d, v, i) => d + (v - hex(b)[i]) ** 2, 0);
            const report = [];
            for (const n of fc.findAll(n => (n.fills || []).some(p => p.type === 'solid'))) {
                const c = n.fills[0].color.toLowerCase();
                if (args.palette.includes(c)) continue;
                const near = args.palette.reduce((a, b) => dist(c, a) <= dist(c, b) ? a : b);
                report.push({id: n.id, name: n.name, from: c, to: near});
                n.fills = [near];
            }
            return report;
        "#}),
    );
    ok(&out);
    assert_eq!(out["result"].as_array().map(Vec::len), Some(2), "{out}");
    let hexes = |s: &Session, id| format!("{:?}", node(s, id).fills);
    assert_eq!(hexes(&s, off), hexes(&s, ok_one));
    assert_ne!(hexes(&s, blue), hexes(&s, ok_one));
    assert_eq!(out["changes"]["counts"]["changed"], json!(2));
}

/// Task 3: variants from data (one card per row, built from a template).
#[test]
fn task_variants_from_data() {
    let mut s = Session::new();
    let rows: Vec<Value> = (0..50).map(|i| json!({"title": format!("Product {i}"), "price": i * 3})).collect();
    let out = ex(
        &mut s,
        "script.run",
        json!({"args": {"rows": rows}, "label": "variants", "source": r#"
            const made = args.rows.map((row, i) => {
                const card = fc.createFrame({x: (i % 10) * 220, y: Math.floor(i / 10) * 140, width: 200, height: 120, name: row.title});
                fc.createText({text: row.title, parent: card, x: 12, y: 12});
                fc.createText({text: '$' + row.price, parent: card, x: 12, y: 60});
                return card;
            });
            return made.length;
        "#}),
    );
    ok(&out);
    assert_eq!(out["result"], json!(50));
    assert_eq!(out["changes"]["counts"]["created"], json!(150));
    assert_eq!(out["changes"]["createdTop"].as_array().map(Vec::len), Some(50));
}

/// Task 4: grid layout (the selection laid out in columns with even gaps).
#[test]
fn task_grid_layout() {
    let mut s = Session::new();
    let ids: Vec<NodeId> = (0..9).map(|i| rect(&mut s, &format!("r{i}"), f64::from(i) * 7.0, "#CCCCCC")).collect();
    ex(&mut s, "select.set", json!({"ids": ids}));
    let out = script(
        &mut s,
        r#"
        const cols = 3, gap = 16;
        fc.selection.forEach((n, i) => { n.x = (i % cols) * (n.width + gap); n.y = Math.floor(i / cols) * (n.height + gap); });
        return fc.selection.map(n => [n.x, n.y]);
        "#,
    );
    ok(&out);
    let last = node(&s, ids[8]);
    let c = last.transform.as_coeffs();
    assert_eq!((c[4], c[5]), (112.0, 112.0));
}

/// Task 5: a report (counts by type, smallest text, hidden layers) without changing anything.
#[test]
fn task_report_changes_nothing() {
    let mut s = Session::new();
    rect(&mut s, "a", 0.0, "#000000");
    ex(&mut s, "node.create", json!({"type": "text", "x": 0, "y": 80, "text": "tiny", "props": {"fontSize": 8}}));
    let steps = undo_labels(&s).len();
    let out = script(
        &mut s,
        r#"
        const all = fc.findAll();
        const byType = {};
        for (const n of all) byType[n.type] = (byType[n.type] || 0) + 1;
        const texts = all.filter(n => n.type === 'TEXT');
        console.log('layers', all.length);
        return {byType, smallest: Math.min(...texts.map(t => t.fontSize)), hidden: all.filter(n => n.visible === false).length};
        "#,
    );
    ok(&out);
    assert_eq!(out["result"]["byType"], json!({"RECTANGLE": 1, "TEXT": 1}));
    assert_eq!(out["result"]["smallest"].as_f64(), Some(8.0));
    assert_eq!(out["logs"], json!(["layers 2"]));
    assert_eq!(out["changes"]["counts"], json!({"created": 0, "changed": 0, "removed": 0}));
    assert_eq!(undo_labels(&s).len(), steps, "a script that changes nothing adds no undo step");
}

/// Speed against `command.batch` on a large page (run with `--ignored --nocapture`).
#[test]
#[ignore]
fn bench_script_vs_batch() {
    let n = 5000;
    let mut s = Session::new();
    let t = web_time::Instant::now();
    let out = ex(
        &mut s,
        "script.run",
        json!({"timeout": 600, "args": {"n": n}, "source": "for (let i = 0; i < args.n; i++) fc.createRectangle({x: (i % 100) * 10, y: Math.floor(i / 100) * 10, width: 8, height: 8, name: 'r' + i}); return args.n"}),
    );
    ok(&out);
    let make = t.elapsed();
    let t = web_time::Instant::now();
    let out = ex(
        &mut s,
        "script.run",
        json!({"timeout": 600, "source": "let k = 0; for (const n of fc.findAll(n => n.name.endsWith('7'))) { n.opacity = 0.5; k++ } return k"}),
    );
    ok(&out);
    let edit = t.elapsed();
    let t = web_time::Instant::now();
    let out = ex(&mut s, "script.run", json!({"timeout": 600, "source": "return fc.findAll(n => n.width === 8).length"}));
    let read = t.elapsed();
    assert_eq!(out["result"], json!(n));

    // The same edit as one batch built outside (what an agent does without scripts).
    let mut s2 = Session::new();
    let steps: Vec<Value> = (0..n)
        .map(|i| json!({"command": "node.create", "params": {"type": "rectangle", "x": (i % 100) * 10, "y": (i / 100) * 10, "width": 8, "height": 8, "name": format!("r{i}"), "select": false}}))
        .collect();
    let t = web_time::Instant::now();
    ex(&mut s2, "command.batch", json!({"commands": steps}));
    let batch = t.elapsed();
    println!("{n} layers: script create {make:?}, script findAll+edit {edit:?}, script findAll read {read:?}, command.batch create {batch:?}");
}
