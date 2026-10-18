//! The script API: `fc`, `console`, `args` and layer objects. A layer is a proxy over its id: reading
//! a property reads the document (cached until the next write), writing one runs `node.set`. The
//! same layer always comes back as the same object, so `===` compares layers.

use boa_engine::object::builtins::{JsArray, JsProxyBuilder};
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string};
use serde_json::{Map, Value, json};

use crate::runtime::{self, throw};

/// Hidden globals: the layer objects made so far (by id) and their shared prototype.
const NODES: &str = "__fcNodes";
const PROTO: &str = "__fcNodeProto";

/// Layer properties that are read-only in scripts (the rest go to `node.set`).
const READ_ONLY: &[&str] = &["id", "type", "parent", "children", "removed", "absoluteBounds", "childCount"];

/// Layer types with a `fc.create<Type>()` shortcut (`node.create` types).
const CREATE: &[(&str, &str)] = &[
    ("createFrame", "frame"),
    ("createRectangle", "rectangle"),
    ("createEllipse", "ellipse"),
    ("createPolygon", "polygon"),
    ("createStar", "star"),
    ("createLine", "line"),
    ("createArrow", "arrow"),
    ("createVector", "vector"),
    ("createText", "text"),
    ("createSection", "section"),
    ("createComponent", "component"),
    ("createSlice", "slice"),
];

type Native = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn function(ctx: &mut Context, name: &str, length: usize, f: NativeFunction) -> boa_engine::object::builtins::JsFunction {
    FunctionObjectBuilder::new(ctx.realm(), f).name(JsString::from(name)).length(length).build()
}

/// Set up the globals of a fresh context.
pub(crate) fn install(ctx: &mut Context, args: &Value) -> JsResult<()> {
    let hidden = Attribute::empty();
    let registry = JsObject::with_null_proto();
    ctx.register_global_property(JsString::from(NODES), registry, hidden)?;

    let methods: [(&str, usize, Native); 10] = [
        ("findAll", 1, node_find_all),
        ("findOne", 1, node_find_one),
        ("findChildren", 1, node_find_children),
        ("remove", 0, node_remove),
        ("clone", 0, node_clone),
        ("appendChild", 1, node_append_child),
        ("insertChild", 2, node_insert_child),
        ("set", 1, node_set),
        ("toJSON", 0, node_to_json),
        ("toString", 0, node_to_string),
    ];
    let mut proto = ObjectInitializer::new(ctx);
    for (name, len, f) in methods {
        proto.function(NativeFunction::from_fn_ptr(f), JsString::from(name), len);
    }
    let proto = proto.build();
    ctx.register_global_property(JsString::from(PROTO), proto, hidden)?;

    let fns: [(&str, usize, Native); 6] = [
        ("run", 2, fc_run),
        ("get", 1, fc_get),
        ("getNodeById", 1, fc_get_node_by_id),
        ("findAll", 1, fc_find_all),
        ("findOne", 1, fc_find_one),
        ("create", 2, fc_create),
    ];
    let accessors: [(&str, Native, Option<Native>); 3] =
        [("currentPage", fc_current_page, Some(fc_set_current_page)), ("pages", fc_pages, None), ("selection", fc_selection, Some(fc_set_selection))];
    let accessors: Vec<_> = accessors
        .into_iter()
        .map(|(name, get, set)| {
            let g = function(ctx, &format!("get {name}"), 0, NativeFunction::from_fn_ptr(get));
            let s = set.map(|s| function(ctx, &format!("set {name}"), 1, NativeFunction::from_fn_ptr(s)));
            (name, g, s)
        })
        .collect();
    let mut fc = ObjectInitializer::new(ctx);
    for (name, len, f) in fns {
        fc.function(NativeFunction::from_fn_ptr(f), JsString::from(name), len);
    }
    for (name, kind) in CREATE {
        let kind: &'static str = kind;
        let f = NativeFunction::from_copy_closure(move |_, args, ctx| create(kind, args.first(), ctx));
        fc.function(f, JsString::from(*name), 1);
    }
    for (name, get, set) in accessors {
        fc.accessor(JsString::from(name), Some(get), set, Attribute::ENUMERABLE);
    }
    let fc = fc.build();
    ctx.register_global_property(js_string!("fc"), fc, Attribute::all())?;

    let mut console = ObjectInitializer::new(ctx);
    for (name, f) in [("log", console_log as Native), ("info", console_log), ("debug", console_log), ("warn", console_warn), ("error", console_error)]
    {
        console.function(NativeFunction::from_fn_ptr(f), JsString::from(name), 0);
    }
    let console = console.build();
    ctx.register_global_property(js_string!("console"), console, Attribute::all())?;

    let args = JsValue::from_json(args, ctx)?;
    ctx.register_global_property(js_string!("args"), args, Attribute::all())?;
    Ok(())
}

fn type_error(message: impl Into<String>) -> boa_engine::JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

fn arg(args: &[JsValue], i: usize) -> JsValue {
    args.get(i).cloned().unwrap_or_default()
}

fn hidden(name: &str, ctx: &mut Context) -> JsResult<JsObject> {
    ctx.global_object().get(JsString::from(name), ctx)?.as_object().ok_or_else(|| throw("the script API was changed"))
}

/// A layer id from a number, a numeric string or a layer object.
fn id_of(v: &JsValue, ctx: &mut Context) -> JsResult<u64> {
    let n = match v.as_object() {
        Some(o) => o.get(js_string!("id"), ctx)?.as_number(),
        None => v.as_number().or_else(|| v.as_string().and_then(|s| s.to_std_string_escaped().trim().parse::<f64>().ok())),
    };
    match n {
        Some(n) if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_991.0 => Ok(n as u64),
        _ => Err(type_error("expected a layer or a layer id")),
    }
}

fn json_of(v: &JsValue, ctx: &mut Context) -> JsResult<Value> {
    runtime::to_json(v, ctx)
}

fn from_json(v: &Value, ctx: &mut Context) -> JsResult<JsValue> {
    JsValue::from_json(v, ctx)
}

/// The layer object for `id` (made once per id and run).
pub(crate) fn node_value(id: u64, ctx: &mut Context) -> JsResult<JsValue> {
    let registry = hidden(NODES, ctx)?;
    let key = JsString::from(id.to_string());
    let known = registry.get(key.clone(), ctx)?;
    if known.is_object() {
        return Ok(known);
    }
    let proto = hidden(PROTO, ctx)?;
    let target = ObjectInitializer::new(ctx).property(js_string!("id"), id as f64, Attribute::ENUMERABLE).build();
    target.set_prototype(Some(proto));
    let proxy: JsValue = JsProxyBuilder::new(target).get(trap_get).set(trap_set).has(trap_has).delete_property(trap_delete).build(ctx)?.into();
    registry.set(key, proxy.clone(), false, ctx)?;
    Ok(proxy)
}

fn node_or_null(id: Option<u64>, ctx: &mut Context) -> JsResult<JsValue> {
    match id {
        Some(id) if runtime::node(id)?.is_some() => node_value(id, ctx),
        _ => Ok(JsValue::null()),
    }
}

fn nodes(ids: impl IntoIterator<Item = u64>, ctx: &mut Context) -> JsResult<JsValue> {
    let mut out = Vec::new();
    for id in ids {
        out.push(node_value(id, ctx)?);
    }
    Ok(JsArray::from_iter(out, ctx).into())
}

fn ids_in(v: Option<&Value>) -> Vec<u64> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default()
}

/// Property `name` of layer `id` as a script value.
fn property(id: u64, name: &str, ctx: &mut Context) -> JsResult<JsValue> {
    let snap = runtime::node(id)?;
    match name {
        "removed" => return Ok(snap.is_none().into()),
        "parent" => return node_or_null(snap.as_ref().and_then(|s| s.get("parent")).and_then(Value::as_u64), ctx),
        "children" => return nodes(ids_in(snap.as_ref().and_then(|s| s.get("children"))), ctx),
        _ => {}
    }
    match snap.as_ref().and_then(|s| s.get(name)) {
        Some(v) => from_json(v, ctx),
        None => Ok(JsValue::undefined()),
    }
}

fn target_id(args: &[JsValue], ctx: &mut Context) -> JsResult<(JsObject, u64)> {
    let target = arg(args, 0).as_object().ok_or_else(|| throw("not a layer"))?;
    let id = target.get(js_string!("id"), ctx)?;
    Ok((target, id_of(&id, ctx)?))
}

/// `get(target, key, receiver)`: the object's own members (`id`, the methods), else the layer's.
fn trap_get(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (target, id) = target_id(args, ctx)?;
    let key = arg(args, 1);
    let pk = key.to_property_key(ctx)?;
    if target.has_property(pk.clone(), ctx)? {
        return target.get(pk, ctx);
    }
    match key.as_string() {
        Some(name) => property(id, &name.to_std_string_escaped(), ctx),
        None => Ok(JsValue::undefined()),
    }
}

/// `set(target, key, value, receiver)`: `node.set` with that one property.
fn trap_set(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (target, id) = target_id(args, ctx)?;
    let key = arg(args, 1);
    let Some(name) = key.as_string().map(|s| s.to_std_string_escaped()) else { return Err(type_error("layers have no symbol properties")) };
    if READ_ONLY.contains(&name.as_str()) || target.has_property(key.to_property_key(ctx)?, ctx)? {
        return Err(type_error(format!("`{name}` can't be set")));
    }
    let value = json_of(&arg(args, 2), ctx)?;
    let mut props = Map::new();
    props.insert(name, value);
    runtime::call("node.set", &json!({"ids": [id], "props": props}))?;
    Ok(true.into())
}

fn trap_has(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let (target, id) = target_id(args, ctx)?;
    let key = arg(args, 1);
    if target.has_property(key.to_property_key(ctx)?, ctx)? {
        return Ok(true.into());
    }
    let Some(name) = key.as_string().map(|s| s.to_std_string_escaped()) else { return Ok(false.into()) };
    let snap = runtime::node(id)?;
    Ok((matches!(name.as_str(), "removed" | "parent" | "children") || snap.is_some_and(|s| s.get(&name).is_some())).into())
}

fn trap_delete(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(type_error("layer properties can't be deleted (set them, or call remove())"))
}

/// The layer a method was called on.
fn this_id(this: &JsValue, ctx: &mut Context) -> JsResult<u64> {
    id_of(this, ctx)
}

/// Layers under `ids` (each list in order) that `pred` accepts (all without one).
fn filter(ids: Vec<u64>, pred: &JsValue, first: bool, ctx: &mut Context) -> JsResult<Vec<JsValue>> {
    let f = match pred {
        v if v.is_undefined() || v.is_null() => None,
        v => Some(v.as_callable().ok_or_else(|| type_error("expected a function (layer => true | false)"))?),
    };
    let mut out = Vec::new();
    for id in ids {
        let node = node_value(id, ctx)?;
        let keep = match &f {
            Some(f) => f.call(&JsValue::undefined(), std::slice::from_ref(&node), ctx)?.to_boolean(),
            None => true,
        };
        if keep {
            out.push(node);
            if first {
                break;
            }
        }
    }
    Ok(out)
}

fn node_find_all(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this, ctx)?;
    let found = filter(runtime::descendants(id)?, &arg(args, 0), false, ctx)?;
    Ok(JsArray::from_iter(found, ctx).into())
}

fn node_find_one(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this, ctx)?;
    Ok(filter(runtime::descendants(id)?, &arg(args, 0), true, ctx)?.into_iter().next().unwrap_or_else(JsValue::null))
}

fn node_find_children(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this, ctx)?;
    let kids = ids_in(runtime::node(id)?.as_ref().and_then(|s| s.get("children")));
    let found = filter(kids, &arg(args, 0), false, ctx)?;
    Ok(JsArray::from_iter(found, ctx).into())
}

fn node_remove(this: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this, ctx)?;
    runtime::call("edit.delete", &json!({"ids": [id]}))?;
    Ok(JsValue::undefined())
}

fn node_clone(this: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this, ctx)?;
    let r = runtime::call("edit.duplicate", &json!({"ids": [id]}))?;
    let copy = ids_in(r.get("ids")).first().copied().ok_or_else(|| throw("edit.duplicate made no copy"))?;
    node_value(copy, ctx)
}

fn node_append_child(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this, ctx)?;
    let child = id_of(&arg(args, 0), ctx)?;
    runtime::call("node.reparent", &json!({"ids": [child], "parent": id}))?;
    node_value(child, ctx)
}

fn node_insert_child(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this, ctx)?;
    let index = arg(args, 0)
        .as_number()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or_else(|| type_error("insertChild(index, layer): index must be a number ≥ 0"))?;
    let child = id_of(&arg(args, 1), ctx)?;
    runtime::call("node.reparent", &json!({"ids": [child], "parent": id, "index": index.floor()}))?;
    node_value(child, ctx)
}

fn node_set(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this, ctx)?;
    let props = json_of(&arg(args, 0), ctx)?;
    if !props.is_object() {
        return Err(type_error("set(props): props must be an object of node.set properties"));
    }
    runtime::call("node.set", &json!({"ids": [id], "props": props}))?;
    Ok(this.clone())
}

fn node_to_json(this: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this, ctx)?;
    match runtime::node(id)? {
        Some(v) => from_json(&v, ctx),
        None => from_json(&json!({"id": id, "removed": true}), ctx),
    }
}

fn node_to_string(this: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = this_id(this, ctx)?;
    let text = match runtime::node(id)? {
        Some(v) => format!(
            "[{} {} \"{}\"]",
            v.get("type").and_then(Value::as_str).unwrap_or("layer"),
            id,
            v.get("name").and_then(Value::as_str).unwrap_or("")
        ),
        None => format!("[removed layer {id}]"),
    };
    Ok(JsString::from(text).into())
}

fn fc_run(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let command =
        arg(args, 0).as_string().map(|s| s.to_std_string_escaped()).ok_or_else(|| type_error("fc.run(command, params): command must be a string"))?;
    let params = match arg(args, 1) {
        v if v.is_undefined() || v.is_null() => json!({}),
        v => json_of(&v, ctx)?,
    };
    if !params.is_object() {
        return Err(type_error("fc.run(command, params): params must be an object"));
    }
    let r = runtime::call(&command, &params)?;
    from_json(&r, ctx)
}

fn fc_get(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = id_of(&arg(args, 0), ctx)?;
    match runtime::node(id)? {
        Some(v) => from_json(&v, ctx),
        None => Ok(JsValue::null()),
    }
}

fn fc_get_node_by_id(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = id_of(&arg(args, 0), ctx).ok();
    node_or_null(id, ctx)
}

fn current_page() -> JsResult<u64> {
    runtime::context()?.get("page").and_then(Value::as_u64).ok_or_else(|| throw("no document is open"))
}

fn fc_find_all(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let found = filter(runtime::descendants(current_page()?)?, &arg(args, 0), false, ctx)?;
    Ok(JsArray::from_iter(found, ctx).into())
}

fn fc_find_one(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    Ok(filter(runtime::descendants(current_page()?)?, &arg(args, 0), true, ctx)?.into_iter().next().unwrap_or_else(JsValue::null))
}

fn fc_current_page(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    node_value(current_page()?, ctx)
}

fn fc_set_current_page(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = id_of(&arg(args, 0), ctx)?;
    runtime::call("page.select", &json!({"id": id}))?;
    Ok(JsValue::undefined())
}

fn fc_pages(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let pages = ids_in(runtime::context()?.get("pages"));
    nodes(pages, ctx)
}

fn fc_selection(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let sel = ids_in(runtime::context()?.get("selection"));
    nodes(sel, ctx)
}

fn fc_set_selection(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let list = arg(args, 0);
    let mut ids = Vec::new();
    if let Some(o) = list.as_object() {
        let len = o
            .get(js_string!("length"), ctx)?
            .as_number()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or_else(|| type_error("fc.selection = [layers]"))?;
        for i in 0..(len as u64).min(1_000_000) {
            let v = o.get(i, ctx)?;
            ids.push(id_of(&v, ctx)?);
        }
    } else if !list.is_null() && !list.is_undefined() {
        return Err(type_error("fc.selection = [layers]"));
    }
    if ids.is_empty() {
        runtime::call("select.none", &json!({}))?;
    } else {
        runtime::call("select.set", &json!({"ids": ids}))?;
    }
    Ok(JsValue::undefined())
}

/// `node.create` a layer of `kind`; `opts` are its parameters (`x`, `y`, `width`, `height`, `parent`
/// (a layer or id), `name`, `text`, `props`, …). A string `opts` is a text layer's text. New layers
/// don't change the selection unless `select: true`.
fn create(kind: &str, opts: Option<&JsValue>, ctx: &mut Context) -> JsResult<JsValue> {
    let mut params = Map::new();
    match opts {
        Some(v) if v.is_string() => {
            params.insert("text".into(), json_of(v, ctx)?);
        }
        Some(v) if v.is_object() => {
            if let Some(o) = v.as_object() {
                let parent = o.get(js_string!("parent"), ctx)?;
                let json = json_of(v, ctx)?;
                if let Value::Object(m) = json {
                    params = m;
                }
                if parent.is_object() {
                    params.insert("parent".into(), json!(id_of(&parent, ctx)?));
                }
            }
        }
        Some(v) if !v.is_undefined() && !v.is_null() => return Err(type_error("expected an object of node.create parameters")),
        _ => {}
    }
    params.insert("type".into(), json!(kind));
    params.entry("select").or_insert(json!(false));
    let r = runtime::call("node.create", &Value::Object(params))?;
    let id = r.get("id").and_then(Value::as_u64).ok_or_else(|| throw("node.create returned no id"))?;
    node_value(id, ctx)
}

fn fc_create(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let kind =
        arg(args, 0).as_string().map(|s| s.to_std_string_escaped()).ok_or_else(|| type_error("fc.create(type, params): type must be a string"))?;
    create(&kind, args.get(1), ctx)
}

/// One `console` line: strings as they are, everything else as JSON.
fn line(args: &[JsValue], ctx: &mut Context) -> JsResult<String> {
    let mut parts = Vec::with_capacity(args.len());
    for a in args {
        let text = match a.as_string() {
            Some(s) => s.to_std_string_escaped(),
            None => match runtime::stringify(a, ctx) {
                Ok(Some(s)) => s,
                _ => a.to_string(ctx).map(|s| s.to_std_string_escaped()).unwrap_or_default(),
            },
        };
        parts.push(text);
    }
    Ok(parts.join(" "))
}

fn console_log(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    runtime::log(line(args, ctx)?)?;
    Ok(JsValue::undefined())
}

fn console_warn(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    runtime::log(format!("warning: {}", line(args, ctx)?))?;
    Ok(JsValue::undefined())
}

fn console_error(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    runtime::log(format!("error: {}", line(args, ctx)?))?;
    Ok(JsValue::undefined())
}
