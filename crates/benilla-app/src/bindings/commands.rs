//! The command registry (decision 0997) — every binding command benilla actually implements, in
//! 1.12 `Bindings.xml` order, with the 1.12 default chords (byte-real: the client's own
//! `bindings-cache.wtf`, account ONE) and each command's dispatch class.
//!
//! **Honest tree**: a command appears here only over a real engine action — the same law as the
//! options rows (0954). The 1.12 commands with no benilla mechanism yet (pitch keys, walk toggle,
//! action pages, the right multibars' MULTIACTIONBAR3/4, camera views, screenshot, combat log, …)
//! are absent, not stubbed; the page shows only what's here, and only non-empty categories (era
//! law). Labels/headers are the 1.12
//! GlobalStrings (`BINDING_NAME_*`/`BINDING_HEADER_*`), defined in the window's XML.
//!
//! Three dispatch classes:
//! - [`Kind::Held`] — press latches, base-key release unlatches (the reference's `runOnUp`
//!   movement pairs); engine systems read the latch ([`super::BindingsState`]).
//! - [`Kind::Edge`] / [`Kind::EdgeUpDown`] — fires Lua in the VM (the reference's binding body,
//!   quoted 1:1 where our FrameXML port has the same functions).
//! - [`Kind::Host`] — fires into [`super::BindingsState::fired`]; an engine system consumes it
//!   (chat open, TAB targeting, nameplates, autorun, …).
//!
//! Recorded default divergences (0997): `TOGGLEUI` ships `ALT-Z` (0870: the cache's `CTRL-Z` is
//! a player rebind all three accounts inherited, not the shipped default — the one row that does
//! not trust the cache); `OPENALLBAGS` ships `B` + `SHIFT-B` (1.12 splits B/TOGGLEBACKPACK from
//! SHIFT-B/OPENALLBAGS; benilla's one bag knob is the open-all toggle that already lived on B).

/// A command's index into [`SPECS`] — the engine-side handle (`cmd::JUMP`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Cmd(pub u16);

pub(crate) enum Kind {
    /// Latched while the base key is held; engine systems read the state.
    Held,
    /// Fires this Lua on the matching press.
    Edge(&'static str),
    /// The reference's `runOnUp` button pair: Lua on press, Lua on base-key release.
    EdgeUpDown(&'static str, &'static str),
    /// Fires into [`super::BindingsState`]; an engine system consumes it.
    Host,
}

pub(crate) struct Spec {
    pub name: &'static str,
    /// The category header's global-string key (`BINDING_HEADER_MOVEMENT`) — the window's
    /// sidebar groups by it, era-style.
    pub category: &'static str,
    pub kind: Kind,
    /// The 1.12 default chords (canonical strings; `None` = shipped unbound).
    pub d1: Option<&'static str>,
    pub d2: Option<&'static str>,
}

impl Spec {
    /// Press+release semantics — the mousewheel-refusal law rides this at `SetBinding` time.
    pub(crate) fn run_on_up(&self) -> bool {
        matches!(self.kind, Kind::Held | Kind::EdgeUpDown(..))
    }
}

macro_rules! spec {
    ($name:literal, $cat:ident, $kind:expr, $d1:expr, $d2:expr) => {
        Spec {
            name: $name,
            category: concat!("BINDING_HEADER_", stringify!($cat)),
            kind: $kind,
            d1: $d1,
            d2: $d2,
        }
    };
}

/// Engine-side handles for the host-dispatched commands (indexes into [`SPECS`], asserted by
/// test). Lua-bodied commands need no handle — nothing in Rust names them.
pub(crate) mod cmd {
    use super::Cmd;
    pub(crate) const MOVE_AND_STEER: Cmd = Cmd(0);
    pub(crate) const MOVE_FORWARD: Cmd = Cmd(1);
    pub(crate) const MOVE_BACKWARD: Cmd = Cmd(2);
    pub(crate) const TURN_LEFT: Cmd = Cmd(3);
    pub(crate) const TURN_RIGHT: Cmd = Cmd(4);
    pub(crate) const STRAFE_LEFT: Cmd = Cmd(5);
    pub(crate) const STRAFE_RIGHT: Cmd = Cmd(6);
    pub(crate) const JUMP: Cmd = Cmd(7);
    pub(crate) const SIT_OR_STAND: Cmd = Cmd(8);
    pub(crate) const TOGGLE_SHEATH: Cmd = Cmd(9);
    pub(crate) const TOGGLE_AUTORUN: Cmd = Cmd(10);
    pub(crate) const OPEN_CHAT: Cmd = Cmd(12);
    pub(crate) const OPEN_CHAT_SLASH: Cmd = Cmd(13);
    pub(crate) const REPLY: Cmd = Cmd(17);
    pub(crate) const TARGET_NEAREST_ENEMY: Cmd = Cmd(50);
    pub(crate) const TARGET_PREVIOUS_ENEMY: Cmd = Cmd(51);
    pub(crate) const NAMEPLATES: Cmd = Cmd(62);
    pub(crate) const FRIEND_NAMEPLATES: Cmd = Cmd(63);
    pub(crate) const ALL_NAMEPLATES: Cmd = Cmd(64);
    pub(crate) const ATTACK_TARGET: Cmd = Cmd(65);
    pub(crate) const TOGGLE_UI: Cmd = Cmd(86);
    pub(crate) const CAMERA_ZOOM_IN: Cmd = Cmd(87);
    pub(crate) const CAMERA_ZOOM_OUT: Cmd = Cmd(88);
}

/// The registry, 1.12 `Bindings.xml` order. Sub-tables (action buttons, shapeshift, raid
/// targets) are written out because each row carries its own Lua body string.
pub(crate) static SPECS: &[Spec] = &[
    // ── Movement (BINDING_HEADER_MOVEMENT) ──────────────────────────────────────────────
    spec!("MOVEANDSTEER", MOVEMENT, Kind::Held, Some("BUTTON3"), None),
    spec!("MOVEFORWARD", MOVEMENT, Kind::Held, Some("W"), Some("UP")),
    spec!(
        "MOVEBACKWARD",
        MOVEMENT,
        Kind::Held,
        Some("S"),
        Some("DOWN")
    ),
    spec!("TURNLEFT", MOVEMENT, Kind::Held, Some("A"), Some("LEFT")),
    spec!("TURNRIGHT", MOVEMENT, Kind::Held, Some("D"), Some("RIGHT")),
    spec!("STRAFELEFT", MOVEMENT, Kind::Held, Some("Q"), None),
    spec!("STRAFERIGHT", MOVEMENT, Kind::Held, Some("E"), None),
    spec!("JUMP", MOVEMENT, Kind::Host, Some("SPACE"), Some("NUMPAD0")),
    spec!("SITORSTAND", MOVEMENT, Kind::Host, Some("X"), None),
    spec!("TOGGLESHEATH", MOVEMENT, Kind::Host, Some("Z"), None),
    spec!(
        "TOGGLEAUTORUN",
        MOVEMENT,
        Kind::Host,
        Some("NUMLOCK"),
        Some("BUTTON4")
    ),
    spec!(
        "FOLLOWTARGET",
        MOVEMENT,
        Kind::Edge(r#"FollowUnit("target")"#),
        None,
        None
    ),
    // ── Chat (BINDING_HEADER_CHAT) ──────────────────────────────────────────────────────
    spec!("OPENCHAT", CHAT, Kind::Host, Some("ENTER"), None),
    spec!("OPENCHATSLASH", CHAT, Kind::Host, Some("/"), None),
    spec!(
        "CHATPAGEUP",
        CHAT,
        Kind::Edge(r#"getglobal("ChatFrame" .. BenillaFCF.selected):PageUp()"#),
        Some("PAGEUP"),
        None
    ),
    spec!(
        "CHATPAGEDOWN",
        CHAT,
        Kind::Edge(r#"getglobal("ChatFrame" .. BenillaFCF.selected):PageDown()"#),
        Some("PAGEDOWN"),
        None
    ),
    spec!(
        "CHATBOTTOM",
        CHAT,
        Kind::Edge(r#"getglobal("ChatFrame" .. BenillaFCF.selected):ScrollToBottom()"#),
        Some("SHIFT-PAGEDOWN"),
        None
    ),
    spec!("REPLY", CHAT, Kind::Host, Some("R"), None),
    // ── Action bar (BINDING_HEADER_ACTIONBAR) ───────────────────────────────────────────
    // The ref's runOnUp pair (Bindings.xml:121: DOWN shows the pushed visual, UP fires) —
    // exactly what the old hardcoded number-row table sent.
    spec!(
        "ACTIONBUTTON1",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(1)", "BenillaActionButtonUp(1)"),
        Some("1"),
        None
    ),
    spec!(
        "ACTIONBUTTON2",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(2)", "BenillaActionButtonUp(2)"),
        Some("2"),
        None
    ),
    spec!(
        "ACTIONBUTTON3",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(3)", "BenillaActionButtonUp(3)"),
        Some("3"),
        None
    ),
    spec!(
        "ACTIONBUTTON4",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(4)", "BenillaActionButtonUp(4)"),
        Some("4"),
        None
    ),
    spec!(
        "ACTIONBUTTON5",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(5)", "BenillaActionButtonUp(5)"),
        Some("5"),
        None
    ),
    spec!(
        "ACTIONBUTTON6",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(6)", "BenillaActionButtonUp(6)"),
        Some("6"),
        None
    ),
    spec!(
        "ACTIONBUTTON7",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(7)", "BenillaActionButtonUp(7)"),
        Some("7"),
        None
    ),
    spec!(
        "ACTIONBUTTON8",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(8)", "BenillaActionButtonUp(8)"),
        Some("8"),
        None
    ),
    spec!(
        "ACTIONBUTTON9",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(9)", "BenillaActionButtonUp(9)"),
        Some("9"),
        None
    ),
    spec!(
        "ACTIONBUTTON10",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(10)", "BenillaActionButtonUp(10)"),
        Some("0"),
        None
    ),
    spec!(
        "ACTIONBUTTON11",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(11)", "BenillaActionButtonUp(11)"),
        Some("-"),
        None
    ),
    spec!(
        "ACTIONBUTTON12",
        ACTIONBAR,
        Kind::EdgeUpDown("BenillaActionButtonDown(12)", "BenillaActionButtonUp(12)"),
        Some("="),
        None
    ),
    // The stance/shapeshift row (ref ShapeshiftBar_ChangeForm(n)) — ours clicks the bar's own
    // buttons, which carry the full form-switch law (StanceBar.xml).
    spec!(
        "SHAPESHIFTBUTTON1",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("BenillaShapeshiftButton1"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F1"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON2",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("BenillaShapeshiftButton2"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F2"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON3",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("BenillaShapeshiftButton3"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F3"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON4",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("BenillaShapeshiftButton4"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F4"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON5",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("BenillaShapeshiftButton5"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F5"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON6",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("BenillaShapeshiftButton6"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F6"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON7",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("BenillaShapeshiftButton7"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F7"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON8",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("BenillaShapeshiftButton8"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F8"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON9",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("BenillaShapeshiftButton9"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F9"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON10",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("BenillaShapeshiftButton10"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F10"),
        None
    ),
    // The PET bar's row — 1.12 calls it BONUSACTIONBUTTON and files it under this same ACTIONBAR
    // header (Bindings.xml:321-390 carries no `header` attribute, so it inherits l.121's), which
    // is why "Secondary Action Button 1-10" reads under Action Bar in the reference's own window.
    // The ref's body is BonusActionButtonDown/Up(id), and those two are one-liners onto
    // PetActionButtonDown/Up (BonusActionBarFrame.lua:106-112) — the PET bar's buttons, not the
    // bonus bar's. benilla has no bonus bar at all, so the pet bar is the whole lane, exactly as
    // the reference wires it. Defaults are byte-real and unanimous: CTRL-1..CTRL-0 in all three
    // of the install's `bindings-cache.wtf` files (unlike TOGGLEUI's rebind, 0870).
    spec!(
        "BONUSACTIONBUTTON1",
        ACTIONBAR,
        Kind::EdgeUpDown(
            "BenillaPetActionButtonDown(1)",
            "BenillaPetActionButtonUp(1)"
        ),
        Some("CTRL-1"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON2",
        ACTIONBAR,
        Kind::EdgeUpDown(
            "BenillaPetActionButtonDown(2)",
            "BenillaPetActionButtonUp(2)"
        ),
        Some("CTRL-2"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON3",
        ACTIONBAR,
        Kind::EdgeUpDown(
            "BenillaPetActionButtonDown(3)",
            "BenillaPetActionButtonUp(3)"
        ),
        Some("CTRL-3"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON4",
        ACTIONBAR,
        Kind::EdgeUpDown(
            "BenillaPetActionButtonDown(4)",
            "BenillaPetActionButtonUp(4)"
        ),
        Some("CTRL-4"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON5",
        ACTIONBAR,
        Kind::EdgeUpDown(
            "BenillaPetActionButtonDown(5)",
            "BenillaPetActionButtonUp(5)"
        ),
        Some("CTRL-5"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON6",
        ACTIONBAR,
        Kind::EdgeUpDown(
            "BenillaPetActionButtonDown(6)",
            "BenillaPetActionButtonUp(6)"
        ),
        Some("CTRL-6"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON7",
        ACTIONBAR,
        Kind::EdgeUpDown(
            "BenillaPetActionButtonDown(7)",
            "BenillaPetActionButtonUp(7)"
        ),
        Some("CTRL-7"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON8",
        ACTIONBAR,
        Kind::EdgeUpDown(
            "BenillaPetActionButtonDown(8)",
            "BenillaPetActionButtonUp(8)"
        ),
        Some("CTRL-8"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON9",
        ACTIONBAR,
        Kind::EdgeUpDown(
            "BenillaPetActionButtonDown(9)",
            "BenillaPetActionButtonUp(9)"
        ),
        Some("CTRL-9"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON10",
        ACTIONBAR,
        Kind::EdgeUpDown(
            "BenillaPetActionButtonDown(10)",
            "BenillaPetActionButtonUp(10)"
        ),
        Some("CTRL-0"),
        None
    ),
    // ── Targeting (BINDING_HEADER_TARGETING) ────────────────────────────────────────────
    spec!(
        "TARGETNEARESTENEMY",
        TARGETING,
        Kind::Host,
        Some("TAB"),
        None
    ),
    spec!(
        "TARGETPREVIOUSENEMY",
        TARGETING,
        Kind::Host,
        Some("SHIFT-TAB"),
        None
    ),
    // The self/party bodies are 1.12's own, 1:1 (Bindings.xml:460-509 — already-targeted
    // falls through to the pet).
    spec!(
        "TARGETSELF",
        TARGETING,
        Kind::Edge(
            r#"if UnitIsUnit("player", "target") then TargetUnit("pet") else TargetUnit("player") end"#
        ),
        Some("F1"),
        None
    ),
    spec!(
        "TARGETPARTYMEMBER1",
        TARGETING,
        Kind::Edge(
            r#"if UnitIsUnit("party1", "target") then TargetUnit("partypet1") else TargetUnit("party1") end"#
        ),
        Some("F2"),
        None
    ),
    spec!(
        "TARGETPARTYMEMBER2",
        TARGETING,
        Kind::Edge(
            r#"if UnitIsUnit("party2", "target") then TargetUnit("partypet2") else TargetUnit("party2") end"#
        ),
        Some("F3"),
        None
    ),
    spec!(
        "TARGETPARTYMEMBER3",
        TARGETING,
        Kind::Edge(
            r#"if UnitIsUnit("party3", "target") then TargetUnit("partypet3") else TargetUnit("party3") end"#
        ),
        Some("F4"),
        None
    ),
    spec!(
        "TARGETPARTYMEMBER4",
        TARGETING,
        Kind::Edge(
            r#"if UnitIsUnit("party4", "target") then TargetUnit("partypet4") else TargetUnit("party4") end"#
        ),
        Some("F5"),
        None
    ),
    spec!(
        "TARGETPET",
        TARGETING,
        Kind::Edge(r#"TargetUnit("pet")"#),
        Some("SHIFT-F1"),
        None
    ),
    spec!(
        "TARGETPARTYPET1",
        TARGETING,
        Kind::Edge(r#"TargetUnit("partypet1")"#),
        Some("SHIFT-F2"),
        None
    ),
    spec!(
        "TARGETPARTYPET2",
        TARGETING,
        Kind::Edge(r#"TargetUnit("partypet2")"#),
        Some("SHIFT-F3"),
        None
    ),
    spec!(
        "TARGETPARTYPET3",
        TARGETING,
        Kind::Edge(r#"TargetUnit("partypet3")"#),
        Some("SHIFT-F4"),
        None
    ),
    spec!(
        "TARGETPARTYPET4",
        TARGETING,
        Kind::Edge(r#"TargetUnit("partypet4")"#),
        Some("SHIFT-F5"),
        None
    ),
    spec!("NAMEPLATES", TARGETING, Kind::Host, Some("V"), None),
    spec!(
        "FRIENDNAMEPLATES",
        TARGETING,
        Kind::Host,
        Some("SHIFT-V"),
        None
    ),
    spec!("ALLNAMEPLATES", TARGETING, Kind::Host, Some("CTRL-V"), None),
    spec!("ATTACKTARGET", TARGETING, Kind::Host, Some("T"), None),
    spec!(
        "PETATTACK",
        TARGETING,
        Kind::Edge("PetAttack()"),
        Some("SHIFT-T"),
        None
    ),
    // ── Interface panels (BINDING_HEADER_INTERFACE) ─────────────────────────────────────
    spec!(
        "TOGGLECHARACTER0",
        INTERFACE,
        Kind::Edge(r#"ToggleCharacter("BenillaPaperDollFrame")"#),
        Some("C"),
        None
    ),
    // 1.12 splits TOGGLEBACKPACK (B, F12) from OPENALLBAGS (SHIFT-B); benilla's one bag knob is
    // the open-all toggle that already lived on B — so OPENALLBAGS ships with both defaults
    // (recorded divergence, 0997).
    spec!(
        "OPENALLBAGS",
        INTERFACE,
        Kind::Edge("BenillaBagToggle_OnClick()"),
        Some("B"),
        Some("SHIFT-B")
    ),
    spec!(
        "TOGGLECHARACTER1",
        INTERFACE,
        Kind::Edge(r#"ToggleCharacter("BenillaSkillFrame")"#),
        Some("K"),
        None
    ),
    spec!(
        "TOGGLESPELLBOOK",
        INTERFACE,
        Kind::Edge("ToggleSpellBook(BOOKTYPE_SPELL)"),
        Some("P"),
        None
    ),
    spec!(
        "TOGGLETALENTS",
        INTERFACE,
        Kind::Edge("ToggleTalentFrame()"),
        Some("N"),
        None
    ),
    spec!(
        "TOGGLEQUESTLOG",
        INTERFACE,
        Kind::Edge("ToggleQuestLog()"),
        Some("L"),
        None
    ),
    spec!(
        "TOGGLEGAMEMENU",
        INTERFACE,
        Kind::Edge("ToggleGameMenu()"),
        Some("ESCAPE"),
        None
    ),
    spec!(
        "TOGGLEMINIMAP",
        INTERFACE,
        Kind::Edge(
            r#"if MinimapCluster:IsVisible() then MinimapCluster:Hide() else MinimapCluster:Show() end"#
        ),
        None,
        None
    ),
    spec!(
        "TOGGLEWORLDMAP",
        INTERFACE,
        Kind::Edge("ToggleWorldMap()"),
        Some("M"),
        None
    ),
    spec!(
        "TOGGLESOCIAL",
        INTERFACE,
        Kind::Edge("ToggleFriendsFrame()"),
        Some("O"),
        None
    ),
    spec!(
        "TOGGLEFRIENDSTAB",
        INTERFACE,
        Kind::Edge("ToggleFriendsFrame(1)"),
        None,
        None
    ),
    spec!(
        "TOGGLEWHOTAB",
        INTERFACE,
        Kind::Edge("ToggleFriendsFrame(2)"),
        None,
        None
    ),
    spec!(
        "TOGGLEGUILDTAB",
        INTERFACE,
        Kind::Edge("ToggleFriendsFrame(3)"),
        None,
        None
    ),
    // ── Miscellaneous (BINDING_HEADER_MISC) ─────────────────────────────────────────────
    spec!(
        "MINIMAPZOOMIN",
        MISC,
        Kind::Edge("Minimap_ZoomInClick()"),
        Some("NUMPADPLUS"),
        None
    ),
    spec!(
        "MINIMAPZOOMOUT",
        MISC,
        Kind::Edge("Minimap_ZoomOutClick()"),
        Some("NUMPADMINUS"),
        None
    ),
    // The sound toggles/steps flip the same CVars the ref's SoundOptionsFrame_* bodies do
    // (1.12 SoundOptionsFrame.lua: master enable = MasterSoundEffects, step 0.1).
    spec!(
        "TOGGLEMUSIC",
        MISC,
        Kind::Edge(
            r#"if GetCVar("EnableMusic") == "1" then SetCVar("EnableMusic", "0") else SetCVar("EnableMusic", "1") end"#
        ),
        Some("CTRL-M"),
        None
    ),
    spec!(
        "TOGGLESOUND",
        MISC,
        Kind::Edge(
            r#"if GetCVar("MasterSoundEffects") == "1" then SetCVar("MasterSoundEffects", "0") else SetCVar("MasterSoundEffects", "1") end"#
        ),
        Some("CTRL-S"),
        None
    ),
    spec!(
        "MASTERVOLUMEUP",
        MISC,
        Kind::Edge(
            r#"local v = tonumber(GetCVar("MasterVolume")) + 0.1; if v > 1 then v = 1 end; SetCVar("MasterVolume", v)"#
        ),
        Some("CTRL-="),
        None
    ),
    spec!(
        "MASTERVOLUMEDOWN",
        MISC,
        Kind::Edge(
            r#"local v = tonumber(GetCVar("MasterVolume")) - 0.1; if v < 0 then v = 0 end; SetCVar("MasterVolume", v)"#
        ),
        Some("CTRL--"),
        None
    ),
    // ALT-Z, not the cache's CTRL-Z — settled by 0870: all three of the install's accounts
    // descend from one profile whose TOGGLEUI had been rebound; ALT-Z is the shipped default
    // (the one command whose default does NOT trust the cache file).
    spec!("TOGGLEUI", MISC, Kind::Host, Some("ALT-Z"), None),
    // ── Camera (BINDING_HEADER_CAMERA) ──────────────────────────────────────────────────
    spec!(
        "CAMERAZOOMIN",
        CAMERA,
        Kind::Host,
        Some("MOUSEWHEELUP"),
        None
    ),
    spec!(
        "CAMERAZOOMOUT",
        CAMERA,
        Kind::Host,
        Some("MOUSEWHEELDOWN"),
        None
    ),
    // ── MultiActionBar (BINDING_HEADER_MULTIACTIONBAR) ──────────────────────────────────
    // The two bottom bars' buttons (MultiBars.xml renders exactly these; 1.12's right bars
    // and their MULTIACTIONBAR3/4 commands stay out — honest tree). Ref bodies are the
    // MultiActionButtonDown/Up runOnUp pair (Bindings.xml:799-966), transcribed in
    // MultiBars.xml; shipped UNBOUND like the ref (no MULTIACTIONBAR* line in any of the
    // install's bindings-cache.wtf files). 1.12 files bar 2 under a BLANK spacer-header;
    // both bars sit under the one MULTIACTIONBAR header here (1008, recorded).
    spec!(
        "MULTIACTIONBAR1BUTTON1",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 1)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 1)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON2",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 2)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 2)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON3",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 3)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 3)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON4",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 4)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 4)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON5",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 5)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 5)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON6",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 6)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 6)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON7",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 7)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 7)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON8",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 8)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 8)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON9",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 9)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 9)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON10",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 10)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 10)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON11",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 11)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 11)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON12",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomLeft", 12)"#,
            r#"BenillaMultiActionButtonUp("BottomLeft", 12)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON1",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 1)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 1)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON2",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 2)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 2)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON3",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 3)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 3)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON4",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 4)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 4)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON5",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 5)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 5)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON6",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 6)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 6)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON7",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 7)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 7)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON8",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 8)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 8)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON9",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 9)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 9)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON10",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 10)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 10)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON11",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 11)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 11)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON12",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"BenillaMultiActionButtonDown("BottomRight", 12)"#,
            r#"BenillaMultiActionButtonUp("BottomRight", 12)"#
        ),
        None,
        None
    ),
    // ── Raid targeting (BINDING_HEADER_RAID_TARGET) ─────────────────────────────────────
    // 1.12 bodies 1:1 (SetRaidTargetIcon toggles when the unit already wears the icon; 0
    // clears — party.rs's registered semantics). All unbound by default, like the client.
    spec!(
        "RAIDTARGET1",
        RAID_TARGET,
        Kind::Edge(r#"SetRaidTargetIcon("target", 1)"#),
        None,
        None
    ),
    spec!(
        "RAIDTARGET2",
        RAID_TARGET,
        Kind::Edge(r#"SetRaidTargetIcon("target", 2)"#),
        None,
        None
    ),
    spec!(
        "RAIDTARGET3",
        RAID_TARGET,
        Kind::Edge(r#"SetRaidTargetIcon("target", 3)"#),
        None,
        None
    ),
    spec!(
        "RAIDTARGET4",
        RAID_TARGET,
        Kind::Edge(r#"SetRaidTargetIcon("target", 4)"#),
        None,
        None
    ),
    spec!(
        "RAIDTARGET5",
        RAID_TARGET,
        Kind::Edge(r#"SetRaidTargetIcon("target", 5)"#),
        None,
        None
    ),
    spec!(
        "RAIDTARGET6",
        RAID_TARGET,
        Kind::Edge(r#"SetRaidTargetIcon("target", 6)"#),
        None,
        None
    ),
    spec!(
        "RAIDTARGET7",
        RAID_TARGET,
        Kind::Edge(r#"SetRaidTargetIcon("target", 7)"#),
        None,
        None
    ),
    spec!(
        "RAIDTARGET8",
        RAID_TARGET,
        Kind::Edge(r#"SetRaidTargetIcon("target", 8)"#),
        None,
        None
    ),
    spec!(
        "RAIDTARGETNONE",
        RAID_TARGET,
        Kind::Edge(r#"SetRaidTargetIcon("target", 0)"#),
        None,
        None
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The named handles in [`cmd`] must index the rows they claim — the table is the truth,
    /// the consts are the readable view of it.
    #[test]
    fn the_cmd_handles_index_their_rows() {
        for (handle, name) in [
            (cmd::MOVE_AND_STEER, "MOVEANDSTEER"),
            (cmd::MOVE_FORWARD, "MOVEFORWARD"),
            (cmd::MOVE_BACKWARD, "MOVEBACKWARD"),
            (cmd::TURN_LEFT, "TURNLEFT"),
            (cmd::TURN_RIGHT, "TURNRIGHT"),
            (cmd::STRAFE_LEFT, "STRAFELEFT"),
            (cmd::STRAFE_RIGHT, "STRAFERIGHT"),
            (cmd::JUMP, "JUMP"),
            (cmd::SIT_OR_STAND, "SITORSTAND"),
            (cmd::TOGGLE_SHEATH, "TOGGLESHEATH"),
            (cmd::TOGGLE_AUTORUN, "TOGGLEAUTORUN"),
            (cmd::OPEN_CHAT, "OPENCHAT"),
            (cmd::OPEN_CHAT_SLASH, "OPENCHATSLASH"),
            (cmd::REPLY, "REPLY"),
            (cmd::TARGET_NEAREST_ENEMY, "TARGETNEARESTENEMY"),
            (cmd::TARGET_PREVIOUS_ENEMY, "TARGETPREVIOUSENEMY"),
            (cmd::NAMEPLATES, "NAMEPLATES"),
            (cmd::FRIEND_NAMEPLATES, "FRIENDNAMEPLATES"),
            (cmd::ALL_NAMEPLATES, "ALLNAMEPLATES"),
            (cmd::ATTACK_TARGET, "ATTACKTARGET"),
            (cmd::TOGGLE_UI, "TOGGLEUI"),
            (cmd::CAMERA_ZOOM_IN, "CAMERAZOOMIN"),
            (cmd::CAMERA_ZOOM_OUT, "CAMERAZOOMOUT"),
        ] {
            assert_eq!(
                SPECS[handle.0 as usize].name, name,
                "cmd handle {name} points at SPECS[{}] = {}",
                handle.0, SPECS[handle.0 as usize].name
            );
        }
    }

    /// Every default chord in the table parses — a typo'd token would otherwise silently ship
    /// an unpressable default.
    #[test]
    fn every_default_chord_parses() {
        for s in SPECS {
            for d in [s.d1, s.d2].into_iter().flatten() {
                assert!(
                    crate::bindings::chord::Chord::parse(d).is_some(),
                    "{}: default '{d}' does not parse",
                    s.name
                );
            }
        }
    }

    /// No two commands ship the SAME default chord. One key, one command is the binding table's
    /// own law (the steal pass in [`super::store::resolve`] enforces it at load), so a duplicate
    /// here would ship a table that eats itself: registration order would decide the winner and
    /// the loser would come up silently unbound. Cheap tripwire for the one mistake a new block
    /// of rows can make — 1052's CTRL-1..CTRL-0 went in with nothing to catch a collision.
    #[test]
    fn no_two_commands_ship_the_same_default_chord() {
        let mut seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for s in SPECS {
            for d in [s.d1, s.d2].into_iter().flatten() {
                if let Some(prev) = seen.insert(d, s.name) {
                    panic!("default '{d}' is on both {prev} and {}", s.name);
                }
            }
        }
    }

    /// Names are unique (the table is keyed by them everywhere: files, Lua, the window).
    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for s in SPECS {
            assert!(seen.insert(s.name), "duplicate command {}", s.name);
        }
    }
}
