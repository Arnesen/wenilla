//! Slider (per-kind behavior; RF-28-grounded, decision 0250). The value/step/orientation contract,
//! the VERTICAL ctor default, the no-swap divergence from StatusBar, and the change-gate that keeps
//! the real scrollbar wiring from recursing.

use super::common::script;
use crate::script::QuadContent;

/// Find the thumb quad (the texture whose path carries `needle`) and return its
/// `(left, right, bottom, top)` rect. Panics if absent — the render must emit it.
fn thumb_rect(quads: &[crate::script::ExtractedQuad], needle: &str) -> (f32, f32, f32, f32) {
    let q = quads
        .iter()
        .find(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains(needle))
        })
        .expect("thumb quad");
    let r = q.rect.expect("thumb rect resolved");
    (r.left, r.right, r.bottom, r.top)
}

#[test]
fn slider_thumb_draws_at_value_fraction_along_the_track() {
    let mut s = script();
    s.set_screen_size(1024.0, 768.0);
    s.run(
        r#"
        local sl = CreateFrame("Slider", "SlRender")
        -- A vertical scrollbar: 16 wide, 100 tall, bottom-left at (100, 100) -> track y in [100, 200].
        sl:SetPoint("BOTTOMLEFT", nil, "BOTTOMLEFT", 100, 100)
        sl:SetSize(16, 100)
        sl:SetThumbTexture("Interface\\Buttons\\UI-ScrollBar-Knob")
        local t = sl:GetThumbTexture()
        t:SetSize(16, 16)
        sl:SetMinMaxValues(0, 100)
        sl:SetValue(0)
    "#,
    )
    .unwrap();
    s.resolve();
    // value=min: the thumb sits flush at the track TOP (a scrollbar at 0 is scrolled to the top).
    // travel = 100 - 16 = 84; thumb centered on cx=108 -> [100,116], top edge at track.top=200.
    assert_eq!(
        thumb_rect(&s.extract(), "Knob"),
        (100.0, 116.0, 184.0, 200.0),
        "value=min: thumb flush at track top, centered on the cross-axis"
    );

    // value=max: the thumb sits flush at the track BOTTOM.
    s.run(r#"SlRender:SetValue(100)"#).unwrap();
    assert_eq!(
        thumb_rect(&s.extract(), "Knob"),
        (100.0, 116.0, 100.0, 116.0),
        "value=max: thumb flush at track bottom"
    );

    // Halfway: thumb centered in the track (midpoint 150).
    s.run(r#"SlRender:SetValue(50)"#).unwrap();
    let (_, _, bottom, top) = thumb_rect(&s.extract(), "Knob");
    assert_eq!(
        (bottom, top),
        (142.0, 158.0),
        "value=mid: thumb centered in the track"
    );
}

#[test]
fn slider_thumb_drag_maps_cursor_to_value() {
    // The same vertical scrollbar as the render test: track y in [100, 200], 16x16 thumb. At value=0
    // the thumb spans y[184,200] (its top edge at the track top), centered on x=108.
    let mut s = script();
    s.set_screen_size(1024.0, 768.0);
    s.run(
        r#"
        bar = CreateFrame("Slider", "SlDrag")
        bar:SetPoint("BOTTOMLEFT", nil, "BOTTOMLEFT", 100, 100)
        bar:SetSize(16, 100)
        bar:SetThumbTexture("Interface\\Buttons\\UI-ScrollBar-Knob")
        bar:GetThumbTexture():SetSize(16, 16)
        bar:SetMinMaxValues(0, 100)
        bar:SetValue(0)
    "#,
    )
    .unwrap();
    s.resolve();

    // Grab the thumb at its center (108, 192) and drag down the track. travel = 84; the grab keeps
    // the same thumb point under the cursor, so the value tracks the cursor absolutely.
    s.mouse_button(108.0, 192.0, "LeftButton", true);
    s.mouse_move(108.0, 150.0);
    let v: f32 = s.eval("return SlDrag:GetValue()").unwrap();
    assert_eq!(v, 50.0, "cursor halfway down the track -> value 50");
    s.mouse_move(108.0, 108.0);
    let v: f32 = s.eval("return SlDrag:GetValue()").unwrap();
    assert_eq!(
        v, 100.0,
        "cursor at the track bottom -> value 100 (clamped)"
    );

    // Release ends the capture: a later move must not move the value.
    s.mouse_button(108.0, 108.0, "LeftButton", false);
    s.mouse_move(108.0, 192.0);
    let v: f32 = s.eval("return SlDrag:GetValue()").unwrap();
    assert_eq!(v, 100.0, "no capture after release");
}

#[test]
fn slider_track_click_and_disabled_thumb_do_not_drag() {
    let mut s = script();
    s.set_screen_size(1024.0, 768.0);
    s.run(
        r#"
        bar = CreateFrame("Slider", "SlNoDrag")
        bar:SetPoint("BOTTOMLEFT", nil, "BOTTOMLEFT", 100, 100)
        bar:SetSize(16, 100)
        bar:SetThumbTexture("Interface\\Buttons\\UI-ScrollBar-Knob")
        bar:GetThumbTexture():SetSize(16, 16)
        bar:SetMinMaxValues(0, 100)
        bar:SetValue(0)
    "#,
    )
    .unwrap();
    s.resolve();

    // A press on the TRACK below the thumb (108, 120) — inside the slider but not on the thumb
    // [184,200] — captures nothing: only the thumb drags (the arrows step, decision 0250).
    s.mouse_button(108.0, 120.0, "LeftButton", true);
    s.mouse_move(108.0, 150.0);
    let v: f32 = s.eval("return SlNoDrag:GetValue()").unwrap();
    assert_eq!(v, 0.0, "track click does not drag");
    s.mouse_button(108.0, 150.0, "LeftButton", false);

    // A disabled slider ignores a press even on the thumb.
    s.run(r#"SlNoDrag:Disable()"#).unwrap();
    s.mouse_button(108.0, 192.0, "LeftButton", true);
    s.mouse_move(108.0, 150.0);
    let v: f32 = s.eval("return SlNoDrag:GetValue()").unwrap();
    assert_eq!(v, 0.0, "disabled slider does not drag");
}

#[test]
fn slider_methods_exist_only_on_sliders() {
    let s = script();
    let ok: bool = s
        .eval(
            r#"
        local sl = CreateFrame("Slider", "SlDuck")
        local plain = CreateFrame("Frame")
        -- Duck-typing: addons branch on `if frame.SetValueStep then` — a plain frame must say nil.
        return (type(sl.SetValue) == "function") and (plain.SetValue == nil)
            and (type(sl.SetValueStep) == "function") and (plain.SetValueStep == nil)
            and (type(sl.SetThumbTexture) == "function") and (plain.SetThumbTexture == nil)
            and (type(sl.Show) == "function") -- base methods still reachable through the fallback
    "#,
        )
        .unwrap();
    assert!(ok);
}

#[test]
fn slider_default_orientation_is_vertical() {
    // The load-bearing fact of decision 0250: every scrollbar omits `orientation` and is vertical,
    // so the CSimpleSlider ctor default is VERTICAL — the opposite of StatusBar's HORIZONTAL.
    let s = script();
    s.run(
        r#"
        local sl = CreateFrame("Slider", "SlOrient")
        assert(sl:GetOrientation() == "VERTICAL", "ctor default is VERTICAL")
        sl:SetOrientation("HORIZONTAL")
        assert(sl:GetOrientation() == "HORIZONTAL", "explicit horizontal takes")
    "#,
    )
    .unwrap();
}

#[test]
fn slider_value_clamps_and_does_not_swap_minmax() {
    let s = script();
    s.run(
        r#"
        local sl = CreateFrame("Slider", "SlClamp")
        sl:SetMinMaxValues(0, 100)
        sl:SetValueStep(5)
        assert(sl:GetValueStep() == 5, "step round-trips")
        sl:SetValue(250)
        assert(sl:GetValue() == 100, "clamped to max")
        sl:SetValue(-5)
        assert(sl:GetValue() == 0, "clamped to min")
        -- Unlike StatusBar, a reversed pair is NOT swapped (Slider LoadXML stores min + range, RF-28).
        sl:SetMinMaxValues(80, 20)
        local mn, mx = sl:GetMinMaxValues()
        assert(mn == 80 and mx == 20, "reversed pair kept as given, not swapped")
    "#,
    )
    .unwrap();
}

#[test]
fn slider_setvalue_fires_onvaluechanged_only_on_change() {
    let s = script();
    s.run(
        r#"
        local sl = CreateFrame("Slider", "SlEvt")
        sl:SetMinMaxValues(0, 10)
        seen = {}
        sl:SetScript("OnValueChanged", function(self, value)
            table.insert(seen, value)
            assert(self == sl and arg1 == value, "RF-0025 conventions carry the value")
        end)
        sl:SetValue(4)
        sl:SetValue(4)          -- no change, no fire
        sl:SetMinMaxValues(0, 3) -- re-clamp 4 -> 3: a value change, fires
        assert(table.getn(seen) == 2 and seen[1] == 4 and seen[2] == 3,
               "fired once per actual change: " .. table.getn(seen))
    "#,
    )
    .unwrap();
}

#[test]
fn slider_enable_disable_roundtrips() {
    let s = script();
    s.run(
        r#"
        local sl = CreateFrame("Slider", "SlEnable")
        assert(sl:IsEnabled(), "enabled by ctor")
        sl:Disable()
        assert(not sl:IsEnabled(), "disabled")
        sl:Enable()
        assert(sl:IsEnabled(), "re-enabled")
    "#,
    )
    .unwrap();
}

#[test]
fn slider_get_thumb_texture_returns_a_stable_region() {
    let s = script();
    s.run(
        r#"
        local sl = CreateFrame("Slider", "SlThumb")
        assert(sl:GetThumbTexture() == nil, "no thumb before one is set")
        sl:SetThumbTexture("Interface\\Buttons\\UI-ScrollBar-Knob")
        local t = sl:GetThumbTexture()
        assert(t ~= nil and t == sl:GetThumbTexture(), "stable region wrapper")
    "#,
    )
    .unwrap();
}

#[test]
fn slider_scrollbar_wiring_does_not_recurse() {
    // The real FauxScrollFrame wiring (UIPanelTemplates.xml): the scrollbar Slider's OnValueChanged
    // drives the ScrollFrame's SetVerticalScroll, whose OnVerticalScroll drives the Slider's
    // SetValue back the other way. A fire-always SetValue would recurse forever; the change-gate
    // (SliderState::store_value returns Some only on an actual change) breaks the loop after one hop.
    // This test *is* the recursion — if the gate regressed it would stack-overflow, not assert-fail.
    let mut s = script();
    s.set_screen_size(1024.0, 768.0);
    s.run(
        r#"
        local sf = CreateFrame("ScrollFrame", "SlSF")
        sf:SetPoint("TOPLEFT", nil, "TOPLEFT", 0, 0)
        sf:SetSize(100, 100)
        local child = CreateFrame("Frame", "SlSFChild", sf)
        child:SetSize(100, 300)         -- 200px taller than the frame -> scroll range 200
        sf:SetScrollChild(child)

        bar = CreateFrame("Slider", "SlSFBar")
        bar:SetMinMaxValues(0, 200)
        bar:SetScript("OnValueChanged", function() sf:SetVerticalScroll(arg1) end)
        sf:SetScript("OnVerticalScroll", function() bar:SetValue(arg1) end)
    "#,
    )
    .unwrap();
    // A resolve populates the rects SetVerticalScroll's live range clamps against.
    s.resolve();
    s.run(
        r#"
        bar:SetValue(50)
        assert(bar:GetValue() == 50, "slider settled at 50")
        assert(SlSF:GetVerticalScroll() == 50, "scroll followed the slider")
    "#,
    )
    .unwrap();
}
