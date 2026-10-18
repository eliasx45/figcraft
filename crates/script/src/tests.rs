use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::{Host, Options, Outcome, run};

/// A page (1) with layers; commands edit them like the engine would.
#[derive(Default)]
struct Fake {
    nodes: BTreeMap<u64, Value>,
    next: u64,
    selection: Vec<u64>,
    calls: Vec<(String, Value)>,
}

impl Fake {
    fn new() -> Self {
        let mut f = Fake { next: 10, ..Fake::default() };
        f.nodes.insert(1, json!({"id": 1, "name": "Page 1", "type": "page", "children": [2, 3]}));
        f.nodes.insert(2, json!({"id": 2, "name": "Card", "type": "frame", "parent": 1, "children": [4, 5], "x": 0, "width": 200}));
        f.nodes.insert(3, json!({"id": 3, "name": "Logo", "type": "rectangle", "parent": 1, "x": 300, "width": 50}));
        f.nodes.insert(4, json!({"id": 4, "name": "Title", "type": "text", "parent": 2, "characters": "Hello", "fontSize": 12}));
        f.nodes.insert(5, json!({"id": 5, "name": "Body", "type": "text", "parent": 2, "characters": "World", "fontSize": 18}));
        f
    }

    fn kids(&self, id: u64) -> Vec<u64> {
        self.nodes
            .get(&id)
            .and_then(|n| n.get("children"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default()
    }

    fn detach(&mut self, id: u64) {
        let parent = self.nodes.get(&id).and_then(|n| n.get("parent")).and_then(Value::as_u64);
        if let Some(p) = parent.and_then(|p| self.nodes.get_mut(&p)) {
            let kids: Vec<u64> =
                p["children"].as_array().map(|a| a.iter().filter_map(Value::as_u64).filter(|k| *k != id).collect()).unwrap_or_default();
            p["children"] = json!(kids);
        }
    }

    fn attach(&mut self, id: u64, parent: u64, index: Option<usize>) {
        let mut kids = self.kids(parent);
        let at = index.unwrap_or(kids.len()).min(kids.len());
        kids.insert(at, id);
        if let Some(p) = self.nodes.get_mut(&parent) {
            p["children"] = json!(kids);
        }
        if let Some(n) = self.nodes.get_mut(&id) {
            n["parent"] = json!(parent);
        }
    }
}

impl Host for Fake {
    fn call(&mut self, command: &str, p: &Value) -> Result<Value, String> {
        self.calls.push((command.to_string(), p.clone()));
        let ids: Vec<u64> = p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
        match command {
            "node.set" => {
                let props = p.get("props").and_then(Value::as_object).ok_or("props")?;
                for id in ids {
                    let n = self.nodes.get_mut(&id).ok_or(format!("no layer {id}"))?;
                    for (k, v) in props {
                        if k == "width" && !v.is_number() {
                            return Err("width must be a number".into());
                        }
                        n[k] = v.clone();
                    }
                }
                Ok(json!({}))
            }
            "node.create" => {
                let id = self.next;
                self.next += 1;
                let kind = p["type"].as_str().unwrap_or("rectangle");
                let mut n = json!({"id": id, "type": kind, "name": p.get("name").cloned().unwrap_or(json!(kind))});
                if let Some(t) = p.get("text") {
                    n["characters"] = t.clone();
                }
                for k in ["x", "y", "width", "height"] {
                    if let Some(v) = p.get(k) {
                        n[k] = v.clone();
                    }
                }
                self.nodes.insert(id, n);
                let parent = p.get("parent").and_then(Value::as_u64).unwrap_or(1);
                self.attach(id, parent, None);
                Ok(json!({"id": id}))
            }
            "edit.delete" => {
                for id in ids {
                    self.detach(id);
                    self.nodes.remove(&id);
                }
                Ok(json!({}))
            }
            "edit.duplicate" => {
                let id = *ids.first().ok_or("ids")?;
                let mut n = self.nodes.get(&id).cloned().ok_or("no layer")?;
                let copy = self.next;
                self.next += 1;
                n["id"] = json!(copy);
                n["children"] = json!([]);
                self.nodes.insert(copy, n);
                let parent = self.nodes[&id]["parent"].as_u64().unwrap_or(1);
                self.attach(copy, parent, None);
                Ok(json!({"ids": [copy]}))
            }
            "node.reparent" => {
                let parent = p["parent"].as_u64().ok_or("parent")?;
                let index = p.get("index").and_then(Value::as_f64).map(|i| i as usize);
                for id in ids {
                    self.detach(id);
                    self.attach(id, parent, index);
                }
                Ok(json!({}))
            }
            "select.set" => {
                self.selection = ids;
                Ok(json!({}))
            }
            "select.none" => {
                self.selection.clear();
                Ok(json!({}))
            }
            "echo" => Ok(p.clone()),
            other => Err(format!("unknown command `{other}`")),
        }
    }

    fn node(&mut self, id: u64) -> Option<Value> {
        self.nodes.get(&id).cloned()
    }

    fn descendants(&mut self, id: u64) -> Vec<u64> {
        let mut out = Vec::new();
        for k in self.kids(id) {
            out.push(k);
            out.extend(self.descendants(k));
        }
        out
    }

    fn context(&mut self) -> Value {
        json!({"page": 1, "pages": [1], "selection": self.selection})
    }
}

fn go_with(src: &str, opts: &Options) -> (Box<Fake>, Outcome) {
    let (host, out) = run(Box::new(Fake::new()), src, opts);
    let any: Box<dyn std::any::Any> = host;
    (any.downcast::<Fake>().expect("the host comes back"), out)
}

fn go(src: &str) -> (Box<Fake>, Outcome) {
    go_with(src, &Options::default())
}

#[test]
fn returns_values_and_logs() {
    let (_, out) = go("console.log('hi', {a: 1}, [2]); console.warn('w'); return {sum: 1 + 2, args};");
    assert!(out.ok(), "{:?}", out.error);
    assert_eq!(out.result, json!({"sum": 3, "args": null}));
    assert_eq!(out.logs, vec!["hi {\"a\":1} [2]".to_string(), "warning: w".to_string()]);
    let (_, out) = go("let x = 1;");
    assert_eq!(out.result, Value::Null);
}

#[test]
fn args_and_fc_run() {
    let opts = Options { args: json!({"n": 3}), ..Options::default() };
    let (host, out) = go_with("return fc.run('echo', {twice: args.n * 2});", &opts);
    assert_eq!(out.result, json!({"twice": 6}));
    assert_eq!(out.calls, 1);
    assert_eq!(host.calls.len(), 1);
}

#[test]
fn layer_model_reads_and_writes() {
    let src = r#"
        const page = fc.currentPage;
        const texts = page.findAll(n => n.type === 'text');
        for (const t of texts) if (t.fontSize < 14) t.fontSize = 16;
        const card = fc.getNodeById(2);
        return {
            names: texts.map(t => t.name),
            sizes: texts.map(t => t.fontSize),
            kids: card.children.map(c => c.id),
            parent: card.children[0].parent === card,
            same: fc.getNodeById(4) === texts[0],
            page: page.type,
            logo: fc.findOne(n => n.name === 'Logo').x,
            missing: fc.getNodeById(999),
            has: 'characters' in texts[0],
            str: String(card),
        };
    "#;
    let (host, out) = go(src);
    assert!(out.ok(), "{:?}", out.error);
    assert_eq!(
        out.result,
        json!({"names": ["Title", "Body"], "sizes": [16, 18], "kids": [4, 5], "parent": true, "same": true, "page": "page", "logo": 300, "missing": null, "has": true, "str": "[frame 2 \"Card\"]"})
    );
    assert_eq!(host.nodes[&4]["fontSize"], json!(16));
    // Only the one small text was written.
    assert_eq!(host.calls.iter().filter(|(c, _)| c == "node.set").count(), 1);
}

#[test]
fn create_clone_reparent_remove_select() {
    let src = r#"
        const card = fc.getNodeById(2);
        const r = fc.createRectangle({x: 5, width: 40, parent: card, name: 'Chip'});
        const t = fc.createText('Label');
        card.insertChild(0, t);
        const copy = fc.getNodeById(3).clone();
        fc.getNodeById(5).remove();
        fc.selection = [r, copy];
        const sel = fc.selection.map(n => n.id);
        r.set({name: 'Chip 2', width: 41});
        return {chip: r.toJSON(), text: t.characters, kids: card.children.map(n => n.name), sel, copy: copy.name, removed: fc.getNodeById(5)};
    "#;
    let (host, out) = go(src);
    assert!(out.ok(), "{:?}", out.error);
    let r = &out.result;
    assert_eq!(r["chip"]["name"], json!("Chip 2"));
    assert_eq!(r["chip"]["width"], json!(41));
    assert_eq!(r["text"], json!("Label"));
    assert_eq!(r["kids"], json!(["text", "Title", "Chip 2"]));
    assert_eq!(r["copy"], json!("Logo"));
    assert_eq!(r["removed"], Value::Null);
    assert_eq!(host.selection.len(), 2);
    // New layers don't take the selection on their own.
    let create = host.calls.iter().find(|(c, _)| c == "node.create").map(|(_, p)| p.clone()).unwrap_or_default();
    assert_eq!(create["select"], json!(false));
    assert_eq!(create["parent"], json!(2));
}

#[test]
fn errors_carry_their_line() {
    let (_, out) = go("let a = 1;\nlet b = 2;\nthrow new Error('boom');");
    let e = out.error.expect("an error");
    assert_eq!(e.message, "boom");
    assert_eq!(e.line, Some(3));

    let (_, out) = go("let a = 1;\nnotDefined + 1;");
    let e = out.error.expect("an error");
    assert!(e.message.contains("notDefined"), "{e:?}");
    assert_eq!(e.line, Some(2));

    let (_, out) = go("let a = ;");
    let e = out.error.expect("a syntax error");
    assert_eq!(e.line, Some(1), "{e:?}");

    // A command's refusal is a catchable error with the command's reason.
    let (_, out) = go("fc.getNodeById(2).width = 'wide';");
    let e = out.error.expect("an error");
    assert!(e.message.contains("width must be a number"), "{e:?}");
    let (_, out) = go("try { fc.run('nope') } catch (e) { return e.message }");
    assert_eq!(out.result, json!("nope: unknown command `nope`"));
}

#[test]
fn read_only_properties_and_bad_arguments_throw() {
    for src in [
        "fc.getNodeById(2).id = 5",
        "fc.getNodeById(2).children = []",
        "fc.getNodeById(2).findAll = 1",
        "delete fc.getNodeById(2).name",
        "fc.getNodeById(2).findAll(5)",
        "fc.run(5)",
        "fc.run('echo', 5)",
        "fc.getNodeById(2).appendChild('x')",
        "fc.getNodeById(2).insertChild(-1, 3)",
        "fc.selection = 5",
        "fc.create(1)",
        "fc.createFrame(5)",
        "fc.getNodeById(2).set(5)",
    ] {
        let (_, out) = go(src);
        assert!(!out.ok(), "{src} should fail");
    }
}

#[test]
fn runaway_scripts_stop() {
    // Out of time: API calls fail once the budget is spent, also inside `catch`.
    let opts = Options { timeout: 0.05, ..Options::default() };
    let (_, out) = go_with("while (true) { try { fc.get(2) } catch (e) {} }", &opts);
    assert!(out.error.expect("stopped").message.contains("out of time"));
    // Deep recursion ends with an error, not a crashed process.
    let (_, out) = go("function f(n) { return f(n + 1) } f(0);");
    assert!(!out.ok());
    // The output is capped.
    let (_, out) = go("for (let i = 0; i < 5000; i++) console.log('line ' + i);");
    assert!(out.ok());
    assert!(out.logs.len() <= 1001);
    assert!(out.logs.last().is_some_and(|l| l.contains("output limit")));
}

#[test]
fn scripts_cannot_run_scripts() {
    struct Nested;
    impl Host for Nested {
        fn call(&mut self, _: &str, _: &Value) -> Result<Value, String> {
            let (_, out) = run(Box::new(Fake::new()), "return 1", &Options::default());
            Err(out.error.map(|e| e.message).unwrap_or_default())
        }
        fn node(&mut self, _: u64) -> Option<Value> {
            None
        }
        fn descendants(&mut self, _: u64) -> Vec<u64> {
            Vec::new()
        }
        fn context(&mut self) -> Value {
            Value::Null
        }
    }
    let (_, out) = run(Box::new(Nested), "fc.run('x')", &Options::default());
    assert!(out.error.expect("refused").message.contains("already running"));
    // And a run after that works again.
    let (_, out) = go("return 2");
    assert_eq!(out.result, json!(2));
}

#[test]
fn results_that_are_layers_come_out_as_properties() {
    let (_, out) = go("return fc.getNodeById(3)");
    assert_eq!(out.result["name"], json!("Logo"));
    let (_, out) = go("return fc.currentPage.children");
    assert_eq!(out.result.as_array().map(Vec::len), Some(2));
    let (_, out) = go("return () => 1");
    assert_eq!(out.result, Value::Null);
}

#[test]
fn junk_scripts_never_panic() {
    for src in [
        "",
        "}",
        "return",
        "\u{0}",
        "fc = null; return 1",
        "return fc.getNodeById('abc')",
        "return fc.get(-1)",
        "args.x.y",
        "return new Proxy({}, {})",
    ] {
        let _ = go(src);
    }
    let big = "x".repeat(crate::runtime::MAX_SOURCE_BYTES + 1);
    let (_, out) = go(&big);
    assert!(!out.ok());
}
