//! Regions join the anchor layout (decision 0068).
//!
//! The smeared-merchant root cause: region `<Size>`/`<Anchors>` used to be dropped — a region either
//! filled its owner or (with a size) drew centered, and its anchors were ignored. Regions now resolve
//! through the same leaf math as frames, owner-relative, inheriting any edge their anchors don't pin.

use super::common::script;
use crate::layout::Rect;
use crate::script::*;

/// A texture region's resolved rect, found by (a fragment of) its texture path.
fn region_tex_rect(s: &UiScript, needle: &str) -> Rect {
    s.extract()
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Texture { path: Some(p), .. } if p.contains(needle) => q.rect,
            _ => None,
        })
        .unwrap_or_else(|| panic!("no texture region rect for {needle}"))
}

/// A fontstring region's resolved rect, found by its exact text.
fn region_text_rect(s: &UiScript, text: &str) -> Rect {
    s.extract()
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Text { text: Some(t), .. } if t == text => q.rect,
            _ => None,
        })
        .unwrap_or_else(|| panic!("no text region rect for {text:?}"))
}

#[test]
fn region_texture_anchored_topleft_resolves_exact_rect() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Owner")
        f:SetPoint("BOTTOMLEFT", 0, 0)   -- owner [bottom 0, left 0, top 50, right 100]
        f:SetSize(100, 50)
        local t = f:CreateTexture("Tex", "ARTWORK")
        t:SetTexture("Interface\\Icon")
        t:SetSize(24, 24)
        t:SetPoint("TOPLEFT", 4, -4)
    "#,
    )
    .unwrap();
    s.resolve();
    // TOPLEFT +(4,-4), 24×24 in the owner: left 4, top 46, right 28, bottom 22.
    assert_eq!(
        region_tex_rect(&s, "Interface\\Icon"),
        Rect::new(22.0, 4.0, 46.0, 28.0)
    );
}

/// A FontString's implicit extent is its measured TEXT — an unpinned edge COLLAPSES onto its
/// pinned opposite while the measure is pending (empty text never measures at all), never onto
/// the owner's edge; only an axis with NO pinned edge keeps the v1 owner fallback. (The old
/// owner-edge inheritance stretched an empty tooltip line to the plate's bottom and marched the
/// line chain out of the plate — the live NPC-tooltip spill.) Textures keep the owner fallback:
/// [`region_texture_anchored_topleft_resolves_exact_rect`]'s sized leg is unaffected either way.
#[test]
fn region_fontstring_unpinned_edge_collapses_until_measured() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Owner")
        f:SetPoint("BOTTOMLEFT", 0, 0)   -- owner [0, 0, 50, 100]
        f:SetSize(100, 50)
        local fs = f:CreateFontString("FS", "ARTWORK")
        fs:SetText("Name")
        fs:SetJustifyH("LEFT")
        fs:SetPoint("LEFT", 5, 0)        -- left edge pinned; no size
    "#,
    )
    .unwrap();
    s.resolve();
    // Pending measure: x collapses onto the pinned left (5); the y-axis has no pinned EDGE
    // (a LEFT point pins the y-CENTER only), so it keeps the owner fallback.
    assert_eq!(region_text_rect(&s, "Name"), Rect::new(0.0, 5.0, 50.0, 5.0));
    // Measured, the rect is the text extent seated on the anchor: 40×12 around y-center 25.
    let answers: Vec<(u32, f32, f32, u64)> = s
        .fontstrings_needing_measure()
        .iter()
        .map(|r| (r.id, 40.0, 12.0, r.key))
        .collect();
    s.set_measured_text_unwrapped(&answers);
    s.resolve();
    assert_eq!(
        region_text_rect(&s, "Name"),
        Rect::new(19.0, 5.0, 31.0, 45.0)
    );
}

#[test]
fn region_sized_no_anchor_centers_on_owner() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Owner")
        f:SetPoint("BOTTOMLEFT", 0, 0)   -- owner [0, 0, 50, 100]
        f:SetSize(100, 50)
        local t = f:CreateTexture("Tex", "ARTWORK")
        t:SetTexture("Interface\\Ring")
        t:SetSize(24, 24)                -- no anchors ⇒ centered on the owner
    "#,
    )
    .unwrap();
    s.resolve();
    // center (50, 25), 24×24 → [bottom 13, left 38, top 37, right 62].
    assert_eq!(
        region_tex_rect(&s, "Interface\\Ring"),
        Rect::new(13.0, 38.0, 37.0, 62.0)
    );
}

#[test]
fn region_set_all_points_fills_owner() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Owner")
        f:SetPoint("BOTTOMLEFT", 0, 0)   -- owner [0, 0, 50, 100]
        f:SetSize(100, 50)
        local t = f:CreateTexture("Tex", "ARTWORK")
        t:SetTexture("Interface\\Fill")
        t:SetSize(24, 24)                -- size present, but setAllPoints wins
        t:SetAllPoints()
    "#,
    )
    .unwrap();
    s.resolve();
    assert_eq!(
        region_tex_rect(&s, "Interface\\Fill"),
        Rect::new(0.0, 0.0, 50.0, 100.0)
    );
}

/// A region anchored to a **sibling region by name** resolves against the sibling's rect — the
/// merchant label-plate shape (`plate LEFT → $parentSlot RIGHT −9`), which the old owner-fallback
/// mis-anchored to the row's right edge (the jutting-plates bug the director's A/B caught). The
/// plate is declared BEFORE the slot so only the resolve fixpoint (not declaration order) can
/// order the chain; the slot itself is region-anchored to the row, one deeper than owner-direct.
#[test]
fn region_anchors_to_sibling_region_by_name() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local row = CreateFrame("Button", "Row")
        row:SetPoint("BOTTOMLEFT", 100, 100); row:SetSize(153, 44)
        -- Plate first: its target doesn't exist yet at SetPoint time in XML order terms; the
        -- name lookup happens at SetPoint (both exist by then in real loads), the RECT ordering
        -- is the fixpoint's job.
        local plate = row:CreateTexture("RowPlate", "BACKGROUND")
        plate:SetSize(128, 78)
        local slot = row:CreateTexture("RowSlot", "BACKGROUND")
        slot:SetSize(64, 64)
        slot:SetPoint("TOPLEFT", "Row", "TOPLEFT", -13, 13)
        plate:SetPoint("LEFT", "RowSlot", "RIGHT", -9, -18)
    "#,
    )
    .unwrap();
    s.resolve();
    let quads = s.extract();
    let rect = |name: &str| {
        quads
            .iter()
            .find(|q| {
                matches!(&q.content, QuadContent::Texture { path, .. } if path.is_none())
                    && q.rect.is_some_and(|r| {
                        (r.width() - if name == "RowSlot" { 64.0 } else { 128.0 }).abs() < 0.1
                    })
            })
            .and_then(|q| q.rect)
            .unwrap_or_else(|| panic!("no rect for {name}"))
    };
    // Slot: row TOPLEFT (100..253, 100..144) → slot at (87, 157)-(151, 93)… y-up: top = 144+13 = 157.
    let slot = rect("RowSlot");
    assert_eq!(
        (slot.left, slot.top),
        (87.0, 157.0),
        "slot at row TOPLEFT (-13,13)"
    );
    assert_eq!(slot.right, 151.0);
    // Plate LEFT anchors to the SLOT's RIGHT (151) − 9 = 142 — NOT the row's right edge (253),
    // which is where the old owner-fallback shoved it (253 − 9 + 128 = jutting past everything).
    let plate = rect("RowPlate");
    assert_eq!(plate.left, 142.0, "plate.LEFT = slot.RIGHT − 9");
    assert_eq!(
        plate.right, 270.0,
        "plate spans its 128 width from the slot edge"
    );
    // The LEFT single-point anchor centers the plate vertically on the target point (y-up):
    // slot center-y = (93+157)/2 = 125, −18 → 107; ±39.
    assert_eq!((plate.bottom, plate.top), (68.0, 146.0));
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// `SetPortraitTexture(region, unit)` (the live model-bake portrait) binds a Texture region to a unit
/// token, carried out through [`QuadContent::Texture::portrait_unit`] with no BLP path/color of its
/// own (the app supplies the off-screen bake). A later `SetTexture` makes it an ordinary texture again,
/// dropping the binding.
#[test]
fn set_portrait_texture_binds_unit_token_then_settexture_clears_it() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "PFrame")
        f:SetPoint("TOPLEFT", 0, 0)
        f:SetSize(100, 100)
        local p = f:CreateTexture("PFramePortrait", "BACKGROUND")
        p:SetSize(64, 64)
        p:SetPoint("TOPLEFT", 0, 0)
        SetPortraitTexture(p, "player")
    "#,
    )
    .unwrap();
    s.resolve();
    let bound = s.extract().into_iter().find_map(|q| match q.content {
        QuadContent::Texture {
            portrait_unit: Some(u),
            path,
            color,
            circular,
            ..
        } => Some((u, path, color, circular)),
        _ => None,
    });
    let (unit, path, color, circular) = bound.expect("a portrait-bound quad is extracted");
    assert_eq!(unit, "player");
    assert_eq!(path, None, "the model bake carries no BLP path");
    assert_eq!(color, None, "the model bake carries no vertex color");
    assert!(circular, "the frame-ring portrait is the round stencil");

    // SetTexture reverts the region to an ordinary texture — the live-unit binding drops.
    s.run(r#" PFramePortrait:SetTexture("Interface\\Icons\\INV_Misc_QuestionMark") "#)
        .unwrap();
    s.resolve();
    assert!(
        !s.extract().iter().any(|q| matches!(
            &q.content,
            QuadContent::Texture {
                portrait_unit: Some(_),
                ..
            }
        )),
        "SetTexture clears the live-unit portrait binding"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// `BenillaSetBoothTexture(region, token)` — the paper doll's **square** booth binding (decision
/// 0208 §5): the same `portrait_unit` carriage as `SetPortraitTexture`, but `circular` stays
/// false (the model pane samples the body bake edge to edge, no frame ring to mask for).
#[test]
fn benilla_set_booth_texture_binds_square() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "DollFrame")
        f:SetPoint("TOPLEFT", 0, 0)
        f:SetSize(200, 300)
        local m = f:CreateTexture("DollFrameModel", "ARTWORK")
        m:SetAllPoints()
        BenillaSetBoothTexture(m, "paperdoll")
    "#,
    )
    .unwrap();
    s.resolve();
    let bound = s.extract().into_iter().find_map(|q| match q.content {
        QuadContent::Texture {
            portrait_unit: Some(u),
            circular,
            ..
        } => Some((u, circular)),
        _ => None,
    });
    let (token, circular) = bound.expect("a booth-bound quad is extracted");
    assert_eq!(token, "paperdoll");
    assert!(
        !circular,
        "the booth pane draws square — no inscribed-circle mask"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// The quest-log detail pane's shape (the 0109 look fix): a LONG region anchor chain — the real
/// client's QuestLogFrame chains ~15 regions deep (title → objectives → obj1..10 → description →
/// rewards) — with a frame (the reward item button) anchored to the chain's tail. The resolver
/// must run to a true fixpoint: the old bounded rounds (2 frame × 3 region passes) left links past
/// ~6 on silent owner-edge fallbacks and dropped the tail-anchored FRAME to the screen origin —
/// decision 0088 §2's "button anchored to a chained region falls to the screen origin" finding,
/// which forced every window into invented fixed offsets instead of ref-verbatim chains.
#[test]
fn long_region_chain_resolves_and_a_frame_binds_to_its_tail() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Book")
        f:SetPoint("TOPLEFT", 0, 0)
        f:SetSize(384, 512)
        for i = 1, 12 do
            local t = f:CreateTexture("Line" .. i, "ARTWORK")
            t:SetTexture("Interface\\Line" .. i)
            t:SetSize(300, 10)
            if i == 1 then
                t:SetPoint("TOPLEFT", "Book", "TOPLEFT", 5, -5)
            else
                t:SetPoint("TOPLEFT", "Line" .. (i - 1), "BOTTOMLEFT", 0, -2)
            end
        end
        local b = CreateFrame("Button", "TailButton", f)
        b:SetSize(147, 41)
        b:SetPoint("TOPLEFT", "Line12", "BOTTOMLEFT", 0, -6)
    "#,
    )
    .unwrap();
    s.resolve();
    // Screen top 600; Line1 top = 600−5 = 595; each link steps 12 (10 height + 2 gap).
    for i in 1..=12u32 {
        let top = 595.0 - 12.0 * (i as f32 - 1.0);
        assert_eq!(
            region_tex_rect(&s, &format!("Interface\\Line{i}")),
            Rect::new(top - 10.0, 5.0, top, 305.0),
            "link {i} must resolve at its true chain position, not an owner-edge fallback"
        );
    }
    // The tail frame: TOPLEFT = Line12's BOTTOMLEFT (5, 453) − 6 → top 447, spans 147×41.
    let button = s
        .extract()
        .iter()
        .find_map(|q| match q.target {
            crate::order::ZTarget::Frame(_) => q.rect.filter(|r| (r.width() - 147.0).abs() < 0.1),
            _ => None,
        })
        .expect("the tail-anchored button resolved a rect (not dropped to origin)");
    assert_eq!(
        button,
        Rect::new(406.0, 5.0, 447.0, 152.0),
        "the frame binds to the CHAIN TAIL's resolved rect"
    );
}

/// Decision 0088 §2 pinned "a child frame shown at runtime does not draw its own `<Layers>`
/// FontStrings" — the engine constraint that forced every window FLAT. Re-tested after the
/// resolver fixpoint (decision 0112): a child frame created and SHOWN at runtime, carrying its own
/// text + texture regions, must extract both quads at the child's resolved position.
#[test]
fn child_frame_layers_regions_render_after_the_fixpoint() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local parent = CreateFrame("Frame", "Win")
        parent:SetPoint("TOPLEFT", 10, -10)
        parent:SetSize(400, 300)
        local child = CreateFrame("Frame", "SubPanel", parent)
        child:SetPoint("TOPLEFT", "Win", "TOPLEFT", 20, -20)
        child:SetSize(200, 100)
        local fs = child:CreateFontString("SubText", "ARTWORK")
        fs:SetPoint("TOPLEFT", "SubPanel", "TOPLEFT", 5, -5)
        fs:SetSize(150, 12)
        fs:SetText("hello from the child")
        local tex = child:CreateTexture("SubTex", "BACKGROUND")
        tex:SetTexture("Interface\\SubFill")
        tex:SetAllPoints()
        child:Hide()
        child:Show()
    "#,
    )
    .unwrap();
    s.resolve();
    let quads = s.extract();
    let text = quads.iter().find(|q| {
        matches!(&q.content, crate::script::QuadContent::Text { text: Some(t), .. } if t == "hello from the child")
    });
    assert!(
        text.is_some_and(|q| q.rect.is_some_and(|r| (r.left - 35.0).abs() < 0.5)),
        "the child frame's own FontString must extract at its resolved spot (got {:?})",
        text.and_then(|q| q.rect)
    );
    let tex = quads.iter().find(|q| {
        matches!(&q.content, crate::script::QuadContent::Texture { path: Some(p), .. } if p == "Interface\\SubFill")
    });
    assert!(
        tex.is_some_and(|q| q.rect.is_some()),
        "the child's texture too"
    );
}

/// `SetAlpha`/`GetAlpha` on a Texture/FontString — the region's *own* alpha, distinct from its
/// owner frame's. A region draws at `ownAlpha × ownerFrame.alpha`: a single hop to the immediate
/// owner (wow-re `propagation.md` — frame SetAlpha overwrite-cascades onto child *frames* and only
/// invalidates child regions). The getter must return the region's value, never the frame's: the
/// ref kit ramps a texture by reading it back (`CastingBarFlash:SetAlpha(GetAlpha() + step)`).
#[test]
fn region_alpha_is_its_own_and_multiplies_the_owner_frames() {
    let mut s = script();
    s.set_screen_size(1024.0, 768.0);
    s.run(
        r#"
        f = CreateFrame("Frame", "AlphaOwner")
        f:SetPoint("BOTTOMLEFT", nil, "BOTTOMLEFT", 0, 0)
        f:SetSize(10, 10)
        tex = f:CreateTexture("AlphaTex", "ARTWORK")
        tex:SetTexture("Interface\\Foo")
        assert(tex:GetAlpha() == 1, "an untouched region is opaque")
    "#,
    )
    .unwrap();
    s.resolve();

    let quad_alpha = |s: &UiScript| {
        s.extract()
            .iter()
            .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(_), .. }))
            .expect("texture quad")
            .alpha
    };
    assert_eq!(quad_alpha(&s), 1.0);

    // The region's own alpha alone.
    s.run("tex:SetAlpha(0.5)").unwrap();
    assert_eq!(s.eval::<f32>("return tex:GetAlpha()").unwrap(), 0.5);
    assert_eq!(quad_alpha(&s), 0.5);

    // …times the owner frame's. The frame's SetAlpha does NOT overwrite the region's own value.
    s.run("f:SetAlpha(0.5)").unwrap();
    assert_eq!(
        s.eval::<f32>("return tex:GetAlpha()").unwrap(),
        0.5,
        "the frame's SetAlpha leaves the region's own alpha alone"
    );
    assert_eq!(quad_alpha(&s), 0.25, "0.5 region × 0.5 frame");

    // A hidden region draws nothing regardless of alpha.
    s.run("tex:SetAlpha(1); tex:Hide()").unwrap();
    assert!(
        !s.extract()
            .iter()
            .any(|q| matches!(&q.content, QuadContent::Texture { path: Some(_), .. })),
        "a hidden region emits no quad"
    );
}

/// **The texture-colour composition law** (wow-re `system/ui/scratch/texture-color-composition.md`,
/// VERIFIED): a region's own solid colour is a real *texel* (`SetTexture(r,g,b,a)` generates an 8×8
/// block at `+0xcc`), its vertex colour is a *separate* slot (`+0xb8`), and the draw
/// **multiplies** them per channel, alpha included — it does not replace.
///
/// This is the reference `SkillFrame` row trough, verbatim: declared `<Color 1,1,1,0.2>`, then
/// `SetVertexColor(0, 0, 0.75, 0.5)`'d. It draws at alpha `0.2 × 0.5 = 0.1`. benilla stored ONE
/// colour slot until this test, which made the second call replace the first and drew it at 0.5.
#[test]
fn a_solid_colour_texel_multiplies_with_the_vertex_colour() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Owner")
        f:SetPoint("BOTTOMLEFT", 0, 0)
        f:SetWidth(100) f:SetHeight(50)
        trough = f:CreateTexture("Trough", "BACKGROUND")
        trough:SetTexture(1, 1, 1, 0.2)
    "#,
    )
    .unwrap();
    s.resolve();

    let solid = |s: &UiScript| {
        s.extract()
            .iter()
            .find_map(|q| match &q.content {
                QuadContent::Texture {
                    path: None, color, ..
                } => Some(*color),
                _ => None,
            })
            .expect("solid-colour quad")
    };
    assert_eq!(
        solid(&s),
        Some([1.0, 1.0, 1.0, 0.2]),
        "untinted, the texel draws as declared"
    );

    s.run("trough:SetVertexColor(0, 0, 0.75, 0.5)").unwrap();
    assert_eq!(
        solid(&s),
        Some([0.0, 0.0, 0.75, 0.1]),
        "texel x vertex, alpha included: 0.2 x 0.5 = 0.1, NOT 0.5"
    );
    // The API readback is the vertex slot itself, not the product — the two are distinct storage.
    assert_eq!(
        s.eval::<(f32, f32, f32, f32)>("return trough:GetVertexColor()")
            .unwrap(),
        (0.0, 0.0, 0.75, 0.5)
    );

    // Art and a solid colour share the `+0xcc` slot: setting a path releases the generated texel,
    // and what is left is the tint alone for the renderer to modulate the sample by.
    s.run(r#"trough:SetTexture("Interface\\Bar.blp")"#).unwrap();
    let tinted = s
        .extract()
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Texture {
                path: Some(p),
                color,
                ..
            } if p.contains("Bar") => Some(*color),
            _ => None,
        })
        .expect("art quad");
    assert_eq!(
        tinted,
        Some([0.0, 0.0, 0.75, 0.5]),
        "the vertex colour outlives the texel it was multiplying"
    );
}

/// The draw gate is the TEXTURE slot, never the colour (`texture-color-composition.md` §4,
/// VERIFIED): `0x7706e0` tests `+0xcc` and emits NOTHING when it is empty, whatever the vertex
/// colour holds. Since the tint deliberately survives `SetTexture(nil)` ("a tint outlives the art
/// it was tinting"), a cleared region used to leak its tint out of extract as a solid plate — an
/// occupied action button going empty on a character switch drew its surviving 1/1/1 usable-tint
/// as a solid WHITE square (decision 1108; the 2026-07-10 grey wells were the same class).
#[test]
fn a_vertex_colour_without_a_texture_draws_nothing() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Owner")
        f:SetPoint("BOTTOMLEFT", 0, 0)
        f:SetWidth(100) f:SetHeight(50)
        icon = f:CreateTexture("Icon", "ARTWORK")
        icon:SetTexture("Interface\\Icons\\Spell_Nature_Sleep")
        icon:SetVertexColor(1, 1, 1)
    "#,
    )
    .unwrap();
    s.resolve();
    let drawn = |s: &UiScript| {
        s.extract()
            .iter()
            .filter(|q| {
                matches!(&q.content, QuadContent::Texture { path, color, .. }
                    if path.is_some() || color.is_some())
            })
            .count()
    };
    assert_eq!(drawn(&s), 1, "tinted art draws");

    // The slot empties: the art clears, the tint stays (distinct storage) — and nothing draws.
    s.run("icon:SetTexture(nil)").unwrap();
    assert_eq!(
        s.eval::<(f32, f32, f32, f32)>("return icon:GetVertexColor()")
            .unwrap(),
        (1.0, 1.0, 1.0, 1.0),
        "the tint survives the clear"
    );
    assert_eq!(
        drawn(&s),
        0,
        "no texture at +0xcc -> emit NOTHING, never a solid plate of the surviving tint"
    );
}

/// **`SetTexture` ignores arguments past the path**, because the client's does.
///
/// The path form reads ONE argument (`0x770200`); only the colour form (`0x770360`) reads up to
/// four, and a C function takes what it wants off the Lua stack and ignores the rest. We typed the
/// trailing three as `Option<f32>` and so RAISED on a stray extra — `bad argument #3: error
/// converting Lua boolean to f32` — where the client silently accepts.
///
/// The live case is `_LazyPig/LazyPigMenu.lua:182`:
/// `texture_title:SetTexture("Interface\DialogFrame\UI-DialogBox-Header", true)`. The `true` is
/// meaningless in 1.12; the addon reached us only once the survey began seating the addon registry.
///
/// Asserted as "does not raise", which is the whole claim — there is no `GetTexture` binding to read
/// the path back through, and inventing one to satisfy a test would be the tail wagging the dog.
/// The colour form is exercised alongside so the fix cannot have made it lax: a non-numeric channel
/// must take the same default a missing one does, exactly as `lua_tonumber` of a non-number is 0.
#[test]
fn set_texture_ignores_arguments_past_the_path_like_the_client_does() {
    let s = script();
    s.run(
        r#"
        f = CreateFrame("Frame", "SetTexArgs")
        t = f:CreateTexture(nil, "ARTWORK")
        t:SetTexture("Interface\\DialogFrame\\UI-DialogBox-Header", true)
    "#,
    )
    .expect("a stray extra argument must not raise — the client ignores it");

    // The colour form, with a boolean and a numeric string among the channels.
    s.run("t:SetTexture(0.25, '0.5', true)")
        .expect("the colour form must tolerate what lua_tonumber tolerates");

    // And the ordinary shapes still work.
    s.run("t:SetTexture(nil) t:SetTexture('') t:SetTexture(1, 0, 0, 1)")
        .expect("clear, blank and the plain colour form are unaffected");
}

/// **`SetGradientAlpha` / `SetGradient` exist, store both stops, and paint.**
///
/// These were the single wall in front of the corpus's largest family: `FuBar\FuBar_Panel.lua:144`
/// calls `SetGradientAlpha` while building the bar, so all 20 FuBar plugins died there — right after
/// the chunk-name/`debugstack` fix got them that far.
///
/// Asserted on the region PAINTING (a gradient-only region must emit a quad, because the client
/// generates the gradient into the same texture slot the colour form fills) and on the midpoint
/// being what a one-tint quad shows. The full gradient stays on the region for a renderer that
/// grows a second stop.
#[test]
fn a_gradient_is_stored_whole_and_painted_as_its_midpoint() {
    let mut s = script();
    s.run(
        r#"
        f = CreateFrame("Frame", "GradProbe")
        f:SetWidth(100) f:SetHeight(20)
        f:SetPoint("TOPLEFT", 0, 0)
        t = f:CreateTexture(nil, "ARTWORK")
        t:SetAllPoints(f)
        -- FuBar's own call: white at both stops, ALPHA only, vertical.
        t:SetGradientAlpha("VERTICAL", 1, 1, 1, 0, 1, 1, 1, 0.5)
    "#,
    )
    .expect("SetGradientAlpha must exist and accept the client's argument shape");

    s.resolve();
    let painted = s.extract().iter().any(|q| {
        matches!(&q.content, crate::script::QuadContent::Texture { color: Some(c), .. }
            // midpoint of alpha 0.0 and 0.5
            if (c[3] - 0.25).abs() < 1e-6 && c[0] == 1.0)
    });
    assert!(
        painted,
        "a region carrying only a gradient must paint, at the midpoint of its two stops"
    );

    // The alpha-less twin: both stops opaque, and a non-"VERTICAL" token is horizontal.
    s.run("t:SetGradient('HORIZONTAL', 1, 0, 0, 0, 0, 1)")
        .expect("SetGradient takes six colour arguments and no alpha");
    s.resolve();
    let mid = s.extract().iter().find_map(|q| match &q.content {
        crate::script::QuadContent::Texture { color: Some(c), .. } => Some(*c),
        _ => None,
    });
    assert_eq!(
        mid.map(|c| [c[0], c[1], c[2], c[3]]),
        Some([0.5, 0.0, 0.5, 1.0]),
        "SetGradient's stops are opaque, so the midpoint alpha is 1"
    );
}
