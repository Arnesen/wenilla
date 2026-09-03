//! **Plain values across the host boundary** — a Lua result rendered as data no caller has to
//! hold an mlua handle for (the MAXCSTACK discipline, `mod.rs`), for a host that relays it
//! somewhere Lua does not reach: the web bridge, which hands page JavaScript the result of a
//! chunk it asked the VM to evaluate.
//!
//! The rendering is deliberately lossy and bounded. A chunk can return `UIParent`, whose table
//! reaches every frame in the session; a bridge that serialized that would hang the frame. So
//! tables render to a depth of [`MAX_DEPTH`] and the whole result to [`MAX_NODES`] values, and
//! past either limit a table is `"<table>"`, the way a function is `"<function>"`. Numbers are
//! `f64` (an `i64` above 2⁵³ loses precision — nothing FrameXML returns is that large).

use mlua::Value;

use super::{ScriptValue, UiScript};

/// A value with no VM behind it: `nil`, booleans, numbers, strings, and tables as either a list
/// (keys `1..=n`, in order) or a map (every key stringified, in `pairs` order).
#[derive(Clone, Debug, PartialEq)]
pub enum PlainValue {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    List(Vec<PlainValue>),
    Map(Vec<(String, PlainValue)>),
}

impl From<&ScriptValue> for PlainValue {
    fn from(v: &ScriptValue) -> Self {
        match v {
            ScriptValue::Nil => PlainValue::Null,
            ScriptValue::Bool(b) => PlainValue::Bool(*b),
            ScriptValue::Int(i) => PlainValue::Num(*i as f64),
            ScriptValue::Number(n) => PlainValue::Num(*n),
            ScriptValue::Str(s) => PlainValue::Str(s.clone()),
        }
    }
}

/// How deep a returned table renders before its children read as `"<table>"`.
pub const MAX_DEPTH: u8 = 4;
/// How many values one result may render in total, all returns and all nesting included.
pub const MAX_NODES: usize = 512;

impl UiScript {
    /// Evaluate a text chunk and render everything it returns as [`PlainValue`]s — the bridge's
    /// query channel. The error string of a chunk that fails to load or run is the `Err`; a
    /// chunk that returns nothing is `Ok(vec![])`. Handles are converted and dropped inside this
    /// call; nothing of the VM's escapes.
    pub fn eval_plain(&self, chunk: &str) -> Result<Vec<PlainValue>, String> {
        let values: mlua::MultiValue = self
            .lua
            .load(chunk)
            .set_mode(mlua::ChunkMode::Text)
            .eval()
            .map_err(|e| e.to_string())?;
        let mut budget = MAX_NODES;
        Ok(values.iter().map(|v| render(v, 0, &mut budget)).collect())
    }
}

fn render(v: &Value, depth: u8, budget: &mut usize) -> PlainValue {
    if *budget == 0 {
        return PlainValue::Str("<truncated>".into());
    }
    *budget -= 1;
    match v {
        Value::Nil => PlainValue::Null,
        Value::Boolean(b) => PlainValue::Bool(*b),
        Value::Integer(i) => PlainValue::Num(*i as f64),
        Value::Number(n) => PlainValue::Num(*n),
        Value::String(s) => PlainValue::Str(s.to_string_lossy()),
        Value::Table(t) => {
            if depth >= MAX_DEPTH {
                return PlainValue::Str("<table>".into());
            }
            // A list is a table whose keys are exactly `1..=n` — `raw_len` gives `n`, and one
            // `pairs` walk decides whether anything else is there.
            let n = t.raw_len();
            let mut entries: Vec<(Value, Value)> = Vec::new();
            let mut is_list = n > 0;
            for pair in t.pairs::<Value, Value>() {
                let Ok((k, val)) = pair else { break };
                if is_list {
                    match k {
                        Value::Integer(i) if i >= 1 && i as usize <= n => {}
                        _ => is_list = false,
                    }
                }
                entries.push((k, val));
                if entries.len() > *budget {
                    break; // the budget will cut the render anyway; stop walking
                }
            }
            if is_list && entries.len() == n {
                let mut list = vec![PlainValue::Null; n];
                for (k, val) in &entries {
                    if let Value::Integer(i) = k {
                        list[*i as usize - 1] = render(val, depth + 1, budget);
                    }
                }
                PlainValue::List(list)
            } else {
                PlainValue::Map(
                    entries
                        .iter()
                        .map(|(k, val)| (key_string(k), render(val, depth + 1, budget)))
                        .collect(),
                )
            }
        }
        Value::Function(_) => PlainValue::Str("<function>".into()),
        Value::Thread(_) => PlainValue::Str("<thread>".into()),
        Value::LightUserData(_) | Value::UserData(_) => PlainValue::Str("<userdata>".into()),
        _ => PlainValue::Str("<?>".into()),
    }
}

fn key_string(k: &Value) -> String {
    match k {
        Value::String(s) => s.to_string_lossy(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        other => format!("<{}>", other.type_name()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> UiScript {
        UiScript::new().expect("construct UiScript")
    }

    #[test]
    fn scalars_lists_and_maps_render_plainly() {
        let got = s()
            .eval_plain(r#"return 1, "a", true, nil, {1, 2, {x = 3}}, {k = "v"}"#)
            .unwrap();
        assert_eq!(got[0], PlainValue::Num(1.0));
        assert_eq!(got[1], PlainValue::Str("a".into()));
        assert_eq!(got[2], PlainValue::Bool(true));
        assert_eq!(got[3], PlainValue::Null);
        assert_eq!(
            got[4],
            PlainValue::List(vec![
                PlainValue::Num(1.0),
                PlainValue::Num(2.0),
                PlainValue::Map(vec![("x".into(), PlainValue::Num(3.0))]),
            ])
        );
        assert_eq!(
            got[5],
            PlainValue::Map(vec![("k".into(), PlainValue::Str("v".into()))])
        );
    }

    #[test]
    fn errors_functions_and_depth_are_bounded() {
        let vm = s();
        assert!(vm.eval_plain("return (").is_err(), "a load error is an Err");
        assert!(vm.eval_plain("error('boom')").unwrap_err().contains("boom"));
        assert_eq!(vm.eval_plain("").unwrap(), vec![]);
        assert_eq!(
            vm.eval_plain("return function() end").unwrap()[0],
            PlainValue::Str("<function>".into())
        );
        // Depth: {{{{{1}}}}} is five levels; the fifth reads as "<table>".
        let deep = vm.eval_plain("return {{{{{1}}}}}").unwrap().remove(0);
        let mut cur = deep;
        for _ in 0..MAX_DEPTH {
            cur = match cur {
                PlainValue::List(mut l) => l.remove(0),
                other => panic!("expected a list, got {other:?}"),
            };
        }
        assert_eq!(cur, PlainValue::Str("<table>".into()));
        // Budget: a 10 000-entry list renders at most MAX_NODES values.
        let big = vm
            .eval_plain("local t = {} for i = 1, 10000 do t[i] = i end return t")
            .unwrap()
            .remove(0);
        match big {
            PlainValue::List(l) => assert!(l.len() <= MAX_NODES),
            PlainValue::Map(m) => assert!(m.len() <= MAX_NODES),
            other => panic!("{other:?}"),
        }
    }
}
