//! The drag gesture (decision 0216 §3) — `RegisterForDrag`/`OnDragStart`/`OnDragStop`/
//! `OnReceiveDrag`'s mechanics: arm on press, start past a threshold, resolve on release. Split
//! out of [`super`] purely for size — [`super::pointer`](crate::script::pointer) is the only
//! external caller (via [`super`]'s re-exports), and the payload types/transition seam this
//! drives live one level up.

use crate::script::Model;
use crate::widget::FrameHandle;

/// INTERIM pixel drag-start threshold (0216 §5 (d) is the byte-verified trigger; the reference
/// almost certainly uses a small OS/engine drag-distance constant, not this one). Compared with
/// **strict** `>` — a move exactly at the threshold does not start the gesture.
pub(crate) const DRAG_START_THRESHOLD: f32 = 4.0;

/// An in-flight drag gesture: armed at mouse-down on a [`Model::drag_registered`] frame,
/// `started` once the cursor has moved past [`DRAG_START_THRESHOLD`] from the press point.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DragGesture {
    pub(crate) button: String,
    pub(crate) source: FrameHandle,
    pub(crate) start: (f32, f32),
    pub(crate) started: bool,
}

/// A resolved drag release (a gesture whose button matched the release) — [`Model::drag`] is
/// always cleared alongside this, win or lose.
pub(crate) struct DragRelease {
    /// Whether the gesture crossed [`DRAG_START_THRESHOLD`] (so `OnDragStart` fired). A released
    /// gesture that never started fires nothing — the normal click path proceeds untouched.
    pub(crate) started: bool,
    pub(crate) source: FrameHandle,
}

/// Arm (or clear) the drag gesture on a mouse-DOWN. `hit` is the pressed frame (`None` = the
/// press hit nothing); a press always REPLACES any leftover in-flight gesture — its matching
/// release should already have cleared it, but a press is the one moment we know for certain no
/// gesture should still be armed against a *different* earlier press.
pub(crate) fn arm_drag(model: &mut Model, hit: Option<FrameHandle>, button: &str, pos: (f32, f32)) {
    model.drag = hit
        .filter(|&h| {
            model
                .drag_registered
                .get(&h)
                .is_some_and(|set| set.iter().any(|b| b.eq_ignore_ascii_case(button)))
        })
        .map(|h| DragGesture {
            button: button.to_string(),
            source: h,
            start: pos,
            started: false,
        });
}

/// Advance the armed gesture on a mouse-move: once the cursor has moved past
/// [`DRAG_START_THRESHOLD`] from the press point, mark it started. Returns `(source id, button)`
/// to fire `OnDragStart` with, exactly once per gesture (already-started and no-gesture both
/// answer `None`); a source that died before starting fires nothing.
pub(crate) fn maybe_start_drag(model: &mut Model, pos: (f32, f32)) -> Option<(u32, String)> {
    let (source, button) = {
        let g = model.drag.as_ref()?;
        if g.started {
            return None;
        }
        let (sx, sy) = g.start;
        let dist = ((pos.0 - sx).powi(2) + (pos.1 - sy).powi(2)).sqrt();
        if dist <= DRAG_START_THRESHOLD {
            return None;
        }
        (g.source, g.button.clone())
    };
    model.drag.as_mut().expect("checked Some above").started = true;
    let id = model
        .arena
        .frame(source)
        .is_some()
        .then(|| model.frame_id(source))?;
    Some((id, button))
}

/// Resolve (and clear) the in-flight drag gesture matching a mouse-button release's `button`, if
/// any (case-insensitive, the `RegisterForClicks` precedent).
pub(crate) fn take_drag(model: &mut Model, button: &str) -> Option<DragRelease> {
    let g = model
        .drag
        .take_if(|g| g.button.eq_ignore_ascii_case(button))?;
    Some(DragRelease {
        started: g.started,
        source: g.source,
    })
}

#[cfg(test)]
mod tests {
    use crate::script::cursor::{CursorItem, CursorPayload};
    use crate::script::UiScript;

    fn drag_script() -> UiScript {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        s
    }

    #[test]
    fn drag_start_stop_and_receive_suppress_onclick() {
        let mut s = drag_script();
        s.run(
            r#"
            drag_starts, drag_stops, receives = 0, 0, 0
            click_a, click_b = 0, 0
            drag_button = nil
            local a = CreateFrame("Frame", "A")
            a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(400, 600); a:EnableMouse(true)
            a:RegisterForDrag("LeftButton")
            a:SetScript("OnDragStart", function(self, button) drag_starts = drag_starts + 1; drag_button = button end)
            a:SetScript("OnDragStop", function(self) drag_stops = drag_stops + 1 end)
            a:SetScript("OnClick", function(self) click_a = click_a + 1 end)
            local b = CreateFrame("Frame", "B")
            b:SetPoint("BOTTOMLEFT", 400, 0); b:SetSize(400, 600); b:EnableMouse(true)
            b:SetScript("OnReceiveDrag", function(self) receives = receives + 1 end)
            b:SetScript("OnClick", function(self) click_b = click_b + 1 end)
            "#,
        )
        .unwrap();
        s.resolve();

        s.mouse_button(100.0, 300.0, "LeftButton", true); // press inside A
        s.mouse_move(102.0, 300.0); // 2px — under the threshold
        assert_eq!(s.eval::<i64>("return drag_starts").unwrap(), 0);

        s.mouse_move(110.0, 300.0); // 10px from the press point — starts
        assert_eq!(s.eval::<i64>("return drag_starts").unwrap(), 1);
        assert_eq!(
            s.eval::<String>("return drag_button").unwrap(),
            "LeftButton"
        );

        s.mouse_move(120.0, 300.0); // further movement doesn't re-fire
        assert_eq!(s.eval::<i64>("return drag_starts").unwrap(), 1);

        s.mouse_move(600.0, 300.0); // over B now
        let consumed = s.mouse_button(600.0, 300.0, "LeftButton", false); // release over B
        assert!(consumed);
        assert_eq!(
            s.eval::<i64>("return drag_stops").unwrap(),
            1,
            "OnDragStop on the source"
        );
        assert_eq!(
            s.eval::<i64>("return receives").unwrap(),
            1,
            "OnReceiveDrag on the target"
        );
        assert_eq!(
            s.eval::<i64>("return click_a").unwrap(),
            0,
            "no OnClick on the source"
        );
        assert_eq!(
            s.eval::<i64>("return click_b").unwrap(),
            0,
            "no OnClick on the target"
        );
        assert!(s.errors().is_empty(), "{:?}", s.errors());
    }

    #[test]
    fn drag_release_on_the_source_still_suppresses_onclick() {
        // Without the suppression, a same-frame press+release Up would normally fire OnClick
        // (a plain Frame's OnClick fires on any same-frame release — see `input.rs`'s click
        // tests) — proving the drag path takes precedence over that default.
        let mut s = drag_script();
        s.run(
            r#"
            clicks = 0
            local a = CreateFrame("Frame", "A")
            a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(400, 600); a:EnableMouse(true)
            a:RegisterForDrag("LeftButton")
            a:SetScript("OnClick", function(self) clicks = clicks + 1 end)
            "#,
        )
        .unwrap();
        s.resolve();

        s.mouse_button(100.0, 300.0, "LeftButton", true);
        s.mouse_move(110.0, 300.0); // starts the drag
        s.mouse_button(100.0, 300.0, "LeftButton", false); // released back on A
        assert_eq!(s.eval::<i64>("return clicks").unwrap(), 0);
        assert!(s.errors().is_empty(), "{:?}", s.errors());
    }

    #[test]
    fn drag_not_started_leaves_the_ordinary_click_path_untouched() {
        let mut s = drag_script();
        s.run(
            r#"
            clicks = 0
            local a = CreateFrame("Frame", "A")
            a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(400, 600); a:EnableMouse(true)
            a:RegisterForDrag("LeftButton")
            a:SetScript("OnClick", function(self) clicks = clicks + 1 end)
            "#,
        )
        .unwrap();
        s.resolve();

        s.mouse_button(100.0, 300.0, "LeftButton", true);
        s.mouse_move(101.0, 300.0); // 1px — never starts
        s.mouse_button(100.0, 300.0, "LeftButton", false);
        assert_eq!(
            s.eval::<i64>("return clicks").unwrap(),
            1,
            "a plain click still fires"
        );
        assert!(s.errors().is_empty(), "{:?}", s.errors());
    }

    /// A drag released over the world does NOTHING to the payload — no popup, no clear (decision
    /// 0218, byte-verified: `0x495300` runs on the WorldFrame click release only; a drag release
    /// routes as a drag, never a click — the director's "it should require an outside up+down").
    #[test]
    fn drag_release_over_nothing_keeps_carrying_no_popup() {
        let mut s = drag_script();
        s.run(
            r#"
            heard = 0
            local a = CreateFrame("Frame", "A")
            a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(400, 600); a:EnableMouse(true)
            a:RegisterForDrag("LeftButton")
            local f = CreateFrame("Frame", "Listener")
            f:RegisterEvent("DELETE_ITEM_CONFIRM")
            f:SetScript("OnEvent", function() heard = heard + 1 end)
            "#,
        )
        .unwrap();
        s.resolve();
        s.set_cursor_for_test(CursorPayload::Item(CursorItem {
            bag: 0,
            slot: 1,
            item_id: 117,
            texture: None,
            link: Some("|cffffffff|Hitem:117|h[Tough Jerky]|h|r".into()),
            count: None,
            quality: Some(3),
            equip_slots: Vec::new(),
        }));

        s.mouse_button(100.0, 300.0, "LeftButton", true); // press inside A
        s.mouse_move(110.0, 300.0); // starts the drag
        s.mouse_button(-50.0, -50.0, "LeftButton", false); // released over the world
        s.tick(0.01);
        assert_eq!(s.eval::<i64>("return heard").unwrap(), 0, "no popup");
        assert!(s.cursor_item().is_some(), "the payload keeps carrying");

        // The follow-up CLICK on the world (down + up, both over nothing) is the trigger.
        s.mouse_button(-50.0, -50.0, "LeftButton", true);
        assert_eq!(
            s.eval::<i64>("return heard").unwrap(),
            0,
            "the press alone is not the trigger"
        );
        let consumed = s.mouse_button(-50.0, -50.0, "LeftButton", false);
        assert!(consumed, "a world-drop click consumes the event");
        s.tick(0.01);
        assert_eq!(s.eval::<i64>("return heard").unwrap(), 1);
        assert!(
            s.cursor_item().is_some(),
            "the payload stays until the popup decides"
        );
        assert!(s.errors().is_empty(), "{:?}", s.errors());
    }

    #[test]
    fn world_click_over_nothing_drops_a_held_item() {
        let mut s = drag_script();
        s.run(
            r#"
            heard, name, quality = 0, nil, nil
            local f = CreateFrame("Frame", "Listener")
            f:RegisterEvent("DELETE_ITEM_CONFIRM")
            f:SetScript("OnEvent", function(self, event, n, q) heard = heard + 1; name = n; quality = q end)
            "#,
        )
        .unwrap();
        s.resolve();
        s.set_cursor_for_test(CursorPayload::Item(CursorItem {
            bag: 0,
            slot: 1,
            item_id: 117,
            texture: None,
            link: None, // no link ⇒ an empty name, not an error
            count: None,
            quality: None, // unknown quality ⇒ 0
            equip_slots: Vec::new(),
        }));

        s.mouse_button(-50.0, -50.0, "LeftButton", true);
        let consumed = s.mouse_button(-50.0, -50.0, "LeftButton", false); // the completed click
        assert!(consumed, "a world drop consumes the event");
        s.tick(0.01);
        assert_eq!(s.eval::<i64>("return heard").unwrap(), 1);
        assert_eq!(s.eval::<String>("return name").unwrap(), "");
        assert_eq!(s.eval::<i64>("return quality").unwrap(), 0);
        assert!(s.cursor_item().is_some(), "the payload stays held");
    }

    /// A world object (unit/GameObject) under the cursor suppresses the world drop entirely
    /// (decisions 0571 + 0574): the reference's object-leg dispatcher (`0x492ce0`) keeps every
    /// real payload and runs SELECT — no `DELETE_ITEM_CONFIRM`, payload untouched. The app
    /// feeds the pick (`set_world_pick`); tests/captures default `Nothing`.
    #[test]
    fn world_click_over_a_world_object_is_not_a_world_drop() {
        let mut s = drag_script();
        s.run(
            r#"
            heard = 0
            local f = CreateFrame("Frame", "Listener")
            f:RegisterEvent("DELETE_ITEM_CONFIRM")
            f:SetScript("OnEvent", function() heard = heard + 1 end)
            "#,
        )
        .unwrap();
        s.resolve();
        s.set_cursor_for_test(CursorPayload::Item(CursorItem {
            bag: 0,
            slot: 1,
            item_id: 117,
            texture: None,
            link: None,
            count: None,
            quality: None,
            equip_slots: Vec::new(),
        }));
        s.set_world_pick(crate::script::WorldPick::Object);

        s.mouse_button(-50.0, -50.0, "LeftButton", true);
        let consumed = s.mouse_button(-50.0, -50.0, "LeftButton", false);
        assert!(
            !consumed,
            "over an object the click is the world's, not a drop"
        );
        s.tick(0.01);
        assert_eq!(s.eval::<i64>("return heard").unwrap(), 0);
        assert!(s.cursor_item().is_some(), "the item payload survives");
    }

    /// A press over a FRAME whose release lands on the world is neither a click on the frame nor
    /// a world click — the payload is untouched.
    #[test]
    fn frame_press_world_release_is_not_a_world_drop() {
        let mut s = drag_script();
        s.run(
            r#"
            heard = 0
            local a = CreateFrame("Frame", "A")
            a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(400, 600); a:EnableMouse(true)
            local f = CreateFrame("Frame", "Listener")
            f:RegisterEvent("DELETE_ITEM_CONFIRM")
            f:SetScript("OnEvent", function() heard = heard + 1 end)
            "#,
        )
        .unwrap();
        s.resolve();
        s.set_cursor_for_test(CursorPayload::Item(CursorItem {
            bag: 0,
            slot: 1,
            item_id: 117,
            texture: None,
            link: None,
            count: None,
            quality: None,
            equip_slots: Vec::new(),
        }));

        s.mouse_button(100.0, 300.0, "LeftButton", true); // press on A (no RegisterForDrag)
        s.mouse_button(-50.0, -50.0, "LeftButton", false); // release over the world
        s.tick(0.01);
        assert_eq!(s.eval::<i64>("return heard").unwrap(), 0);
        assert!(s.cursor_item().is_some());
    }

    /// A spell/action payload world-CLICK clears silently (`0x495300`'s non-item arms); a drag
    /// release over the world keeps it carrying, same as an item.
    #[test]
    fn world_click_with_a_spell_payload_clears_silently() {
        let mut s = drag_script();
        s.run(
            r#"
            heard = 0
            local a = CreateFrame("Frame", "A")
            a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(400, 600); a:EnableMouse(true)
            a:RegisterForDrag("LeftButton")
            local f = CreateFrame("Frame", "Listener")
            f:RegisterEvent("DELETE_ITEM_CONFIRM")
            f:SetScript("OnEvent", function() heard = heard + 1 end)
            "#,
        )
        .unwrap();
        s.resolve();
        s.set_cursor_for_test(CursorPayload::Spell(crate::script::cursor::CursorSpell {
            book_slot: 1,
            book_type: "spell".into(),
            spell_id: 1,
            texture: None,
        }));

        s.mouse_button(100.0, 300.0, "LeftButton", true);
        s.mouse_move(110.0, 300.0);
        s.mouse_button(-50.0, -50.0, "LeftButton", false); // drag release: keeps carrying
        assert!(s.cursor_payload().is_some(), "a drag release never drops");

        s.mouse_button(-50.0, -50.0, "LeftButton", true);
        s.mouse_button(-50.0, -50.0, "LeftButton", false); // the world click clears
        s.tick(0.01);
        assert_eq!(
            s.eval::<i64>("return heard").unwrap(),
            0,
            "no delete popup for a spell"
        );
        assert!(s.cursor_payload().is_none(), "cleared silently");
    }
}
