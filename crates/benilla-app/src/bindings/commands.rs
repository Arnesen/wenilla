//! The command registry (decision 0997) — every binding command benilla actually implements, in
//! 1.12 `Bindings.xml` order, with the 1.12 default chords (byte-real: the client's own
//! `bindings-cache.wtf`, account ONE) and each command's dispatch class.
//!
//! **Honest tree**: a command appears here only over a real engine action — the same law as the
//! options rows (0954). The 1.12 commands with no benilla mechanism yet (pitch keys, walk toggle,
//! action pages, the right multibars' MULTIACTIONBAR3/4, camera views, combat log, …)
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
//! not trust the cache). That is now the ONLY one — the bag row's divergence (`OPENALLBAGS` wearing
//! both `B` and `SHIFT-B`, because benilla had a single all-bags knob) is gone as of 1494: the
//! reference's split ships whole, keys and bodies alike.

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
    /// Press+release semantics — the reference's `<Binding runOnUp=…>`, whose ONE reader is
    /// `RunCommand 0x4b7bf1`: it gates the release half and nothing else. (It used to gate a
    /// mousewheel refusal at `SetBinding` time here; that refusal is not in the client — 1295.)
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
    use super::{Cmd, TABLE};

    /// Resolve a command NAME to its [`Cmd`] handle at compile time — a linear scan of [`TABLE`]
    /// in a `const fn`, so a name that is not in the registry is a BUILD error, not a runtime
    /// surprise.
    ///
    /// These handles used to be hand-written row numbers, and every row that landed ahead of one
    /// silently re-pointed it at its neighbour: 1057's `TOGGLECHARACTER3` moved three of them
    /// 86/87/88 → 87/88/89, 1136's `TOGGLEACTIONBARLOCK` moved the same three again, and 1494's
    /// bag family would have moved them a third time (`TOGGLE_UI` landing on `MINIMAPZOOMOUT`). A
    /// guard test named the drift but the numbers still had to be re-typed by hand each time; the
    /// name is the thing we actually mean, so the name is what the table is asked for now.
    const fn by_name(name: &str) -> Cmd {
        let mut i = 0;
        while i < TABLE.len() {
            if const_str_eq(TABLE[i].name, name) {
                return Cmd(i as u16);
            }
            i += 1;
        }
        panic!("no such binding command")
    }

    const fn const_str_eq(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    pub(crate) const MOVE_AND_STEER: Cmd = by_name("MOVEANDSTEER");
    pub(crate) const MOVE_FORWARD: Cmd = by_name("MOVEFORWARD");
    pub(crate) const MOVE_BACKWARD: Cmd = by_name("MOVEBACKWARD");
    pub(crate) const TURN_LEFT: Cmd = by_name("TURNLEFT");
    pub(crate) const TURN_RIGHT: Cmd = by_name("TURNRIGHT");
    pub(crate) const STRAFE_LEFT: Cmd = by_name("STRAFELEFT");
    pub(crate) const STRAFE_RIGHT: Cmd = by_name("STRAFERIGHT");
    pub(crate) const JUMP: Cmd = by_name("JUMP");
    pub(crate) const SIT_OR_STAND: Cmd = by_name("SITORSTAND");
    pub(crate) const TOGGLE_SHEATH: Cmd = by_name("TOGGLESHEATH");
    pub(crate) const TOGGLE_AUTORUN: Cmd = by_name("TOGGLEAUTORUN");
    pub(crate) const OPEN_CHAT: Cmd = by_name("OPENCHAT");
    pub(crate) const OPEN_CHAT_SLASH: Cmd = by_name("OPENCHATSLASH");
    pub(crate) const REPLY: Cmd = by_name("REPLY");
    pub(crate) const TARGET_NEAREST_ENEMY: Cmd = by_name("TARGETNEARESTENEMY");
    pub(crate) const TARGET_PREVIOUS_ENEMY: Cmd = by_name("TARGETPREVIOUSENEMY");
    pub(crate) const NAMEPLATES: Cmd = by_name("NAMEPLATES");
    pub(crate) const FRIEND_NAMEPLATES: Cmd = by_name("FRIENDNAMEPLATES");
    pub(crate) const ALL_NAMEPLATES: Cmd = by_name("ALLNAMEPLATES");
    pub(crate) const ATTACK_TARGET: Cmd = by_name("ATTACKTARGET");
    pub(crate) const TOGGLE_UI: Cmd = by_name("TOGGLEUI");
    pub(crate) const CAMERA_ZOOM_IN: Cmd = by_name("CAMERAZOOMIN");
    pub(crate) const CAMERA_ZOOM_OUT: Cmd = by_name("CAMERAZOOMOUT");
}

/// The registry, 1.12 `Bindings.xml` order. Sub-tables (action buttons, shapeshift, raid
/// targets) are written out because each row carries its own Lua body string.
///
/// `TABLE` is the `const` view [`cmd::by_name`] scans at compile time (a `static` cannot be read
/// during const evaluation); `SPECS` is the one everything else uses.
pub(crate) static SPECS: &[Spec] = TABLE;

const TABLE: &[Spec] = &[
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
        Kind::EdgeUpDown("ActionButtonDown(1)", "ActionButtonUp(1)"),
        Some("1"),
        None
    ),
    spec!(
        "ACTIONBUTTON2",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(2)", "ActionButtonUp(2)"),
        Some("2"),
        None
    ),
    spec!(
        "ACTIONBUTTON3",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(3)", "ActionButtonUp(3)"),
        Some("3"),
        None
    ),
    spec!(
        "ACTIONBUTTON4",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(4)", "ActionButtonUp(4)"),
        Some("4"),
        None
    ),
    spec!(
        "ACTIONBUTTON5",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(5)", "ActionButtonUp(5)"),
        Some("5"),
        None
    ),
    spec!(
        "ACTIONBUTTON6",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(6)", "ActionButtonUp(6)"),
        Some("6"),
        None
    ),
    spec!(
        "ACTIONBUTTON7",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(7)", "ActionButtonUp(7)"),
        Some("7"),
        None
    ),
    spec!(
        "ACTIONBUTTON8",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(8)", "ActionButtonUp(8)"),
        Some("8"),
        None
    ),
    spec!(
        "ACTIONBUTTON9",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(9)", "ActionButtonUp(9)"),
        Some("9"),
        None
    ),
    spec!(
        "ACTIONBUTTON10",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(10)", "ActionButtonUp(10)"),
        Some("0"),
        None
    ),
    spec!(
        "ACTIONBUTTON11",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(11)", "ActionButtonUp(11)"),
        Some("-"),
        None
    ),
    spec!(
        "ACTIONBUTTON12",
        ACTIONBAR,
        Kind::EdgeUpDown("ActionButtonDown(12)", "ActionButtonUp(12)"),
        Some("="),
        None
    ),
    // The stance/shapeshift row (ref ShapeshiftBar_ChangeForm(n)) — ours clicks the bar's own
    // buttons, which carry the full form-switch law (StanceBar.xml).
    spec!(
        "SHAPESHIFTBUTTON1",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("ShapeshiftButton1"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F1"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON2",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("ShapeshiftButton2"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F2"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON3",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("ShapeshiftButton3"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F3"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON4",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("ShapeshiftButton4"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F4"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON5",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("ShapeshiftButton5"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F5"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON6",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("ShapeshiftButton6"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F6"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON7",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("ShapeshiftButton7"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F7"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON8",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("ShapeshiftButton8"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F8"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON9",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("ShapeshiftButton9"); if b and b:IsVisible() then b:Click() end"#
        ),
        Some("CTRL-F9"),
        None
    ),
    spec!(
        "SHAPESHIFTBUTTON10",
        ACTIONBAR,
        Kind::Edge(
            r#"local b = getglobal("ShapeshiftButton10"); if b and b:IsVisible() then b:Click() end"#
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
        Kind::EdgeUpDown("PetActionButtonDown(1)", "PetActionButtonUp(1)"),
        Some("CTRL-1"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON2",
        ACTIONBAR,
        Kind::EdgeUpDown("PetActionButtonDown(2)", "PetActionButtonUp(2)"),
        Some("CTRL-2"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON3",
        ACTIONBAR,
        Kind::EdgeUpDown("PetActionButtonDown(3)", "PetActionButtonUp(3)"),
        Some("CTRL-3"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON4",
        ACTIONBAR,
        Kind::EdgeUpDown("PetActionButtonDown(4)", "PetActionButtonUp(4)"),
        Some("CTRL-4"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON5",
        ACTIONBAR,
        Kind::EdgeUpDown("PetActionButtonDown(5)", "PetActionButtonUp(5)"),
        Some("CTRL-5"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON6",
        ACTIONBAR,
        Kind::EdgeUpDown("PetActionButtonDown(6)", "PetActionButtonUp(6)"),
        Some("CTRL-6"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON7",
        ACTIONBAR,
        Kind::EdgeUpDown("PetActionButtonDown(7)", "PetActionButtonUp(7)"),
        Some("CTRL-7"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON8",
        ACTIONBAR,
        Kind::EdgeUpDown("PetActionButtonDown(8)", "PetActionButtonUp(8)"),
        Some("CTRL-8"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON9",
        ACTIONBAR,
        Kind::EdgeUpDown("PetActionButtonDown(9)", "PetActionButtonUp(9)"),
        Some("CTRL-9"),
        None
    ),
    spec!(
        "BONUSACTIONBUTTON10",
        ACTIONBAR,
        Kind::EdgeUpDown("PetActionButtonDown(10)", "PetActionButtonUp(10)"),
        Some("CTRL-0"),
        None
    ),
    // The action-bar lock (decision 1136), the ref's own binding body verbatim (Bindings.xml:433-
    // 439) — it flips the `LOCK_ACTIONBAR` uvar `ActionBar.xml` declares, the same global the
    // Options window's Action Bars row writes. It sits here because the reference files it under
    // this header (l.433 carries no `header=`, so it inherits l.121's ACTIONBAR), and it ships
    // **unbound**: no `TOGGLEACTIONBARLOCK` line in any of the install's three
    // `bindings-cache.wtf` files, which is also what the option's own tooltip implies ("can be
    // bound to a function key in the keybindings interface").
    spec!(
        "TOGGLEACTIONBARLOCK",
        ACTIONBAR,
        Kind::Edge(
            r#"if LOCK_ACTIONBAR == "1" then LOCK_ACTIONBAR = "0" else LOCK_ACTIONBAR = "1" end"#
        ),
        None,
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
        Kind::Edge(r#"ToggleCharacter("PaperDollFrame")"#),
        Some("C"),
        None
    ),
    // The bag family, in the reference's own `Bindings.xml` order and with its own bodies and
    // defaults (1494). 0997 collapsed all of this onto ONE command — `OPENALLBAGS` wearing both
    // `B` and `SHIFT-B` over a `ToggleBackpack()` that opened every bag — because benilla had a
    // single all-bags knob to hang keys off. The reference has three knobs and the director
    // reported the difference: `B` opens the backpack ALONE, `SHIFT-B` opens the lot.
    //
    // Every default here is byte-real from the client's own `bindings-cache.wtf`, identical across
    // all three independent accounts (ONE, TWO, WINUSER) — which is what rules out a player rebind
    // (the `TOGGLEUI` trap, 0870).
    //
    // Each body is the bare GLOBAL, exactly the ref's own `Bindings.xml` — NOT a button's OnClick
    // handler: a handler carries the button's checked bookkeeping, and routing a key through it
    // drags that onto a path the reference keeps clean (with a bag addon holding `ToggleBackpack`,
    // the key press is the path that lights the backpack button on the real client — the addon's
    // OnShow write is the last word).
    spec!(
        "TOGGLEBACKPACK",
        INTERFACE,
        Kind::Edge("ToggleBackpack()"),
        Some("B"),
        Some("F12")
    ),
    // TOGGLEBAG**N** is bag **5-N**: the reference's numbering runs the bar right-to-left, so F8
    // opens the slot FARTHEST from the backpack. Quoted from ref Bindings.xml l.564-575.
    spec!(
        "TOGGLEBAG1",
        INTERFACE,
        Kind::Edge("ToggleBag(4)"),
        Some("F8"),
        None
    ),
    spec!(
        "TOGGLEBAG2",
        INTERFACE,
        Kind::Edge("ToggleBag(3)"),
        Some("F9"),
        None
    ),
    spec!(
        "TOGGLEBAG3",
        INTERFACE,
        Kind::Edge("ToggleBag(2)"),
        Some("F10"),
        None
    ),
    spec!(
        "TOGGLEBAG4",
        INTERFACE,
        Kind::Edge("ToggleBag(1)"),
        Some("F11"),
        None
    ),
    spec!(
        "OPENALLBAGS",
        INTERFACE,
        Kind::Edge("OpenAllBags()"),
        Some("SHIFT-B"),
        None
    ),
    spec!(
        "TOGGLECHARACTER1",
        INTERFACE,
        Kind::Edge(r#"ToggleCharacter("SkillFrame")"#),
        Some("K"),
        None
    ),
    // The pet paper doll is TOGGLECHARACTER**3**, not 2 — 1.12's `Bindings.xml` numbers these by
    // page, not by tab (0 = PaperDoll, 1 = Skill, 2 = Reputation, 3 = PetPaperDoll, 4 = Honor), so
    // the name is the reference's and has nothing to do with our tab index. `SHIFT-P` is byte-real
    // from the client's own `bindings-cache.wtf`, and identical in two independent accounts (ONE
    // and WINUSER) — which is what rules out a player rebind (the `TOGGLEUI` trap, 0870).
    // Decision 1057.
    spec!(
        "TOGGLECHARACTER3",
        INTERFACE,
        Kind::Edge(r#"ToggleCharacter("PetPaperDollFrame")"#),
        Some("SHIFT-P"),
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
    // Print screen (decision 1487). The body is the reference's own one-liner because our
    // `TakeScreenshot` has the same contract its does (ScreenshotStatus.xml) — hide the last
    // shot's confirmation, then ask the engine.
    //
    // `PRINTSCREEN` comes from the SHIPPED default, not a player's cache: `DefaultBindings.wtf`
    // lives inside `patch.MPQ` and its line 128 is `bind PRINTSCREEN SCREENSHOT` (wow-re's
    // dispatch on this feature; the install's account-ONE `bindings-cache.wtf` agrees). `Edge`,
    // not `EdgeUpDown`, is also byte-real: the `<Binding>` carries no `runOnUp`, and the
    // reference's dispatcher returns on key-up unless that flag is set (`0x4b7bea`).
    //
    // On a Mac keyboard the token arrives as F13, which is the reference's own Mac mapping rather
    // than an accommodation (`KEY_PRINTSCREEN_MAC = "F13"`); `super::chord` does the translation.
    spec!(
        "SCREENSHOT",
        MISC,
        Kind::Edge("TakeScreenshot();"),
        Some("PRINTSCREEN"),
        None
    ),
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
            r#"MultiActionButtonDown("MultiBarBottomLeft", 1)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 1)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON2",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 2)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 2)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON3",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 3)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 3)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON4",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 4)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 4)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON5",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 5)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 5)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON6",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 6)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 6)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON7",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 7)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 7)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON8",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 8)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 8)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON9",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 9)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 9)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON10",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 10)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 10)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON11",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 11)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 11)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR1BUTTON12",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomLeft", 12)"#,
            r#"MultiActionButtonUp("MultiBarBottomLeft", 12)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON1",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 1)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 1)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON2",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 2)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 2)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON3",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 3)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 3)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON4",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 4)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 4)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON5",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 5)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 5)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON6",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 6)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 6)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON7",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 7)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 7)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON8",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 8)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 8)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON9",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 9)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 9)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON10",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 10)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 10)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON11",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 11)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 11)"#
        ),
        None,
        None
    ),
    spec!(
        "MULTIACTIONBAR2BUTTON12",
        MULTIACTIONBAR,
        Kind::EdgeUpDown(
            r#"MultiActionButtonDown("MultiBarBottomRight", 12)"#,
            r#"MultiActionButtonUp("MultiBarBottomRight", 12)"#
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

    /// **The registry against the real client's own two files.** Two columns of this table are
    /// pure transcription from the 1.12 install, and both are load-bearing in a way nothing else
    /// here can see: `runOnUp` — which decides whether a wheel notch delivers a release half at
    /// all (1295), and which B265 turned entirely on — and every command's default chords. A
    /// hand-typed table drifts; this reads the source of truth instead of trusting the typing.
    ///
    /// `Interface\FrameXML\Bindings.xml` carries the flag, `WTF\DefaultBindings.wtf` the chords.
    /// Both are install content, read at runtime, never committed; the test skips (passes)
    /// without a client, the house pattern for a real-data test. Running Blizzard's own 234-row
    /// file through our [`benilla_ui::bindings_xml`] is a second gate riding along — that is the
    /// parser every addon's `Bindings.xml` lands in.
    #[test]
    fn the_registry_matches_the_installs_own_bindings() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open the 1.12 patch chain");

        let xml = String::from_utf8_lossy(
            &chain
                .read_file("Interface\\FrameXML\\Bindings.xml")
                .expect("the install carries Bindings.xml"),
        )
        .into_owned();
        let reference = benilla_ui::bindings_xml::parse(&xml).expect("Blizzard's own file parses");
        let run_on_up: std::collections::HashMap<&str, bool> = reference
            .iter()
            .map(|b| (b.name.as_str(), b.run_on_up))
            .collect();

        // `bind <CHORD> <COMMAND>`, in the file's own order — which is the order the command's
        // Key 1 and Key 2 slots take.
        let wtf = String::from_utf8_lossy(
            &chain
                .read_file("WTF\\DefaultBindings.wtf")
                .expect("the install carries DefaultBindings.wtf"),
        )
        .into_owned();
        let mut defaults: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for line in wtf.lines() {
            let mut it = line.split_whitespace();
            if let (Some("bind"), Some(chord), Some(command), None) =
                (it.next(), it.next(), it.next(), it.next())
            {
                defaults.entry(command).or_default().push(chord);
            }
        }

        for s in SPECS {
            let Some(&reference_run_on_up) = run_on_up.get(s.name) else {
                panic!(
                    "{} is not a 1.12 binding at all — the tree is meant to be honest",
                    s.name
                );
            };
            assert_eq!(
                s.run_on_up(),
                reference_run_on_up,
                "{}: runOnUp disagrees with the install's Bindings.xml — that flag decides \
                 whether a press of this command delivers a release half (1295)",
                s.name
            );
            let ours: Vec<&str> = [s.d1, s.d2].into_iter().flatten().collect();
            assert_eq!(
                ours,
                defaults.get(s.name).cloned().unwrap_or_default(),
                "{}: default chords disagree with the install's DefaultBindings.wtf",
                s.name
            );
        }
    }
}
