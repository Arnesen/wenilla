use mlua::{Function, ObjectLike, Table, Value};

use crate::framexml::Element;

use super::{children_named, Loader};

impl Loader<'_> {
    /// `<Scripts>` → SetScript each handler (rf24 `0x769ef0`). Handler source comes from the element
    /// body (`<OnClick>lua…</OnClick>`) or a `function="Global"` attribute. Returns the compiled
    /// `OnLoad` handle (if any) so the caller can fire it bottom-up. A body that fails to *compile* is
    /// an error; an unsupported handler *name* (e.g. the keyboard-focus handlers `OnKeyDown`/`OnChar`,
    /// not yet modeled) is a warn-once gap.
    pub(super) fn apply_scripts(
        &mut self,
        el: &Element,
        wrapper: &Table,
        dbg: &str,
    ) -> Option<Function> {
        let mut onload = None;
        for scripts in children_named(el, "Scripts") {
            for handler in &scripts.children {
                let name = handler.tag.clone();
                let Some(func) = self.compile_handler(handler, &name, dbg) else {
                    continue;
                };
                // SetScript stores it; an unsupported name errors — surface as a gap, don't drop hard.
                if let Err(e) = wrapper.call_method::<()>("SetScript", (name.clone(), func.clone()))
                {
                    self.warn_once(
                        &format!("script:{name}"),
                        format!("{dbg}: SetScript(\"{name}\") unsupported in v1: {e}"),
                    );
                    continue;
                }
                if name.eq_ignore_ascii_case("OnLoad") {
                    onload = Some(func);
                }
            }
        }
        onload
    }

    /// Compile a handler element into `function(self, ...) <body> end`, or resolve its
    /// `function="Global"` reference. A syntax error in the body is recorded as an error (and the
    /// handler dropped), never a panic.
    pub(super) fn compile_handler(
        &mut self,
        handler: &Element,
        name: &str,
        dbg: &str,
    ) -> Option<Function> {
        let body = handler.body.trim();
        if !body.is_empty() {
            let src = format!("return function(self, ...)\n{body}\nend");
            match self
                .lua()
                .load(&src)
                .set_name(format!("{dbg}:{name}"))
                .set_mode(mlua::ChunkMode::Text)
                .eval::<Function>()
            {
                Ok(f) => return Some(f),
                Err(e) => {
                    self.report
                        .errors
                        .push(format!("{dbg}: compiling <{name}>: {e}"));
                    return None;
                }
            }
        }
        if let Some(global) = handler.attr("function") {
            match self.lua().globals().get::<Value>(global) {
                Ok(Value::Function(f)) => return Some(f),
                _ => {
                    self.warn_once(
                        &format!("fn:{global}"),
                        format!(
                            "{dbg}: <{name} function=\"{global}\">: no such global function (yet)"
                        ),
                    );
                    return None;
                }
            }
        }
        None
    }

    /// Fire a captured `OnLoad` with the frame wrapper as both the legacy `this` global (RF-0025,
    /// set-then-restored) and the modern `self` argument — the same dual convention the host's event
    /// path uses; we replicate only the `this` set/restore here because there is no public host API to
    /// fire `OnLoad` directly (it is not an event). Handler errors are recorded, never propagated.
    pub(super) fn fire_onload(&mut self, wrapper: &Table, func: &Function, dbg: &str) {
        // The RF-0025 `this`/`self` convention lives in one home (`UiScript::invoke_handler`); the
        // loader doesn't re-implement the set/restore, it just supplies the wrapper + captured func.
        if let Err(e) = self.script.invoke_handler(wrapper, func) {
            self.report.errors.push(format!("{dbg}: OnLoad: {e}"));
        }
    }

    /// Call a frame-wrapper method, recording (not propagating) any error.
    pub(super) fn call(
        &mut self,
        wrapper: &Table,
        method: &str,
        args: impl mlua::IntoLuaMulti,
        dbg: &str,
    ) {
        if let Err(e) = wrapper.call_method::<()>(method, args) {
            self.report.errors.push(format!("{dbg}: {method}: {e}"));
        }
    }

    /// Call a region-wrapper method, recording (not propagating) any error.
    pub(super) fn call_region(
        &mut self,
        region: &Table,
        method: &str,
        args: impl mlua::IntoLuaMulti,
        dbg: &str,
    ) {
        if let Err(e) = region.call_method::<()>(method, args) {
            self.report
                .errors
                .push(format!("{dbg}: region {method}: {e}"));
        }
    }
}
