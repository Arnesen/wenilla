//! SetPoint + resolve → GetWidth/GetHeight + rect.

use super::common::script;
use crate::layout::Rect;

// Regression: an *explicit* `nil` relativeTo must still consume its argument slot, so the
// relativePoint + offsets that follow line up. The `SetPoint("P", nil, "P", x, y)` form is the
// common FrameXML idiom for "anchor to the screen at an offset"; a leading nil that failed to
// advance the cursor silently dropped the offsets (screen-anchored frames pinned to the corner).
#[test]
fn setpoint_explicit_nil_relative_to_keeps_offsets() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Nil")
        f:SetPoint("TOPLEFT", nil, "TOPLEFT", 40, -40)
        f:SetSize(300, 200)
    "#,
    )
    .unwrap();
    s.resolve();
    let rect = s
        .extract()
        .iter()
        .find_map(|q| match q.target {
            crate::order::ZTarget::Frame(_) => q.rect,
            _ => None,
        })
        .expect("resolved frame rect");
    // screen [0,0,600,800], TOPLEFT+(40,-40): left 40, top 560, size 300×200.
    assert_eq!(rect, Rect::new(360.0, 40.0, 560.0, 340.0));
}

#[test]
fn setpoint_resolve_size_and_rect() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0); // screen rect [bottom 0, left 0, top 600, right 800]
    s.run(
        r#"
        local f = CreateFrame("Frame", "Sized")
        f:SetPoint("TOPLEFT", 10, -5)   -- relativeTo = screen (default), relativePoint = TOPLEFT
        f:SetSize(200, 50)
    "#,
    )
    .unwrap();
    s.resolve();

    let (w, h): (f32, f32) = s
        .eval("return Sized:GetWidth(), Sized:GetHeight()")
        .unwrap();
    assert_eq!(w, 200.0);
    assert_eq!(h, 50.0);

    // Hand-computed (layout.md oracle): TOPLEFT anchored to screen [0,0,600,800] at (10,-5), size
    // 200×50 → Rect(bottom 545, left 10, top 595, right 210).
    let quads = s.extract();
    let frame_rect = quads
        .iter()
        .find_map(|q| match q.target {
            crate::order::ZTarget::Frame(_) => q.rect,
            _ => None,
        })
        .expect("resolved frame rect");
    assert_eq!(frame_rect, Rect::new(545.0, 10.0, 595.0, 210.0));
}

#[test]
fn getwidth_falls_back_to_explicit_size_before_resolve() {
    let s = script();
    // No SetPoint ⇒ unresolvable; GetWidth returns the explicit SetSize value.
    let w: f32 = s
        .eval(r#"local f = CreateFrame("Frame"); f:SetWidth(123); return f:GetWidth()"#)
        .unwrap();
    assert_eq!(w, 123.0);
}

// A *named* relativeTo that doesn't resolve falls back to the parent/owner — the client's
// behavior — but must SAY so: the silent version misdirected ItemTextFrame's scrollbar track
// onto the parchment (an XML forward reference; anchors resolve at SetPoint time). Both the
// frame and the region SetPoint paths warn.
#[test]
fn setpoint_unresolved_name_warns() {
    let mut s = script();
    s.run(
        r#"
        local f = CreateFrame("Frame", "Orphan")
        f:SetPoint("TOPLEFT", "NoSuchFrame", "TOPLEFT", 0, 0)
        local t = f:CreateTexture(nil, "ARTWORK")
        t:SetPoint("TOPRIGHT", "NoSuchRegion")
    "#,
    )
    .unwrap();
    let w = s.take_warnings();
    assert!(
        w.iter()
            .any(|w| w.contains("Orphan") && w.contains("NoSuchFrame")),
        "frame path: {w:#?}"
    );
    assert!(
        w.iter()
            .any(|w| w.contains("Orphan") && w.contains("NoSuchRegion")),
        "region path: {w:#?}"
    );
    // A resolvable name stays silent.
    s.run(r#"CreateFrame("Frame", "Target"); Orphan:SetPoint("BOTTOMLEFT", "Target", "TOPLEFT")"#)
        .unwrap();
    assert!(s.take_warnings().is_empty());
}

/// **A region whose OWNER has no rect still resolves from its own anchors.**
///
/// `layout.rs`'s region pass used to `continue` when `resolved` held nothing for the owner frame.
/// But the owner's rect is only the *fallback* for axes the region's own anchors do not pin — a
/// region anchored fully to some other frame needs nothing from its owner, and the reference
/// resolves it. Skipping made an ordinary addon shape silently invisible: a bare container frame
/// (`CreateFrame("Frame", n, parent)` with no size and no `SetPoint`) holding a region anchored
/// elsewhere.
///
/// Found by reproducing the director's MapCoords report. Its world-map readout computed the right
/// string every single frame — `Cursor Coords: … Player Coords: …` — and was never given a
/// position, with no error raised anywhere. Three of its four frames are this shape.
///
/// The control matters as much as the subject: a SIZED owner must be unaffected, or the fix has
/// traded one wrong layout for another.
#[test]
fn a_region_resolves_even_when_its_owner_frame_has_no_rect() {
    let mut s = crate::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    s.run(
        r#"
        host = CreateFrame("Frame", "AnchorHost")
        host:SetWidth(400) host:SetHeight(200)
        host:SetPoint("BOTTOMLEFT", 100, 50)

        -- the shape that was invisible: owner with NO size and NO SetPoint
        bare = CreateFrame("Frame", "BareOwner")
        mark = bare:CreateTexture("BareMark", "ARTWORK")
        mark:SetWidth(20) mark:SetHeight(10)
        mark:SetPoint("BOTTOMLEFT", host, "BOTTOMLEFT", 5, 7)

        -- control: identical region, owner that DOES resolve
        sized = CreateFrame("Frame", "SizedOwner")
        sized:SetWidth(10) sized:SetHeight(10) sized:SetPoint("CENTER")
        ctl = sized:CreateTexture("SizedMark", "ARTWORK")
        ctl:SetWidth(20) ctl:SetHeight(10)
        ctl:SetPoint("BOTTOMLEFT", host, "BOTTOMLEFT", 5, 7)
        "#,
    )
    .unwrap();
    s.resolve();

    // The owner genuinely has no rect — that part is correct and must stay true.
    assert_eq!(
        s.eval::<Option<f32>>("return BareOwner:GetLeft()").unwrap(),
        None,
        "an unpositioned frame has no rect; the fix must not invent one for it"
    );

    // ...and its region resolves anyway, from its own anchor: host's BOTTOMLEFT (100,50) + (5,7).
    assert_eq!(
        s.eval::<Option<f32>>("return BareMark:GetLeft()").unwrap(),
        Some(105.0),
        "a fully-anchored region needs nothing from its owner"
    );
    assert_eq!(
        s.eval::<Option<f32>>("return BareMark:GetBottom()")
            .unwrap(),
        Some(57.0)
    );
    // The control is unmoved.
    assert_eq!(
        s.eval::<Option<f32>>("return SizedMark:GetLeft()").unwrap(),
        Some(105.0),
        "a sized owner's region must be unaffected by the fix"
    );
}

/// **…but a region the owner's rect is the ONLY candidate for stays unresolved with it.**
///
/// The other half of the law above, and the half that shipped wrong: an unpinned axis on an
/// unpositioned owner has nothing to fall back to, so standing a zero rect in for the missing owner
/// puts the region at the **screen origin** instead of nowhere. That is not a degenerate rect, it is
/// a wrong position — and a template whose textures chain off each other turns it into real,
/// visible geometry a few links down.
///
/// Reported as B264 (carni, 2026-08-13): opening the social pane drew a stray dropdown capsule at
/// the bottom of the screen next to the action bar. `BenillaFriendsDropDown` carries no anchors —
/// *exactly* as the reference's own `FriendsDropDown` does (`FriendsFrame.xml` l.598), and the
/// reference draws nothing — so every texture of `UIDropDownMenuTemplate` hung off a phantom rect
/// at (0,0). The shape below is that template's first two textures verbatim.
#[test]
fn an_unanchored_owners_region_chain_resolves_nowhere() {
    let mut s = crate::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    s.run(
        r#"
        -- The dropdown host: a child frame with no anchors and no size, as declared.
        bare = CreateFrame("Frame", "StrayHost")

        -- $parentLeft: anchored to the OWNER (no relativeTo = the owner frame), which has no rect.
        cap = bare:CreateTexture("StrayLeft", "ARTWORK")
        cap:SetWidth(25) cap:SetHeight(64)
        cap:SetPoint("TOPLEFT")

        -- $parentMiddle: the sibling chain that turned a zero rect into 115x64 of visible capsule.
        mid = bare:CreateTexture("StrayMiddle", "ARTWORK")
        mid:SetWidth(115) mid:SetHeight(64)
        mid:SetPoint("LEFT", cap, "RIGHT")
        "#,
    )
    .unwrap();
    s.resolve();

    assert_eq!(
        s.eval::<Option<f32>>("return StrayHost:GetLeft()").unwrap(),
        None,
        "the premise: an unanchored frame has no rect"
    );
    assert_eq!(
        s.eval::<Option<f32>>("return StrayLeft:GetLeft()").unwrap(),
        None,
        "a region with nothing but its unpositioned owner to derive from has no rect either — \
         standing in a zero rect here is what put the capsule at the screen origin"
    );
    assert_eq!(
        s.eval::<Option<f32>>("return StrayMiddle:GetLeft()")
            .unwrap(),
        None,
        "and the chain off it stays unresolved — this is the link that was actually visible"
    );
}

/// **Geometry answers inside the same call stack that moved it** — the seam that killed 97 addons.
///
/// A handler creates a frame, anchors it, shows it and then measures it, all before any resolve
/// pass can run. `Dewdrop-2.0.lua` — embedded in ~65 corpus addons — does exactly this in its menu
/// `Open`: `local left = frame:GetLeft()` (l.1942) then `curX - left - width/2` (l.1960). Our
/// getters read a cache filled by the per-frame `resolve()`, so `left` was nil and the arithmetic
/// died. **97 of the 108 addons that drew and then raised on being touched died on that one line**,
/// with no missing verb anywhere.
///
/// The readers settle the graph on demand now. The second half of the test is the one that keeps
/// it honest: a *further* move inside the same stack must also be visible, or the fix is just a
/// one-shot warm-up.
#[test]
fn geometry_answers_within_the_call_stack_that_moved_it() {
    let mut s = crate::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);

    // Dewdrop's shape, in one eval — no resolve() between the writes and the read.
    let (left, bottom): (Option<f32>, Option<f32>) = s
        .eval(
            r#"
            local f = CreateFrame("Frame", "MenuLike", UIParent)
            f:SetWidth(100) f:SetHeight(50)
            f:SetPoint("BOTTOMLEFT", UIParent, "BOTTOMLEFT", 40, 60)
            f:Show()
            return f:GetLeft(), f:GetBottom()
            "#,
        )
        .unwrap();
    assert_eq!(
        (left, bottom),
        (Some(40.0), Some(60.0)),
        "a frame must report its geometry in the stack that anchored it"
    );

    // ...and a LATER move in the same stack is visible too — not a stale first answer.
    let moved: Option<f32> = s
        .eval(
            r#"
            MenuLike:ClearAllPoints()
            MenuLike:SetPoint("BOTTOMLEFT", UIParent, "BOTTOMLEFT", 200, 60)
            return MenuLike:GetLeft()
            "#,
        )
        .unwrap();
    assert_eq!(moved, Some(200.0), "the settle must re-run after each move");

    // Width/height and the centre reader take the same path.
    assert_eq!(
        s.eval::<(f32, f32)>("return MenuLike:GetWidth(), MenuLike:GetHeight()")
            .unwrap(),
        (100.0, 50.0)
    );
    assert_eq!(
        s.eval::<(Option<f32>, Option<f32>)>("return MenuLike:GetCenter()")
            .unwrap(),
        (Some(250.0), Some(85.0))
    );
}
