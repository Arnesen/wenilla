use mlua::Table;

use crate::framexml::{self, Element};

use super::{abs_dim, children_named, Loader};

impl Loader<'_> {
    /// LoadXML attribute handling (rf24 `0x769820`): the subset the v1 object model exposes, plus a
    /// warn-once for the attributes whose methods don't exist yet (a next-phase gap, not a workaround).
    pub(super) fn apply_attrs(&mut self, el: &Element, wrapper: &Table, dbg: &str) {
        if el.attr_bool("hidden") {
            self.call(wrapper, "Hide", (), dbg);
        }
        if let Some(strata) = el.attr("frameStrata") {
            self.call(wrapper, "SetFrameStrata", strata.to_string(), dbg);
        }
        if let Some(level) = el.attr("frameLevel") {
            if let Ok(n) = level.parse::<i64>() {
                self.call(wrapper, "SetFrameLevel", n, dbg);
            }
        }
        if let Some(alpha) = el.attr("alpha") {
            if let Ok(a) = alpha.parse::<f32>() {
                self.call(wrapper, "SetAlpha", a, dbg);
            }
        }
        if let Some(id) = el.attr("id") {
            if let Ok(n) = id.parse::<i64>() {
                self.call(wrapper, "SetID", n, dbg);
            }
        }
        // `clampedToScreen="true"` → SetClampedToScreen (rf24 `0x768cc0` — geometry flags bit4,
        // the layout resolve's screen clamp; GameTooltip frames carry it by construction).
        if el.attr_bool("clampedToScreen") {
            self.call(wrapper, "SetClampedToScreen", true, dbg);
        }
        // `enableMouse` lands as the real EnableMouse call (the hit test gates on it —
        // pointer.rs): a ref frame authored mouse-blocking (StaticPopup, WorldMapBlackout, the
        // minimap cluster) must actually capture. The loader ignored it for a while after the
        // native landed — the stale "v1 gap" note here — which left those frames click-through
        // and the minimap deaf to its ping click (0434 phase 6c's root cause).
        if el.attr_bool("enableMouse") {
            self.call(wrapper, "EnableMouse", true, dbg);
        }
        // `<HitRectInsets><AbsInset left= right= top= bottom=/></HitRectInsets>` (also accepted
        // inline on the element) → SetHitRectInsets: the frame's MOUSE rect, inset from its
        // resolved rect. Absent sides read 0, so a partial element insets only what it names.
        if let Some(ins) = children_named(el, "HitRectInsets").next() {
            let src = children_named(ins, "AbsInset").next().unwrap_or(ins);
            let side = |k: &str| {
                src.attr(k)
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .unwrap_or(0.0)
            };
            self.call(
                wrapper,
                "SetHitRectInsets",
                (side("left"), side("right"), side("top"), side("bottom")),
                dbg,
            );
        }
        // `movable`/`resizable` → SetMovable/SetResizable — the same flag word the methods write
        // (`0x76a3c0` with mask 0x100 / 0x200; wow-re `rf24-framexml-loader.md` records the loader
        // calling that very setter). Both were in the gap list below until the movable family
        // landed; leaving them there would have left every `movable="true"` reference window
        // undraggable while `SetMovable` worked from Lua.
        if el.attr_bool("movable") {
            self.call(wrapper, "SetMovable", true, dbg);
        }
        if el.attr_bool("resizable") {
            self.call(wrapper, "SetResizable", true, dbg);
        }
        // `toplevel` → SetToplevel — the third bit of that same flag word (`0x76a3c0` mask `0x1`,
        // XML site `0x7698ec`; wow-re `ui/scratch/toplevel-raise.md`), and the third attribute to
        // graduate out of the gap list below for the same reason: the raise law is built
        // (`script::object::toplevel`), so accepting the attribute now means the behaviour, not
        // silence. 82 corpus addons and thirteen of our own frames declare it.
        if el.attr_bool("toplevel") {
            self.call(wrapper, "SetToplevel", true, dbg);
        }
        // Still a genuine gap — the window-behaviour surface (input arbitration, decision 0068).
        // Flagged rather than silently dropped or faked: accepting an attribute without
        // implementing it is strictly worse than saying so.
        if el.attr_bool("enableKeyboard") {
            self.warn_once(
                "EnableKeyboard",
                "`enableKeyboard` ignored: EnableKeyboard is not in the v1 object model (gap)",
            );
        }
    }

    /// `<Size>` → SetWidth/SetHeight (rf24 `0x767800`). Accepts either `<Size><AbsDimension x= y=/>`
    /// or the inline `<Size x= y=/>` form; a dimension that's absent is left untouched (the client's
    /// "0 = derive"). ALL `<Size>` children apply, in document order — template expansion appends the
    /// instance's children after the template's ([`crate::framexml::expand`]), and the client simply
    /// processes each child in turn, so an instance's own `<Size>` overwrites its template's. (Taking
    /// only `.next()` here silently pinned every templated frame to the TEMPLATE's size; caught on
    /// the quest log's Abandon button — 125×21 in the instance XML, 80×22 on screen.)
    pub(super) fn apply_size(&mut self, el: &Element, wrapper: &Table, dbg: &str) {
        for size in children_named(el, "Size") {
            let (x, y) = abs_dim(size);
            if let Some(w) = x {
                self.call(wrapper, "SetWidth", w, dbg);
            }
            if let Some(h) = y {
                self.call(wrapper, "SetHeight", h, dbg);
            }
        }
    }

    /// `<Anchors>` → SetPoint per `<Anchor point= relativeTo= relativePoint=><Offset .../></Anchor>`
    /// (rf24 `0x767800`). `relativePoint` defaults to `point`; `relativeTo` is `$parent`-substituted
    /// (rf27) and passed by name (the object model resolves it, falling back to the parent when
    /// absent/unresolved); a missing `point` is skipped with a warning ("Invalid anchor point").
    /// The `setAllPoints="true"` shorthand applies first, like the region path — a frame carrying
    /// only the attribute (WorldMapFrame's chrome layers) pins TOPLEFT+BOTTOMRIGHT to its parent.
    pub(super) fn apply_anchors(
        &mut self,
        el: &Element,
        wrapper: &Table,
        parent_name: &str,
        dbg: &str,
    ) {
        if el.attr_bool("setAllPoints") {
            self.call(wrapper, "SetAllPoints", (), dbg);
        }
        for anchors in children_named(el, "Anchors") {
            for anchor in children_named(anchors, "Anchor") {
                let Some(point) = anchor.attr("point") else {
                    self.report
                        .warnings
                        .push(format!("{dbg}: <Anchor> without a point; skipped"));
                    continue;
                };
                let rel_point = anchor.attr("relativePoint").unwrap_or(point).to_string();
                let rel_to: Option<String> = anchor
                    .attr("relativeTo")
                    .map(|r| framexml::resolve_name(r, parent_name));
                let (x, y) = children_named(anchor, "Offset")
                    .next()
                    .map(abs_dim)
                    .unwrap_or((None, None));
                self.call(
                    wrapper,
                    "SetPoint",
                    (
                        point.to_string(),
                        rel_to,
                        rel_point,
                        x.unwrap_or(0.0),
                        y.unwrap_or(0.0),
                    ),
                    dbg,
                );
            }
        }
    }
}
