//! **The 1.12 surface is the contract** (decisions 1188 §2, 1189) — enforced here rather than
//! trusted to memory.
//!
//! benilla targets the 1.12.1 API. Anything beyond it is a *listed, justified* exception, and the
//! point of this file is that the list cannot go unknown: every global our VM exposes must either
//! be one 1.12 has, or appear below with a reason. A session that adds an Era-shaped verb has to
//! delete or extend an assertion that says why not — which is exactly what did not happen when
//! 1187 shipped eight Era globals on Era's authority and 1189 had to take them back out.
//!
//! **Why a superset is not free.** Lua branches on presence. An addon writing `if strmatch then`
//! takes a path we cannot honour, and the failure surfaces far from the cause. Extra functions are
//! harmless only if nothing feature-detects, which is not true of this ecosystem.
//!
//! The reference side is `reference/1.12-globals.tsv` — the running 1.12.1 client's own in-world
//! `_G` (see `reference/README.md`). This test reads the `engine` and `lua` rows: the surface a VM
//! is responsible for. It deliberately does **not** assert the converse — that we have everything
//! 1.12 has — because that is a multi-year backlog, not a regression gate;
//! `scripts/api-coverage.sh` is where that number is read.

use std::collections::HashSet;

use super::common::script;

/// `reference/1.12-globals.tsv`, as `name -> origin`.
fn reference() -> Vec<(String, String)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../reference/1.12-globals.tsv"
    );
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("reading {path}: {e} — regenerate with scripts/gen-reference-globals.py")
    });
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?.to_string(), f.nth(1)?.to_string()))
        })
        .collect()
}

/// Names benilla exposes that 1.12 does not, each with the reason it is allowed to stay.
///
/// **`Benilla*` and `__benilla_*` are covered by prefix**, not enumerated: the first is our host
/// bridge (verbs only our own transcribed FrameXML calls — a paperdoll model's facing, an item's
/// stat block) and the second is the tick's pushed state. Neither is an API-target claim, and
/// neither is reachable by accident from an addon that means to call a WoW function.
fn allowed_beyond_1_12() -> HashSet<&'static str> {
    [
        // ── our Lua runtime is 5.1 where 1.12's is 5.0 ────────────────────────────────────────
        // `select` is load-bearing and stays on purpose — our transcribed FrameXML uses it in 16
        // files as the 5.1 spelling of 5.0's implicit `arg` table.
        //
        // `_G` is the last one inherited rather than chosen: 1.12's base library does not export
        // it (an addon reaches the globals with `getfenv(0)`, which is what AceLibrary does), but
        // our own `getglobal`/`setglobal` are written over it. Closing it means rewriting those
        // against the registry first.
        //
        // **This list has been shrinking as the dialect got measured**: `coroutine` left in 1194
        // with the 5.1-only members of `string`/`table`/`math`; `print` and `_VERSION` left in
        // 1197, when the RE dispatch read the base library's 36-entry array and neither was in it.
        // The list only ever covered globals — the members needed `dump_globals --members` before
        // anyone could see them at all.
        "_G",
        "select",
        // ── WoW API past 1.12 — 1188 phase 5's list, and the reason that phase exists ─────────
        // Every one of these predates 1189 and is used by our own transcribed FrameXML today.
        // Resolving each means either replacing it with its 1.12 equivalent (`UnitPower` →
        // `UnitMana`, which 1.12 has and we do not) or recording why it stays.
        "CancelUnitBuff",
        "GetCursorInfo",
        "GetGossipQuestInfo",
        "GetInventoryItemID",
        "GetNumGossipQuests",
        "GetPlayerFacing",
        "GetTradePartnerName",
        "IsGossipOptionCoded",
        "SelectGossipQuest",
        "SubmitChatInput",
        "UnitAura",
        "UnitIsAFK",
        "UnitIsDND",
        "strconcat",
        "strjoin",
        "strsplit",
        "strtrim",
        "tostringall",
        "wipe",
    ]
    .into_iter()
    .collect()
}

/// Every global benilla's VM exposes is one 1.12 has, or a listed exception.
///
/// The failure message is the point: it names what is new and tells you the two ways out, so the
/// next session cannot resolve it by deleting a bare assertion.
#[test]
fn our_globals_stay_inside_the_1_12_surface() {
    let reference = reference();
    let known: HashSet<&str> = reference.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        known.len() > 19_000,
        "the reference table looks truncated ({} names) — regenerate it",
        known.len()
    );
    let allowed = allowed_beyond_1_12();

    let ours: Vec<String> = script()
        .eval(
            "local out = {} \
             for k in pairs(_G) do if type(k) == 'string' then table.insert(out, k) end end \
             return out",
        )
        .expect("dump _G");

    let mut unlisted: Vec<&str> = ours
        .iter()
        .map(String::as_str)
        .filter(|n| !known.contains(n))
        .filter(|n| !allowed.contains(n))
        .filter(|n| !n.starts_with("Benilla") && !n.starts_with("__benilla_"))
        .collect();
    unlisted.sort_unstable();

    assert!(
        unlisted.is_empty(),
        "benilla exposes {} global(s) the 1.12.1 client does not, and they are not listed as \
         exceptions:\n    {}\n\n\
         1.12 is the target (decision 1188). Either give it its 1.12 spelling, or add it to \
         `allowed_beyond_1_12` in this file WITH the reason it has to stay — an unexplained \
         superset is what 1189 had to roll back.",
        unlisted.len(),
        unlisted.join(" ")
    );
}

/// The exception list is exact — no entry outlives the global it excuses.
///
/// Without this, a removed global leaves its excuse behind, and the list slowly stops describing
/// the code. That is how a "listed, justified exception" decays into residue nobody can explain.
#[test]
fn the_exception_list_has_no_dead_entries() {
    let known: HashSet<String> = reference().into_iter().map(|(n, _)| n).collect();
    let ours: HashSet<String> = script()
        .eval::<Vec<String>>(
            "local out = {} \
             for k in pairs(_G) do if type(k) == 'string' then table.insert(out, k) end end \
             return out",
        )
        .expect("dump _G")
        .into_iter()
        .collect();

    let mut dead: Vec<&str> = allowed_beyond_1_12()
        .into_iter()
        .filter(|n| !ours.contains(*n))
        .collect();
    dead.sort_unstable();
    assert!(
        dead.is_empty(),
        "these are excused as beyond-1.12 but benilla no longer exposes them — drop them from \
         `allowed_beyond_1_12`:\n    {}",
        dead.join(" ")
    );

    // An entry that 1.12 turns out to have is a different bug: the excuse is wrong, not stale.
    let mut wrong: Vec<&str> = allowed_beyond_1_12()
        .into_iter()
        .filter(|n| known.contains(*n))
        .collect();
    wrong.sort_unstable();
    assert!(
        wrong.is_empty(),
        "these are excused as beyond-1.12 but the 1.12.1 client DOES have them — they need no \
         excuse:\n    {}",
        wrong.join(" ")
    );
}

/// **`Texture:GetTexture()` — the three contract details that a plausible implementation gets
/// silently wrong.** Verified in wow-re's widget-method batch (`0x79ba70`/`0x79baf0`/`0x835708`,
/// `system/ui/scratch/widget-api-batch-benilla.md`).
///
/// Four corpus addons reach it: `AtlasQuest.lua:228` is `AQATLASMAP = AtlasMap:GetTexture()` and
/// `FuBarPlugin-2.0.lua:343` is `return self.iconFrame:GetTexture()`, each behind two addons.
///
/// The colour case is the one worth a test of its own. `SetTexture(r,g,b)` synthesizes an 8x8 solid
/// and the getter reports the literal name `"Solid Texture"` — so an addon's `if not tex then`
/// guard passes straight through. Returning nil there would read as tidier and would be wrong in
/// exactly the direction callers test for.
#[test]
fn get_texture_returns_the_stripped_path_and_solid_texture_for_a_fill() {
    let s = crate::script::UiScript::new().unwrap();
    s.run(
        r#"
        f = CreateFrame("Frame", "GTF")
        tex = f:CreateTexture("GTFTex", "ARTWORK")
        fill = f:CreateTexture("GTFFill", "ARTWORK")
        "#,
    )
    .unwrap();

    // Never set: nil, and exactly one value.
    assert_eq!(
        s.eval::<Option<String>>("return GTFTex:GetTexture()")
            .unwrap(),
        None
    );
    assert_eq!(
        s.eval::<i64>("local a,b = GTFTex:GetTexture() return select('#', GTFTex:GetTexture())")
            .unwrap_or(1),
        1,
        "exactly one return value"
    );

    // A path with no extension comes back unchanged.
    s.run(r#"GTFTex:SetTexture("Interface\\Icons\\Spell_Fire_FlameBolt")"#)
        .unwrap();
    assert_eq!(
        s.eval::<String>("return GTFTex:GetTexture()").unwrap(),
        "Interface\\Icons\\Spell_Fire_FlameBolt"
    );

    // ...and one WITH an extension is stripped at the last `.` — the loader appends it, the getter
    // takes it back off.
    s.run(r#"GTFTex:SetTexture("Interface\\Icons\\Foo.blp")"#)
        .unwrap();
    assert_eq!(
        s.eval::<String>("return GTFTex:GetTexture()").unwrap(),
        "Interface\\Icons\\Foo"
    );

    // The colour form: the literal name, NOT nil.
    s.run("GTFFill:SetTexture(1, 0, 0, 1)").unwrap();
    assert_eq!(
        s.eval::<String>("return GTFFill:GetTexture()").unwrap(),
        "Solid Texture",
        "a colour-filled region reports a NAME — `if not tex then` must not fire"
    );

    // And a plain SetTexture makes it ordinary again.
    s.run(r#"GTFFill:SetTexture("Interface\\Buttons\\UI-Quickslot2")"#)
        .unwrap();
    assert_eq!(
        s.eval::<String>("return GTFFill:GetTexture()").unwrap(),
        "Interface\\Buttons\\UI-Quickslot2"
    );
}

/// **The shadow accessors on a REGION, and `GetShadowColor`'s four values.**
///
/// They existed on the font-object table; a FontString from `CreateFontString` had none, which is
/// exactly where the corpus calls them. `FuBar_NavigatorFu/NavigatorFu.lua:31` is
/// `coordText:SetShadowColor(GameFontNormal:GetShadowColor())` — getter on a font object, setter on
/// a fresh region, in one line — and `KLHThreatMeter/.../KTM_Gui.lua:404` is
/// `fontstring:SetShadowColor(0,0,0,0.3)`.
///
/// **Four values, not three** (`0x79dd2f`, `mov eax,0x4` — wow-re's widget-method batch). Three is
/// the plausible wrong answer and it silently drops the alpha NavigatorFu round-trips: the whole
/// point of its line is that whatever `GameFontNormal` carries arrives intact.
#[test]
fn region_shadow_accessors_round_trip_four_values() {
    let s = crate::script::UiScript::new().unwrap();
    s.run(r#"f = CreateFrame("Frame", "SHF") fs = f:CreateFontString("SHFText", "ARTWORK")"#)
        .unwrap();

    assert_eq!(
        s.eval::<i64>("return select('#', SHFText:GetShadowColor())")
            .unwrap(),
        4,
        "GetShadowColor returns FOUR values — three drops the alpha"
    );
    assert_eq!(
        s.eval::<i64>("return select('#', SHFText:GetShadowOffset())")
            .unwrap(),
        2
    );

    // NavigatorFu's line, in shape: a font object's shadow piped straight into a region's.
    s.run("SHFText:SetShadowColor(0, 0, 0, 0.3) SHFText:SetShadowOffset(1, -1)")
        .unwrap();
    let (r, g, b, a) = s
        .eval::<(f32, f32, f32, f32)>("return SHFText:GetShadowColor()")
        .unwrap();
    assert_eq!((r, g, b, a), (0.0, 0.0, 0.0, 0.3), "the alpha survives");
    assert_eq!(
        s.eval::<(f32, f32)>("return SHFText:GetShadowOffset()")
            .unwrap(),
        (1.0, -1.0)
    );

    // Either half may be set first: setting only the offset must not blank the colour.
    s.run("SHFText:SetShadowOffset(2, -2)").unwrap();
    assert_eq!(
        s.eval::<f32>("local _,_,_,a = SHFText:GetShadowColor() return a")
            .unwrap(),
        0.3,
        "setting the offset alone keeps the colour"
    );
}

/// **`Region:SetParent(frame)` — it exists on a Texture/FontString, and we were the ones missing
/// it.** `SetParent` is in the Region method table (`0x7a1550`) and both leaf lookups fall back to
/// Region's (`0x79c650` / `0x79ee50`), so `FuBar_FuXPFu.lua:210`'s
/// `self.Spark:SetParent(self.XPBar)` — a texture from `XPBar:CreateTexture` — is a working line on
/// the real client (wow-re `system/ui/scratch/widget-api-batch-benilla.md` Q7).
///
/// The two traps pinned here are the ones a plausible implementation gets wrong in opposite
/// directions. **A non-Frame argument RAISES** (`IsA(FrameTag)` at `0x7a16ea`, message
/// `"…Wrong parent object type, expected frame"`) rather than no-opping; and **anchors are NOT
/// touched** by the re-link, which moves draw-layer/region-list membership only — so a re-parented
/// texture keeps resolving against whatever `SetPoint` named, and FuXPFu's re-point on the next
/// line is the correct pattern rather than a workaround.
#[test]
fn region_set_parent_relinks_the_draw_owner_and_leaves_anchors_alone() {
    /// The resolved rect of the one texture quad whose path contains `needle`, if it draws at all.
    fn tex_rect(s: &crate::script::UiScript, needle: &str) -> Option<crate::layout::Rect> {
        s.extract().iter().find_map(|q| match &q.content {
            crate::script::QuadContent::Texture { path: Some(p), .. } if p.contains(needle) => {
                Some(q.rect.expect("resolved rect"))
            }
            _ => None,
        })
    }

    let mut s = crate::script::UiScript::new().unwrap();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        A = CreateFrame("Frame", "SPOwnerA")
        A:SetPoint("BOTTOMLEFT", 0, 0)  A:SetSize(100, 50)
        B = CreateFrame("Frame", "SPOwnerB")
        B:SetPoint("BOTTOMLEFT", 300, 0)  B:SetSize(100, 50)
        Spark = A:CreateTexture("SPSpark", "ARTWORK")
        Spark:SetTexture("Interface\\SPGlow")
        Spark:SetSize(24, 24)
        Spark:SetPoint("TOPLEFT", 4, -4)
        "#,
    )
    .unwrap();
    s.resolve();
    // A is [bottom 0, left 0, top 50, right 100]: TOPLEFT +(4,-4), 24x24 ⇒ 4..28 x 22..46.
    let before = tex_rect(&s, "SPGlow").expect("the spark draws under A");
    assert_eq!(before, crate::layout::Rect::new(22.0, 4.0, 46.0, 28.0));

    // FuXPFu's line, and it returns nothing at all.
    assert_eq!(
        s.eval::<i64>("return select('#', Spark:SetParent(B))")
            .unwrap(),
        0,
        "SetParent returns zero values on every path"
    );
    s.resolve();
    assert_eq!(
        tex_rect(&s, "SPGlow"),
        Some(before),
        "the anchors are untouched — the spark still resolves against A, 300px from B"
    );

    // …and the re-link is real, not merely recorded: the spark now draws with B. Hiding B takes it
    // off the screen; hiding A (which it is still anchored to) does not.
    s.run("B:Hide()").unwrap();
    assert_eq!(
        tex_rect(&s, "SPGlow"),
        None,
        "the spark hides with its NEW owner"
    );
    s.run("B:Show() A:Hide()").unwrap();
    s.resolve();
    assert_eq!(
        tex_rect(&s, "SPGlow"),
        Some(before),
        "and not with the frame it is merely anchored to"
    );

    // A non-Frame argument raises. A texture is the case the reference names, and it is the one an
    // addon actually hits (a mixed table of frames and textures walked in a loop).
    for bad in ["Spark:SetParent(Spark)", "Spark:SetParent(5)"] {
        let e = s
            .run(bad)
            .expect_err("a non-frame parent must raise")
            .to_string();
        assert!(
            e.contains("expected frame"),
            "wanted the reference's 'expected frame' rejection, got: {e}"
        );
    }
    // A MISSING argument is not the nil form: TNONE never reaches the nil branch.
    assert!(s.run("Spark:SetParent()").is_err(), "no argument raises");

    // The nil form detaches without error: orphaned and unrendered, not destroyed — a later
    // re-parent brings the same region back.
    s.run("A:Show() Spark:SetParent(nil)").unwrap();
    s.resolve();
    assert_eq!(
        tex_rect(&s, "SPGlow"),
        None,
        "a detached region draws nothing"
    );
    s.run("Spark:SetParent(A)").unwrap();
    s.resolve();
    assert_eq!(
        tex_rect(&s, "SPGlow"),
        Some(before),
        "and re-parenting restores it"
    );
}

/// **`Button:SetFont(file, height [, flags])` — it exists, it returns NOTHING, and it never
/// touches the label.** `0x780880`, wow-re's widget-method batch Q8 (§5-verified).
/// `_LazyPig/LazyPigMenu.lua:214` calls it straight on a `CreateFrame("Button", …)` and is blocked
/// on it today.
///
/// Three traps, each pinned here because each is a plausible implementation's silent divergence.
/// **Zero return values**: the shared impl pushes `1`/nil (a real font-load probe on a Font object)
/// and Button *discards* it (`xor eax,eax`), so answering `true` would hand an addon a probe the
/// client does not have. **`GetFont` returns three values**, off the normal embedded font — which
/// resolves through what that state inherits when nothing set it locally. And **a bare Button with
/// no `<ButtonText>` is a silent no-op**: `SetFont`/`GetFont` never dereference the FontString
/// pointer `+0x338`, so there is no error and — the observable half — **no lazy label creation**,
/// which `GetFontString()` would report.
#[test]
fn button_set_font_returns_nothing_and_is_a_no_op_without_a_label() {
    let s = crate::script::UiScript::new().unwrap();
    s.run(
        r#"
        Bare = CreateFrame("Button", "SFBare")
        Labelled = CreateFrame("Button", "SFLabelled")
        Labelled:SetText("Show Keybinds")
        "#,
    )
    .unwrap();

    // _LazyPig's line, on a Button that has no label at all.
    assert_eq!(
        s.eval::<i64>(r#"return select('#', SFBare:SetFont("Fonts\\FRIZQT__.TTF", 8))"#)
            .unwrap(),
        0,
        "SetFont returns ZERO values — the delegate's 1/nil is discarded by a Button"
    );
    assert!(
        s.eval::<bool>("return SFBare:GetFontString() == nil")
            .unwrap(),
        "no label existed and none was lazily created"
    );

    assert_eq!(
        s.eval::<i64>("return select('#', SFBare:GetFont())")
            .unwrap(),
        3,
        "GetFont returns THREE values"
    );
    assert_eq!(
        s.eval::<(String, f32, String)>("return SFBare:GetFont()")
            .unwrap(),
        ("Fonts\\FRIZQT__.TTF".to_string(), 8.0, String::new())
    );
    // Still three when nothing has ever set a font — path and height are nil, not absent.
    assert_eq!(
        s.eval::<i64>("return select('#', SFLabelled:GetFont())")
            .unwrap(),
        3
    );
    assert!(s
        .eval::<bool>("local f = SFLabelled:GetFont() return f == nil")
        .unwrap());

    // The flags argument is normalized to its OUTLINETYPE token, like every other GetFont.
    s.run(r#"SFBare:SetFont("Fonts\\SKURRI.TTF", 12, "THICKOUTLINE")"#)
        .unwrap();
    assert_eq!(
        s.eval::<String>("local _, _, flags = SFBare:GetFont() return flags")
            .unwrap(),
        "THICKOUTLINE"
    );
    // Both arguments are required (`lua_isstring` + `lua_isnumber`, else the usage error).
    assert!(s.run(r#"SFBare:SetFont("Fonts\\SKURRI.TTF")"#).is_err());

    // Unset locally, GetFont reads through what the NORMAL state inherits — how a
    // GameMenuButtonTemplate button answers its template's face before anything calls SetFont.
    s.run(
        r#"
        CreateFont("SFInherited")
        SFInherited:SetFont("Fonts\\MORPHEUS.TTF", 14)
        SFLabelled:SetTextFontObject(SFInherited)
        "#,
    )
    .unwrap();
    assert_eq!(
        s.eval::<(String, f32)>("local p, h = SFLabelled:GetFont() return p, h")
            .unwrap(),
        ("Fonts\\MORPHEUS.TTF".to_string(), 14.0)
    );

    // …and a local SetFont outranks that inherited object on the label's PAINT, not only in the
    // getter — the whole point of the call `_LazyPig` makes.
    s.run(r#"SFLabelled:SetFont("Fonts\\SKURRI.TTF", 9)"#)
        .unwrap();
    let (font, height) = s
        .extract()
        .iter()
        .find_map(|q| match &q.content {
            crate::script::QuadContent::Text {
                text: Some(t),
                font,
                font_height,
                ..
            } if t == "Show Keybinds" => Some((font.clone(), *font_height)),
            _ => None,
        })
        .expect("the labelled button draws its text");
    assert_eq!(
        (font.as_deref(), height),
        (Some("Fonts\\SKURRI.TTF"), Some(9.0)),
        "the button's own font repaints the label over the inherited font object"
    );
}

/// **`SetNonSpaceWrap` / `CanNonSpaceWrap`** — FontString only (`0x79e9f0`/`0x79ead0`, wow-re's
/// widget-method batch). `oRA2/Leader/Item.lua:561` is `f.textname:SetNonSpaceWrap(false)`, reached
/// by two addons.
///
/// Two contract details, both easy to get wrong and both pinned here: the getter is
/// **`CanNonSpaceWrap`**, not `GetNonSpaceWrap`, and it answers **`1` or nil** rather than a
/// boolean — 1.12 predates that convention and an addon may compare against 1. And a **no-argument
/// call ENABLES** it; the default is on, so it is not a query.
#[test]
fn non_space_wrap_defaults_on_and_answers_one_or_nil() {
    let s = crate::script::UiScript::new().unwrap();
    s.run(r#"f = CreateFrame("Frame", "NSF") fs = f:CreateFontString("NSFText", "ARTWORK")"#)
        .unwrap();

    // Default is ON, and the answer is the NUMBER 1 — not `true`.
    assert_eq!(
        s.eval::<Option<i64>>("return NSFText:CanNonSpaceWrap()")
            .unwrap(),
        Some(1),
        "default on, answered as 1"
    );
    assert!(
        !s.eval::<bool>("return NSFText:CanNonSpaceWrap() == true")
            .unwrap(),
        "it must NOT be a boolean — an addon comparing against 1 would break"
    );

    // oRA2's line.
    s.run("NSFText:SetNonSpaceWrap(false)").unwrap();
    assert_eq!(
        s.eval::<Option<i64>>("return NSFText:CanNonSpaceWrap()")
            .unwrap(),
        None,
        "off answers nil, not 0 and not false"
    );

    // A no-argument call ENABLES rather than querying.
    s.run("NSFText:SetNonSpaceWrap()").unwrap();
    assert_eq!(
        s.eval::<Option<i64>>("return NSFText:CanNonSpaceWrap()")
            .unwrap(),
        Some(1),
        "a bare SetNonSpaceWrap() turns it ON"
    );
}

/// **The EditBox font block — the first sixteen entries of its own registrar table, and the largest
/// single gap the per-kind widget-method census found** (decision 1229, whose ranking opens
/// `63  EditBox:SetFontObject   (on Texture, FontString)`).
///
/// `EditBox`'s table is `.data 0x87bb68`, **48 entries** — the count read from the `mov edx,0x30` at
/// the registering site `0x799ab5`, never from a run-length scan. There is no `FontInstance` class
/// in the 1.12 Lua chain (wow-re `widget-api-batch-benilla.md`): each of the six text-bearing types
/// re-declares the block in its own flat table, so membership is a per-table fact and **a name we
/// add that the table does not carry is exactly as wrong as one we miss**.
///
/// The two halves pinned here are therefore both directions. Present: the fourteen we wire.
/// Absent: `SetSpacing`/`GetSpacing`, which the table *does* carry (#10–#11) and we deliberately
/// withhold because nothing here models line spacing — a stored-but-undrawn setter is the silent
/// divergence of 1203/1205/1211, while a nil-value call names itself, and corpus demand is zero.
/// Absent too: the names that belong to *other* tables and must not leak onto an EditBox through
/// ours — `CopyFontObject` (Font object only), `SetNonSpaceWrap`/`CanNonSpaceWrap` and
/// `SetTextHeight`/`GetStringWidth` (FontString only).
#[test]
fn editbox_carries_the_font_block_its_registrar_table_declares() {
    let s = crate::script::UiScript::new().unwrap();
    s.run(r#"EB = CreateFrame("EditBox", "EBFont")"#).unwrap();

    for name in [
        "SetFontObject",
        "GetFontObject",
        "SetFont",
        "GetFont",
        "SetTextColor",
        "GetTextColor",
        "SetShadowColor",
        "GetShadowColor",
        "SetShadowOffset",
        "GetShadowOffset",
        "SetJustifyH",
        "GetJustifyH",
        "SetJustifyV",
        "GetJustifyV",
    ] {
        assert_eq!(
            s.eval::<String>(&format!("return type(EBFont.{name})"))
                .unwrap(),
            "function",
            "EditBox table entry '{name}' is missing"
        );
    }

    for name in [
        // On EditBox's real table, withheld on purpose (nothing models spacing).
        "SetSpacing",
        "GetSpacing",
        // Not on EditBox's table at all — adding one is the same class of error as missing one.
        "CopyFontObject",
        "SetNonSpaceWrap",
        "CanNonSpaceWrap",
        "SetTextHeight",
        "GetStringWidth",
    ] {
        assert!(
            s.eval::<bool>(&format!("return EBFont.{name} == nil"))
                .unwrap(),
            "'{name}' must NOT answer on an EditBox"
        );
    }
}

/// **Every EditBox font binding is a type-guard shim that returns whatever the shared implementation
/// returned**, so the arities below are the shared ones verbatim — verified at the bytes: each
/// binding ends `call <shared>` then `pop edi; pop esi; pop ebx; ret`, with **no `mov eax,N` and no
/// `xor eax,eax`** after the call.
///
/// The trap is `SetFont`, and it points the opposite way to the one already pinned for Button.
/// `Button:SetFont` (`0x780880`) ends `xor eax,eax` and returns **nothing** —
/// [`button_set_font_returns_nothing_and_is_a_no_op_without_a_label`]. `EditBox:SetFont`
/// (`0x797210` → `0x79f210`) does not discard, so the shared `lua_pushnumber(1.0)` (`0x79f345`) /
/// `lua_pushnil` (`0x79f361`) passes straight through: **one value, the number `1`, not `true` and
/// not zero values**. A probe written `if not eb:SetFont(f, s) then` needs the nil half; a probe
/// written `== 1` needs the number half.
///
/// `GetShadowColor` returns **four** values (`0x79f9b3 mov eax,0x4`); three is the plausible wrong
/// answer and silently drops the alpha.
#[test]
fn editbox_font_block_return_shapes_are_the_shared_implementations() {
    let s = crate::script::UiScript::new().unwrap();
    s.run(
        r#"
        EB = CreateFrame("EditBox", "EBShape")
        CreateFont("EBProbeFont")
        EBProbeFont:SetFont("Fonts\\FRIZQT__.TTF", 14)
        "#,
    )
    .unwrap();

    // The mutators push nothing at all.
    for call in [
        "SetFontObject(EBProbeFont)",
        "SetTextColor(1, 0, 0)",
        "SetShadowColor(0, 0, 0, 0.3)",
        "SetShadowOffset(1, -1)",
        r#"SetJustifyH("CENTER")"#,
        r#"SetJustifyV("TOP")"#,
    ] {
        assert_eq!(
            s.eval::<i64>(&format!("return select('#', EBShape:{call})"))
                .unwrap(),
            0,
            "EditBox:{call} must return ZERO values"
        );
    }

    // The getters' exact widths.
    for (call, want) in [
        ("GetFontObject()", 1),
        ("GetFont()", 3),
        ("GetTextColor()", 4),
        ("GetShadowColor()", 4),
        ("GetShadowOffset()", 2),
        ("GetJustifyH()", 1),
        ("GetJustifyV()", 1),
    ] {
        assert_eq!(
            s.eval::<i64>(&format!("return select('#', EBShape:{call})"))
                .unwrap(),
            want,
            "EditBox:{call} must return {want} value(s)"
        );
    }

    // SetFont: ONE value, and it is the number 1 — not `true`, and not nothing (the Button shape).
    assert_eq!(
        s.eval::<i64>(r#"return select('#', EBShape:SetFont("Fonts\\SKURRI.TTF", 12))"#)
            .unwrap(),
        1,
        "EditBox:SetFont returns one value, unlike Button:SetFont"
    );
    assert_eq!(
        s.eval::<String>(r#"return type(EBShape:SetFont("Fonts\\SKURRI.TTF", 12))"#)
            .unwrap(),
        "number",
        "the number 1, never the boolean true"
    );
    assert!(s
        .eval::<bool>(r#"return EBShape:SetFont("Fonts\\SKURRI.TTF", 12) == 1"#)
        .unwrap());
    // An unloadable (empty) path is the FALSEY answer, not an error — `!OmniCC`'s probe idiom.
    assert!(s
        .eval::<bool>(r#"return EBShape:SetFont("", 12) == nil"#)
        .unwrap());
    // Both arguments are gated (`lua_isstring` + `lua_isnumber`, else the usage error).
    assert!(s.run(r#"EBShape:SetFont("Fonts\\SKURRI.TTF")"#).is_err());

    // The shadow round-trips all four channels — three would drop the alpha.
    assert_eq!(
        s.eval::<(f32, f32, f32, f32)>("return EBShape:GetShadowColor()")
            .unwrap(),
        (0.0, 0.0, 0.0, 0.3)
    );
    assert_eq!(
        s.eval::<(f32, f32)>("return EBShape:GetShadowOffset()")
            .unwrap(),
        (1.0, -1.0)
    );
    // GetFontObject answers the OBJECT, never its name — the corpus indexes the result immediately.
    assert_eq!(
        s.eval::<String>("return type(EBShape:GetFontObject())")
            .unwrap(),
        "table"
    );
}

/// **The real `Dewdrop-2.0` shape, which is the whole reason this landed.**
///
/// `Dewdrop-2.0.lua:1673-1675` is verbatim:
/// ```lua
/// local editBox = CreateFrame("EditBox", nil, editBoxFrame)
/// editBoxFrame.editBox = editBox
/// editBox:SetFontObject(ChatFontNormal)
/// ```
/// — an **anonymous** EditBox parented to a frame, then `SetFontObject` on it two lines later. That
/// file is vendored into **63 addon folders** of the 218-addon corpus (65 copies: `FuBar` plus ~50
/// FuBar plugins, `BigWigs`, `AtlasLoot`, `oRA2`, `SnaFu`, …), so the census's `63` is **one library
/// replicated**, not 63 independent addons (decision 1207).
///
/// `AceGUIWidget-Slider.lua:204-210` is the same shape with `SetJustifyH("CENTER")` on the end.
#[test]
fn the_dewdrop_editbox_font_shape_resolves_and_paints() {
    let s = crate::script::UiScript::new().unwrap();
    s.run(
        r#"
        CreateFont("ChatFontNormal")
        ChatFontNormal:SetFont("Fonts\\ARIALN.TTF", 11)
        ChatFontNormal:SetTextColor(0.9, 0.9, 0.9)

        editBoxFrame = CreateFrame("Frame", "DewdropEBFrame")
        local editBox = CreateFrame("EditBox", nil, editBoxFrame)
        editBoxFrame.editBox = editBox
        editBox:SetFontObject(ChatFontNormal)
        editBox:SetWidth(160)
        editBox:SetHeight(13)
        "#,
    )
    .unwrap();

    // The link is live and readable back as the object.
    assert!(s
        .eval::<bool>("return DewdropEBFrame.editBox:GetFontObject() == ChatFontNormal")
        .unwrap());
    // …and the object's paint really reached the box's implicit FontString (the `[this+0x324]` the
    // reference's shim hands the shared implementation).
    assert_eq!(
        s.eval::<(String, f32)>("local p, h = DewdropEBFrame.editBox:GetFont() return p, h")
            .unwrap(),
        ("Fonts\\ARIALN.TTF".to_string(), 11.0)
    );
    let (r, g, b, _) = s
        .eval::<(f32, f32, f32, f32)>("return DewdropEBFrame.editBox:GetTextColor()")
        .unwrap();
    assert_eq!((r, g, b), (0.9, 0.9, 0.9));

    // The AceGUI slider's tail, on the same box.
    s.run(r#"DewdropEBFrame.editBox:SetJustifyH("CENTER")"#)
        .unwrap();
}

/// **`SetJustifyH`/`SetJustifyV` mask to their own axis, and a token from the other axis silently
/// clears rather than erroring.** The enum is `.rdata 0x811ad0`, `{bits, token}`: `LEFT` 0x01,
/// `CENTER` 0x02, `RIGHT` 0x04, `TOP` 0x08, `MIDDLE` 0x10, `BOTTOM` 0x20, stored in one dword with
/// bits 0–2 horizontal and 3–5 vertical.
///
/// Two traps, both of which our older FontString/Font copies of these verbs get wrong by coercing
/// anything unrecognised to CENTER/MIDDLE:
///  · **an unknown token RAISES** `Usage: %s:SetJustifyH("justify")` (`0x87c77c`);
///  · **a valid token from the wrong axis parses and then masks to nothing** — `SetJustifyH("TOP")`
///    yields 0x08 and `0x08 & 0x07 == 0`, so justifyH is CLEARED and `GetJustifyH()` answers the
///    literal `"UNKNOWN"` (`0x6f1a00`, `.data 0x838044`). No error is raised either way.
#[test]
fn editbox_justify_masks_to_its_axis_and_answers_unknown() {
    let s = crate::script::UiScript::new().unwrap();
    s.run(r#"EB = CreateFrame("EditBox", "EBJustify")"#)
        .unwrap();

    // **LEFT, not the generic font default CENTER.** The `CSimpleFont` ctor really does default to
    // `0x212` (CENTER | MIDDLE | 0x200), but the EditBox ctor overrides the horizontal axis right
    // after linking its font instance (`0x779bcd … and eax,~6; or eax,1;` stored at `0x779be4`), so
    // a fresh box starts at `0x211`. Taking the generic default is the plausible wrong answer, and
    // it is exactly what this test asserted until wow-re's §5 trio read that ctor
    // (`system/ui/scratch/editbox-font-surface.md` §6.2).
    assert_eq!(
        s.eval::<String>("return EBJustify:GetJustifyH()").unwrap(),
        "LEFT"
    );
    assert_eq!(
        s.eval::<String>("return EBJustify:GetJustifyV()").unwrap(),
        "MIDDLE"
    );

    // Each axis is replaced independently — setting V leaves H alone.
    s.run(r#"EBJustify:SetJustifyH("LEFT") EBJustify:SetJustifyV("BOTTOM")"#)
        .unwrap();
    assert_eq!(
        s.eval::<(String, String)>("return EBJustify:GetJustifyH(), EBJustify:GetJustifyV()")
            .unwrap(),
        ("LEFT".to_string(), "BOTTOM".to_string())
    );

    // The cross-axis trap: "TOP" parses, masks to 0, and CLEARS justifyH.
    s.run(r#"EBJustify:SetJustifyH("TOP")"#).unwrap();
    assert_eq!(
        s.eval::<String>("return EBJustify:GetJustifyH()").unwrap(),
        "UNKNOWN",
        "a vertical token on SetJustifyH clears the axis rather than centering it"
    );
    assert_eq!(
        s.eval::<String>("return EBJustify:GetJustifyV()").unwrap(),
        "BOTTOM",
        "and it leaves the vertical axis untouched"
    );

    // An unrecognised token is the usage error, never a silent fallback to CENTER.
    assert!(s.run(r#"EBJustify:SetJustifyH("SIDEWAYS")"#).is_err());
}

/// **`EditBox:SetJustifyV` echoes through its getter and never reaches the pixels — on the real
/// client too, permanently and by construction.** wow-re `editbox-font-surface.md` §6 (§5 trio +
/// byte arbitration): `CSimpleFontString+0x124` is a per-bit *inherit* mask over the rendered
/// justify `+0x120`, and `SetMultiLine 0x77a4a0` clears the whole vertical group `0x38` from it on
/// **both** legs while writing the V bits locally — multi-line → TOP, single-line → MIDDLE. The
/// EditBox ctor calls `SetMultiLine` unconditionally at birth (`0x779c2f`, with the ctor's zero
/// register), and a census of all 256 `+0x124` operands image-wide found every `CSimpleFontString`
/// writer to be an AND: **nothing ever ORs an inherit bit back in.** So the value written by
/// `SetJustifyV` is masked out at `0x77086e`, while `GetJustifyV` reads the font *instance*
/// (`0x79fd73`) and answers it faithfully.
///
/// This pins the shape rather than the accident: a future change that "fixes" the getter by wiring
/// V justify through to the region would match neither the client nor our own draw law.
#[test]
fn editbox_justify_v_echoes_but_multiline_alone_decides_the_pixels() {
    let mut s = crate::script::UiScript::new().unwrap();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        EB = CreateFrame("EditBox", "EBJV")
        EBJV:SetPoint("CENTER", 0, 0) EBJV:SetWidth(120) EBJV:SetHeight(40)
        EBJV:SetText("typed")
        "#,
    )
    .unwrap();

    /// The vertical justification the box's text actually draws with.
    fn drawn_v(s: &crate::script::UiScript) -> crate::script::JustifyV {
        s.extract()
            .iter()
            .find_map(|q| match &q.content {
                crate::script::QuadContent::Text {
                    text: Some(t),
                    justify_v,
                    ..
                } if t == "typed" => Some(*justify_v),
                _ => None,
            })
            .expect("the box draws its text")
    }

    // A single-line box renders MIDDLE, the `0x77a599` leg.
    assert_eq!(drawn_v(&s), crate::script::JustifyV::Middle);

    // SetJustifyV("TOP") is echoed by the getter…
    s.run(r#"EBJV:SetJustifyV("TOP")"#).unwrap();
    assert_eq!(
        s.eval::<String>("return EBJV:GetJustifyV()").unwrap(),
        "TOP",
        "the getter reads the font instance and echoes faithfully"
    );
    // …and does NOT move the pixels. This is the disagreement, and it is faithful.
    assert_eq!(
        drawn_v(&s),
        crate::script::JustifyV::Middle,
        "SetJustifyV is masked out of the rendered justify — multiLine alone decides it"
    );

    // multiLine is what actually moves it, on the `0x77a509` leg.
    s.run("EBJV:SetMultiLine(true)").unwrap();
    assert_eq!(drawn_v(&s), crate::script::JustifyV::Top);
    // …and the getter still echoes whatever was last set, unaffected.
    assert_eq!(
        s.eval::<String>("return EBJV:GetJustifyV()").unwrap(),
        "TOP"
    );
}

/// **`Texture:SetDesaturated(flag)` answers `shaderSupported`, and ours says nil.**
///
/// `0x79c1e0` (wow-re ledger). The reference's own `ItemButtonTemplate.lua:69` is
/// `local shaderSupported = icon:SetDesaturated(desaturated)`, and lines 70-78 fall back to a 0.5
/// grey vertex tint when that answer is falsy — 1.12 shipped on cards without the shader, so the
/// verb reporting "no" is a real machine's answer, not a stub.
///
/// We have no desaturating shader, so nil is the honest reply: claiming support would suppress
/// FrameXML's grey fallback and leave disabled icons at full colour, which looks *more* wrong.
/// The test pins the return shape rather than the state, because the return is what callers branch
/// on — and `nil`, not `false`, because the C answer is `1|nil` and the reference writes
/// `not shaderSupported`.
///
/// 98 of the 109 draw-then-raise addons in the corpus die on this one verb, via
/// `FuBar_Panel.lua:43` → Dewdrop `AddLine` → `button.arrow:SetDesaturated(true)`, unguarded.
#[test]
fn set_desaturated_reports_no_shader_support_and_does_not_raise() {
    let s = crate::script::UiScript::new().unwrap();
    s.run(r#"f = CreateFrame("Frame", "DsF") tex = f:CreateTexture("DsTex", "ARTWORK")"#)
        .unwrap();

    // Dewdrop's exact call — the one 98 addons reach. It must not raise.
    s.run("DsTex:SetDesaturated(true)").unwrap();

    // ...and it answers nil, so the reference's `not shaderSupported` branch is taken.
    assert!(
        s.eval::<bool>("return DsTex:SetDesaturated(true) == nil")
            .unwrap(),
        "shaderSupported must be nil (1|nil C shape), not false"
    );
    assert_eq!(
        s.eval::<i64>("return select('#', DsTex:SetDesaturated(true))")
            .unwrap(),
        1,
        "one return value"
    );

    // The reference's own consumer, transcribed: a falsy answer greys via vertex colour.
    s.run(
        r#"
        local shaderSupported = DsTex:SetDesaturated(true)
        local r, g, b = 1, 1, 1
        if not shaderSupported then r, g, b = 0.5, 0.5, 0.5 end
        DsTex:SetVertexColor(r, g, b)
        "#,
    )
    .unwrap();
    let (r, g, b) = s
        .eval::<(f32, f32, f32)>("return DsTex:GetVertexColor()")
        .unwrap();
    assert_eq!((r, g, b), (0.5, 0.5, 0.5), "the no-shader fallback greys");

    // Both spellings of "off" are off — nil and false.
    s.run("DsTex:SetDesaturated(nil) DsTex:SetDesaturated(false)")
        .unwrap();
}

/// `UnitCreatureType(unit)` — `0x51a280`'s three-stage resolver, of which we model stages 2 and 3.
///
/// The load-bearing assertion is the **player** one. `0x605570` falls through the (never populated)
/// creature record to a `ChrRaces.dbc` col-9 lookup that is **7 for all nine shipped races**, and
/// `CreatureType[7]` is `"Humanoid"` — so a player answers a word, not nil. Reading only the
/// creature record, which is what our data alone suggested, would have been wrong for every
/// `UnitCreatureType("player")` call in the corpus.
#[test]
fn unit_creature_type_answers_the_record_then_falls_back_to_humanoid() {
    let mut s = script();
    s.set_unit(
        "target",
        Some(crate::script::UnitState {
            exists: true,
            creature_type_name: Some("Beast".into()),
            ..Default::default()
        }),
    );
    s.set_unit(
        "player",
        Some(crate::script::UnitState {
            exists: true,
            is_player: true,
            ..Default::default()
        }),
    );

    // Stage 2: a creature's cached record wins.
    assert_eq!(
        s.eval::<String>(r#"return UnitCreatureType("target")"#)
            .unwrap(),
        "Beast"
    );
    // Stage 3: a player has no record and falls through to race -> Humanoid.
    assert_eq!(
        s.eval::<String>(r#"return UnitCreatureType("player")"#)
            .unwrap(),
        "Humanoid"
    );
    // An unresolved TOKEN is nil — not a raise.
    assert!(s
        .eval::<bool>(r#"return UnitCreatureType("party4") == nil"#)
        .unwrap());
    // A non-string ARGUMENT is a raise, which is a different failure from the nil above:
    // `lua_isstring` gates it and `luaL_error` longjmps, so a missing arg abandons the statement.
    let err = s
        .run("UnitCreatureType()")
        .expect_err("a missing arg must raise");
    assert!(
        format!("{err}").contains("Usage: UnitCreatureType"),
        "got {err}"
    );
}
