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
fn slider_track_press_seats_the_thumb_and_a_disabled_slider_ignores_it() {
    let mut s = script();
    s.set_screen_size(1024.0, 768.0);
    s.run(
        r#"
        bar = CreateFrame("Slider", "SlTrack")
        bar:SetPoint("BOTTOMLEFT", nil, "BOTTOMLEFT", 100, 100)
        bar:SetSize(16, 100)
        bar:SetThumbTexture("Interface\\Buttons\\UI-ScrollBar-Knob")
        bar:GetThumbTexture():SetSize(16, 16)
        bar:SetMinMaxValues(0, 100)
        bar:SetValue(0)
        fired = {}
        bar:SetScript("OnValueChanged", function() table.insert(fired, arg1) end)
    "#,
    )
    .unwrap();
    s.resolve();

    // A press on the TRACK below the thumb (108, 120) — inside the slider, not on the thumb
    // [184,200] — SEATS the thumb's center under the cursor and fires OnValueChanged from the
    // press itself (0989's track-press law; 0250 §5 covered only the thumb grab). Track
    // y in [100, 200], travel 84: cursor 120 → thumb top 128 → fraction (200−128)/84 = 72/84.
    s.mouse_button(108.0, 120.0, "LeftButton", true);
    let v: f32 = s.eval("return SlTrack:GetValue()").unwrap();
    assert!(
        (v - 100.0 * (72.0 / 84.0)).abs() < 1e-3,
        "the press seats the thumb center at the cursor (got {v})"
    );
    let n: usize = s.eval("return table.getn(fired)").unwrap();
    assert_eq!(n, 1, "the jump fired OnValueChanged once, from the press");

    // The SAME press keeps dragging — the capture began at the track press.
    s.mouse_move(108.0, 108.0);
    let v: f32 = s.eval("return SlTrack:GetValue()").unwrap();
    assert_eq!(v, 100.0, "the gesture drags on without re-grabbing");
    s.mouse_button(108.0, 108.0, "LeftButton", false);

    // A disabled slider ignores a press anywhere — thumb and track alike.
    s.run(r#"SlTrack:SetValue(0); SlTrack:Disable(); fired = {}"#)
        .unwrap();
    for y in [192.0, 150.0] {
        s.mouse_button(108.0, y, "LeftButton", true);
        s.mouse_move(108.0, 130.0);
        let v: f32 = s.eval("return SlTrack:GetValue()").unwrap();
        assert_eq!(v, 0.0, "disabled slider does not move (press at y={y})");
        s.mouse_button(108.0, 130.0, "LeftButton", false);
    }
    let n: usize = s.eval("return table.getn(fired)").unwrap();
    assert_eq!(n, 0, "disabled: no OnValueChanged at all");
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

#[test]
fn slider_value_bits_gate_the_first_fire_and_the_range_reclamp() {
    // The client's `+0x314` bit1 (has a range) and bit2 (has a value), wow-re
    // `slider-mouse-law.md` §3/§6, byte-read off `SetValue 0x789930` / `SetMinMaxValues 0x7898f0`:
    // rangeless SetValue is a no-op; the FIRST SetValue always fires, even at the zero-init value;
    // SetMinMaxValues re-clamps through SetValue only once a value exists. Decision 2095 — Atlas's
    // option sliders raised from their own <OnLoad> because our SetMinMaxValues clamped the
    // zero-init value into (0.25, 1) and ran a handler the addon had not armed yet.
    let s = script();
    s.run(
        r#"
        seen = {}
        local sl = CreateFrame("Slider", "SlBits")
        sl:SetScript("OnValueChanged", function() table.insert(seen, arg1) end)

        sl:SetValue(7)                       -- bit1 clear: a complete no-op
        assert(sl:GetValue() == 0, "rangeless SetValue stores nothing: " .. sl:GetValue())
        assert(table.getn(seen) == 0, "rangeless SetValue fires nothing")

        sl:SetMinMaxValues(0.25, 1)          -- bit2 clear: the range excludes 0, still no fire
        assert(table.getn(seen) == 0, "SetMinMaxValues on a fresh slider fires nothing")
        assert(sl:GetValue() == 0, "and does not clamp a value that does not exist")

        sl:SetMinMaxValues(0, 10)
        sl:SetValue(0)                       -- first-ever value == zero-init: fires anyway
        assert(table.getn(seen) == 1 and seen[1] == 0, "the first SetValue always fires")
        sl:SetValue(0)                       -- now the change-gate holds
        assert(table.getn(seen) == 1, "an equal value after the first does not fire")

        sl:SetValue(8)
        sl:SetMinMaxValues(0, 5)             -- bit2 set: re-clamp 8 -> 5 fires
        assert(table.getn(seen) == 3 and seen[3] == 5, "a held value re-clamps and fires")
        sl:SetMinMaxValues(0, 20)            -- 5 stays 5: no fire
        assert(table.getn(seen) == 3, "a re-range that moves nothing fires nothing")
    "#,
    )
    .unwrap();
}

#[test]
fn slider_onload_range_does_not_run_an_unarmed_onvaluechanged() {
    // Atlas's exact shape (AtlasOptions.xml): the slider's <OnLoad> sets a range whose low end is
    // above zero, and its <OnValueChanged> indexes a saved-variables global that only exists after
    // ADDON_LOADED. In the reference the load raises nothing; before 2095 ours raised
    // "attempt to index global 'AtlasOptions' (a nil value)" out of the OnLoad.
    let mut s = script();
    let doc = crate::framexml::parse(
        r#"<Ui>
          <Slider name="SlAtlasAlpha">
            <Scripts>
              <OnLoad>this:SetMinMaxValues(0.25, 1); this:SetValueStep(0.05)</OnLoad>
              <OnValueChanged>SlAtlasOpts.alpha = this:GetValue()</OnValueChanged>
            </Scripts>
          </Slider>
        </Ui>"#,
    )
    .unwrap();
    let report = crate::loader::load(&s, &doc, &|_| None);
    assert!(
        report.errors.is_empty() && s.take_errors().is_empty(),
        "an OnLoad range on a fresh slider must not run OnValueChanged: {:?}",
        report.errors
    );
    s.run(
        r#"
        SlAtlasOpts = {}
        SlAtlasAlpha:SetValue(0.5)
        assert(SlAtlasOpts.alpha == 0.5, "the first real SetValue reaches the handler")
    "#,
    )
    .unwrap();
}

#[test]
fn a_thumb_with_no_authored_size_takes_its_arts_texel_span_and_still_drags() {
    // Dewdrop-2.0's `OpenSlider` (Cartographer 2.02 → Look 'n' Feel → Overlay transparency), in
    // its own construction order — the shape that has no `<ThumbTexture><Size>` anywhere:
    // `SetThumbTexture(path)` from Lua and nothing else. The reference asks the thumb for its own
    // `GetWidth`/`GetHeight` (`0x789ba0` → `[thumb_vt+0x1c]/[+0x20]` = `CSimpleTexture::0x770720`
    // /`0x770790`), which falls back to the art's native texel span — 32×32 for
    // `UI-SliderBar-Button-Vertical` — so the thumb is a 32-unit knob on a 128-unit track with
    // 96 units of travel. Ours read the *authored* size only and, finding none, sized the thumb to
    // the whole track: zero travel, a thumb smeared over the bar, and a drag that cannot move.
    let mut s = script();
    s.set_screen_size(1024.0, 768.0);
    s.set_texture_size_probe(Box::new(|p| {
        p.contains("UI-SliderBar-Button-Vertical")
            .then_some((32, 32))
    }));
    s.run(
        r#"
        -- The popout is a parentless FULLSCREEN_DIALOG frame that takes the mouse itself, with
        -- the slider one frame level above it — so this also pins that the press resolves to the
        -- Slider and not to the mouse-enabled host sitting under it.
        host = CreateFrame("Frame", "DdHost", nil)
        host:SetPoint("BOTTOMLEFT", nil, "BOTTOMLEFT", 100, 100)
        host:SetSize(80, 170)
        host:SetFrameStrata("FULLSCREEN_DIALOG")
        host:EnableMouse(true)
        bar = CreateFrame("Slider", "DdSlider", host)
        bar:SetFrameLevel(host:GetFrameLevel() + 1)
        bar:SetOrientation("VERTICAL")
        bar:SetMinMaxValues(0, 1)
        bar:SetValueStep(0.01)
        bar:SetValue(0.5)
        bar:SetWidth(16)
        bar:SetHeight(128)
        bar:SetPoint("LEFT", host, "LEFT", 15, 0)
        bar:SetThumbTexture("Interface\\Buttons\\UI-SliderBar-Button-Vertical")
        -- Dewdrop then seats the open value: overlayAlpha is 100 % of a 25 %..100 % range, and
        -- the popout inverts it (`1 - (value-min)/(max-min)`), so the engine value is 0 = the
        -- TOP of a vertical track.
        bar:SetValue(0)
    "#,
    )
    .unwrap();
    s.resolve();

    // host y ∈ [100, 270] ⇒ the bar is centred on y=185, 128 tall ⇒ track y ∈ [121, 249];
    // x: host left 100 + 15 ⇒ [115, 131], so the thumb centres on x=123.
    assert_eq!(
        thumb_rect(&s.extract(), "UI-SliderBar-Button-Vertical"),
        (107.0, 139.0, 217.0, 249.0),
        "a sizeless thumb is its art's 32×32, flush at the track top — not the whole 16×128 track"
    );

    // And it drags: grab the thumb at its centre and pull to the bottom of the track.
    s.mouse_button(123.0, 233.0, "LeftButton", true);
    s.mouse_move(123.0, 137.0);
    let v: f32 = s.eval("return DdSlider:GetValue()").unwrap();
    assert!(
        (v - 1.0).abs() < 1e-3,
        "travel is 128−32 = 96, so a 96-unit pull runs the value min→max (got {v})"
    );
}
