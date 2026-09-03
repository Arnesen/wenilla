//! The JavaScript boundary — the only file in the bridge that names `js_sys`/`web_sys`, in the
//! `webprogress` pattern: a wasm arm that talks to `window.__wenilla_bridge`, and a native arm
//! with the same signatures that answers "no page" (so every call site type-checks on x86,
//! where the browser build cannot be compiled). Every JS call's result is discarded: a page
//! handler that throws must never take the client down.

use benilla_ui::script::plain::PlainValue;

use super::{BridgeCommand, BridgeConfig, BridgeOutbox};

/// The `wake` closure installed on the hook object, kept alive as long as the page holds the
/// object (dropping it invalidates the JS function — a page calling it then gets a thrown
/// error, which is the honest answer once the bridge has let go). A `NonSend` resource: a
/// `Closure` is not `Send`.
#[derive(Default)]
pub(crate) struct WakeHook(pub(crate) Option<imp::Wake>);

pub(crate) use imp::{
    drain_queue, emit_event, emit_frame, hook_present, install_wake, read_config,
};

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::*;
    use bevy::winit::{EventLoopProxyWrapper, WinitUserEvent};
    use js_sys::{Array, Function, Object, Reflect};
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::{JsCast, JsValue};

    pub(crate) type Wake = Closure<dyn FnMut()>;

    const HOOK: &str = "__wenilla_bridge";

    fn hook() -> Option<Object> {
        let window = web_sys::window()?;
        Reflect::get(&window, &HOOK.into())
            .ok()?
            .dyn_into::<Object>()
            .ok()
    }

    fn get(obj: &Object, key: &str) -> JsValue {
        Reflect::get(obj, &key.into()).unwrap_or(JsValue::UNDEFINED)
    }

    fn function(obj: &Object, key: &str) -> Option<Function> {
        get(obj, key).dyn_into::<Function>().ok()
    }

    pub(crate) fn hook_present() -> bool {
        hook().is_some()
    }

    /// The knobs, as the page left them; a missing or malformed one keeps its default.
    pub(crate) fn read_config(cfg: &mut BridgeConfig) {
        let Some(h) = hook() else { return };
        let defaults = BridgeConfig::default();
        cfg.hz = get(&h, "hz").as_f64().map_or(defaults.hz, |v| v as f32);
        cfg.radius = get(&h, "radius")
            .as_f64()
            .map_or(defaults.radius, |v| v as f32);
        cfg.max_units = get(&h, "maxUnits")
            .as_f64()
            .map_or(defaults.max_units, |v| v.max(0.0) as usize);
        let events = get(&h, "events");
        cfg.events = if events.as_string().as_deref() == Some("*") {
            super::super::EventFilter::All
        } else if let Ok(arr) = events.dyn_into::<Array>() {
            let names: std::collections::HashSet<String> =
                arr.iter().filter_map(|v| v.as_string()).collect();
            if names.contains("*") {
                super::super::EventFilter::All
            } else if names.is_empty() {
                super::super::EventFilter::None
            } else {
                super::super::EventFilter::Some(names)
            }
        } else {
            super::super::EventFilter::None
        };
    }

    /// Take every command object off `queue`, leaving it empty. A shape the bridge does not
    /// understand becomes one `error` event rather than a silent drop.
    pub(crate) fn drain_queue(out: &mut BridgeOutbox) -> Vec<BridgeCommand> {
        let Some(h) = hook() else { return Vec::new() };
        let Ok(queue) = get(&h, "queue").dyn_into::<Array>() else {
            return Vec::new();
        };
        let mut cmds = Vec::with_capacity(queue.length() as usize);
        for item in queue.iter() {
            let Ok(obj) = item.dyn_into::<Object>() else {
                report(out, "not an object");
                continue;
            };
            let op = get(&obj, "op").as_string().unwrap_or_default();
            let str_of = |k: &str| get(&obj, k).as_string();
            let num_of = |k: &str| get(&obj, k).as_f64();
            let parsed = match op.as_str() {
                "hold" => str_of("cmd").map(|cmd| BridgeCommand::Hold {
                    cmd,
                    down: get(&obj, "down").as_bool().unwrap_or(true),
                }),
                "fire" => str_of("cmd").map(|cmd| BridgeCommand::Fire {
                    cmd,
                    amount: num_of("amount").unwrap_or(1.0) as f32,
                }),
                "look" => num_of("dyaw").map(|dyaw| BridgeCommand::Look { dyaw: dyaw as f32 }),
                "lua" => str_of("chunk").map(|chunk| BridgeCommand::Lua {
                    id: num_of("id").unwrap_or(0.0) as u32,
                    chunk,
                }),
                "chat" => str_of("text").map(BridgeCommand::Chat),
                "release" => Some(BridgeCommand::Release),
                _ => None,
            };
            match parsed {
                Some(c) => cmds.push(c),
                None => report(out, &format!("bad command: op={op:?}")),
            }
        }
        queue.set_length(0);
        cmds
    }

    fn report(out: &mut BridgeOutbox, reason: &str) {
        out.push(
            "error",
            PlainValue::Map(vec![("reason".into(), PlainValue::Str(reason.into()))]),
        );
    }

    pub(crate) fn emit_frame(frame: &PlainValue) {
        let Some(h) = hook() else { return };
        let Some(f) = function(&h, "onFrame") else {
            return;
        };
        let _ = f.call1(&JsValue::NULL, &to_js(frame));
    }

    pub(crate) fn emit_event(name: &str, payload: &PlainValue) {
        let Some(h) = hook() else { return };
        let Some(f) = function(&h, "onEvent") else {
            return;
        };
        let _ = f.call2(&JsValue::NULL, &name.into(), &to_js(payload));
    }

    /// Put `wake()` on the hook object: a call sends `WinitUserEvent::WakeUp` through the
    /// event-loop proxy, which runs one app update even when no animation frame will come —
    /// the hidden tab's heartbeat, driven by the page from a Worker.
    pub(crate) fn install_wake(proxy: &EventLoopProxyWrapper) -> Option<Wake> {
        let h = hook()?;
        let proxy = (**proxy).clone();
        let wake: Wake = Closure::new(move || {
            let _ = proxy.send_event(WinitUserEvent::WakeUp);
        });
        Reflect::set(&h, &"wake".into(), wake.as_ref().unchecked_ref::<JsValue>()).ok()?;
        Some(wake)
    }

    fn to_js(v: &PlainValue) -> JsValue {
        match v {
            PlainValue::Null => JsValue::NULL,
            PlainValue::Bool(b) => JsValue::from_bool(*b),
            PlainValue::Num(n) => JsValue::from_f64(*n),
            PlainValue::Str(s) => JsValue::from_str(s),
            PlainValue::List(items) => {
                let arr = Array::new_with_length(items.len() as u32);
                for (i, item) in items.iter().enumerate() {
                    arr.set(i as u32, to_js(item));
                }
                arr.into()
            }
            PlainValue::Map(entries) => {
                let obj = Object::new();
                for (k, item) in entries {
                    let _ = Reflect::set(&obj, &k.as_str().into(), &to_js(item));
                }
                obj.into()
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::*;
    use bevy::winit::EventLoopProxyWrapper;

    /// Nothing to keep alive natively; the type exists so the resource's shape is one.
    pub(crate) struct Wake;

    /// Native: there is no page. The one way to see the bridge's output off the browser is the
    /// `bridge` trace tag (`WOW_TRACE=bridge`), which prints every event — the debugging aid for
    /// the parts that do not need a browser to be wrong.
    pub(crate) fn hook_present() -> bool {
        false
    }

    pub(crate) fn read_config(_cfg: &mut BridgeConfig) {}

    pub(crate) fn drain_queue(_out: &mut BridgeOutbox) -> Vec<BridgeCommand> {
        Vec::new()
    }

    pub(crate) fn emit_frame(_frame: &PlainValue) {}

    pub(crate) fn emit_event(name: &str, payload: &PlainValue) {
        if benilla_assets::trace::enabled_for("bridge") {
            benilla_assets::trace::line("bridge", &format!("{name} {payload:?}"));
        }
    }

    pub(crate) fn install_wake(_proxy: &EventLoopProxyWrapper) -> Option<Wake> {
        None
    }
}
