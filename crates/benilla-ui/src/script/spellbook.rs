//! The spellbook (decision 0216 §8, slice 5) — the spell **source** for the cursor payload
//! system: a read-only book model the app builds from `PlayerActions.spells`
//! (`SMSG_INITIAL_SPELLS`), the same two-way seam shape as [`super::merchant`]/[`super::action`]:
//! the app pushes a **book snapshot** ([`UiScript::set_spellbook`] — tabs + the flat slot list,
//! already resolved to name/rank/icon/passive by the app's `benilla_formats::SpellCatalog` ×
//! skill-line join), and `CastSpell`/`PickupSpell` queue outbound intents the app drains
//! ([`UiScript::take_spell_casts`] / the cursor seam's own `CursorPayload::Spell` arm — decision
//! 0216 §1). The engine holds no spell KNOWLEDGE (icons/ranks/skill lines are the app's job) — a
//! slot is "a spell id, a name, a rank, a texture, and a passive bit".
//!
//! ## The book-id seam (decision 0218 §4's byte-verified 0-based book slot)
//!
//! The ref's own FrameXML computes a **1-based, per-tab-cumulative "book id"**
//! (`SpellBookFrame.lua`'s `SpellBook_GetSpellID`: `buttonId + tabOffset + 12*(page-1)`, where
//! `buttonId` is a spell button's own 1..12 `id=` attribute and `tabOffset` is
//! `GetSpellTabInfo`'s own `offset` return) and passes that SAME id, unmodified, to every one of
//! `GetSpellName`/`GetSpellTexture`/`IsSpellPassive`/`CastSpell`/`PickupSpell`. 0218 §4 byte-read
//! `PickupSpell`'s own argument as a **0-based book slot** — so the real client's Lua↔C++ glue
//! does the `-1` itself, invisibly to FrameXML. This engine keeps the ref's exact Lua-facing
//! convention (every binding below takes the SAME 1-based-cumulative `id` a transcribed
//! `SpellBookFrame.xml` computes and passes verbatim, so the transcription needs no special
//! casing) and does the byte-verified `-1` at THIS one seam ([`slot_index`]) before indexing
//! [`SpellBookState::slots`] (0-based, flat, tab order). `GetSpellTabInfo`'s pushed `offset` is
//! therefore exactly each tab's 0-based START index into `slots` — the app computes it as the
//! running sum of every earlier tab's `num_spells` (tab 1's is `0`, so its first spell's book id
//! is `1`, matching the ref's own "first tab's first spell is id 1").
//!
//! ## The pet book (decision 1032 — live; 0216 §8's deferral is retired)
//!
//! `BOOKTYPE_PET` selects a **second slot list** ([`PetBookState`]), fed from `SMSG_PET_SPELLS`'
//! own spell tail. Every `bookType`-taking binding is a two-way fork ([`book_slot`]) exactly as the
//! reference's are — `isPet ? [0xb6f098 + 4*i] : [0xb700f0 + 4*i]`, written out once per binding —
//! and the three pet-only bindings (`HasPetSpells`, `GetSpellAutocast`, `ToggleSpellAutocast`) live
//! here with them.
//!
//! Two asymmetries are the API and not tidiable away:
//!
//! - `GetNumSpellTabs`/`GetSpellTabInfo` take **no** `bookType` and only ever answer the player's
//!   skill lines. That is the reference's own signature: the pet book has no tabs, and
//!   `SpellBookFrame_Update` hides the whole skill-line strip while it is up.
//! - `PickupSpell` and `CastSpell` produce a **different kind of thing** on the pet side — a pet
//!   action word on the cursor (`0x494e20`, cursor modes 1-7) and a `CMSG_PET_ACTION` on the wire
//!   (`0x4b34ce`) — rather than a spell payload and a player cast. See each binding.
//!
//! `BOOKTYPE_SPELL`/`BOOKTYPE_PET` are installed as plain Lua globals here rather than left to the
//! transcribed XML's own `<Script>` block (the ref's actual home for them, `SpellBookFrame.lua:
//! 5-6`, and this crate's usual house rule for Era top-level constants) — the one deliberate
//! exception, so this module's OWN engine-level tests can drive the pet-deferral path without
//! loading a real `SpellBookFrame.xml`.

use mlua::{Lua, MultiValue, Value};

use super::cursor::{queue_cursor_update, CursorPayload, CursorSpell};
use super::Model;

const BOOKTYPE_SPELL: &str = "spell";
const BOOKTYPE_PET: &str = "pet";

/// `HasPetSpells`' second return when the app has not resolved a token — the reference's own
/// literal at `0x846a40`, pushed by `0x4b44a6` whenever the player object fails to resolve. Never
/// nil: FrameXML concatenates it (`"PET_TYPE_"..token`), which would error on one.
const PET_TOKEN_FALLBACK: &str = "PET";

/// One skill-line tab (`GetSpellTabInfo`'s own Era tuple shape). `offset` is the tab's 0-based
/// START index into [`SpellBookState::slots`] (module docs' book-id seam) — pushed by the app,
/// trusted here (the engine holds no spell knowledge to derive it from itself).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpellTabView {
    pub name: String,
    pub texture: Option<String>,
    pub offset: u32,
    pub num_spells: u32,
}

/// One spell in the flat book (0-based [`SpellBookState::slots`] index; module docs' book-id
/// seam). Every field is pre-resolved by the app — the engine draws whatever it's given.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpellSlotView {
    pub spell_id: u32,
    pub name: String,
    /// The rank/subtext line (`Spell.dbc`'s `NameSubtext`, `benilla-formats`' own pin); `None`
    /// shows no second line.
    pub rank: Option<String>,
    pub texture: Option<String>,
    /// `SPELL_ATTR_PASSIVE` (`benilla-formats`' `SpellDisplay::passive`) — grays the name in the
    /// transcribed XML and refuses both [`CastSpell`]-family casts (this module) and, faithfully,
    /// nothing else: a passive can still be picked up/placed on a bar (the ref never blocks that).
    pub passive: bool,
    /// The `IsCurrentCast` verdict for this slot — the checked ring (`SpellButton_UpdateSelection`'s
    /// gold glow). The delegate `0x4b3600` has exactly two arms (wow-re
    /// `spellbook-checked-predicate.md`): a shapeshift spell whose form is the player's current
    /// form byte, or the open trade-skill window's own spell — never an ordinary in-flight cast.
    /// App-resolved (`benilla::ui_spellbook`), pushed with the book; the app fires
    /// `CURRENT_SPELL_CAST_CHANGED` on its edges.
    pub current: bool,
    /// The spell's running cooldown as `(start_ms on the GetTime clock, duration_ms, enabled)` —
    /// the same app-computed triple [`super::ActionState::cooldown`] and the container slots
    /// carry, resolved by the ONE cooldown store (`benilla::cooldowns::Cooldowns::info` — id,
    /// category and GCD reads alike); `GetSpellCooldown` answers the reference's
    /// `(start, duration, enable)`. `None` = cold. Frame-stable per arm (the absolute start), so
    /// a running cooldown never churns the book diff.
    pub cooldown: Option<(i64, u32, bool)>,
    /// `GetSpellAutocast`'s `(allowed, enabled)` pair — **pet-book only**, and `None` is not
    /// "neither": it is the reference's own player-book answer, `(nil, nil)`, because `0x4b4180`
    /// short-circuits on the book flag (`0x4b41cb`/`0x4b41d6`) before it looks a spell up at all.
    /// Read off the pet's **raw** word (`0x4bd160` → bits 31/30), never off the filtered book.
    pub autocast: Option<(bool, bool)>,
    /// The pet slot's packed word **verbatim** — what `PickupSpell(id, "pet")` puts on the cursor.
    /// `0x4b3260`'s pet arm hands `0x494e20` a *pointer* to this very word, so the payload is the
    /// server's own dword, type byte and autocast bits included, not a synthesized one. `0` for a
    /// player-book slot, which has no word.
    pub packed: u32,
}

/// The **pet's** book — the reference's second flat array (`0xb6f098`, count `0xb71174`), which is
/// a genuinely different object from the player's rather than a variant of it:
///
/// - **no tabs.** `GetNumSpellTabs`/`GetSpellTabInfo` take no `bookType` and only ever answer the
///   player's skill lines; `SpellBookFrame_Update` hides every skill-line tab while the pet book is
///   up (`SpellBookFrame.lua:124`), and `SpellBook_GetSpellID` returns the button's own 1..12 id
///   with no tab offset at all (`l.460-462`).
/// - **a different add-gate.** `0x4b2f90` admits a spell iff it resolves in `Spell.dbc` **and**
///   `Attributes & 0x80` (DO_NOT_DISPLAY) is clear — `0x4b2fa8 mov dl,[rec+0x18]; test dl,dl; js`.
///   That is *one* of the three tests the player book's own gate makes: no `IS_TRADESKILL` leg and
///   no `castUI == 0` leg. Reusing the player book's gate here would be the same class of mistake
///   as reusing its tab routing.
/// - **the same order.** `0x4b2fd0(ecx = 0, edx = 1)` sorts it with `0x4b30c0`, the player book's
///   own comparator (name, then parsed rank), and tail-jumps `SPELLS_CHANGED`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PetBookState {
    /// `HasPetSpells`'s second return: `ChrClasses.dbc` field 4 for the player's class — `"PET"`
    /// or `"DEMON"` (`benilla_formats::PetNameTokens`). A **key**, not display text: FrameXML does
    /// `getglobal("PET_TYPE_"..token)`. `None` only while there is no book at all.
    pub token: Option<String>,
    pub slots: Vec<SpellSlotView>,
}

/// The player's known-spell book: tabs (skill lines) + the flat slot list every tab indexes into
/// (module docs). Durable player state, not a session window (like [`super::action`]'s
/// `actions` map) — never `Option`; "no known spells yet" is simply empty vectors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpellBookState {
    pub tabs: Vec<SpellTabView>,
    pub slots: Vec<SpellSlotView>,
}

impl super::UiScript {
    /// Push the whole book snapshot (tabs + flat slots), replacing whatever was there. A bare
    /// setter — firing `SPELLS_CHANGED` is the app's own diff-and-fire job (mirroring
    /// `set_action`/`set_container`; never auto-fired here).
    pub fn set_spellbook(&mut self, state: SpellBookState) {
        self.model_mut().spellbook = state;
    }

    /// Push the pet's book ([`PetBookState`]), replacing whatever was there. A bare setter for the
    /// same reason as its sibling: the reference fires `SPELLS_CHANGED` for **both** books off the
    /// one re-sort (`0x4b2fd0` → `SignalEvent(0x104)`), and whose diff moved is the app's to know.
    pub fn set_pet_book(&mut self, state: PetBookState) {
        self.model_mut().pet_book = state;
    }

    /// Drain the pet spell ids `CastSpell(id, "pet")` queued. Separate from
    /// [`Self::take_spell_casts`] because the wire verb is different in kind: the dispatcher's pet
    /// arm sends **`CMSG_PET_ACTION`** with a synthesized type-1 word (`0x4b34ce`), not a player
    /// cast, so folding them into one queue would lose which end of the leash the cast came from.
    pub fn take_pet_spell_casts(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().pet_spell_casts)
    }

    /// Drain the pet spell ids `ToggleSpellAutocast` queued — the pet **book**'s autocast verb,
    /// which is a different opcode from the pet **bar**'s ([`super::pet::UiScript::…`]'s
    /// `take_pet_autocast_toggles` → `CMSG_PET_SET_ACTION`): this one is
    /// `CMSG_PET_SPELL_AUTOCAST 0x2F3` and names a spell id rather than a bar slot
    /// (`0x4b4291` → `0x4bccb0`).
    pub fn take_pet_spell_autocasts(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().pet_spell_autocasts)
    }

    /// Read the book back — for the ONE app-side consumer that must resolve a spell name by the
    /// same law `CastSpellByName` does: a macro's bound spell (`benilla::ui_macro`, decision
    /// 0983). Going through the pushed book rather than re-deriving from the catalog is what
    /// stops the bar's cooldown swirl and the macro's own cast disagreeing about which rank a
    /// bare `/cast Fireball` means.
    pub fn spellbook(&self) -> SpellBookState {
        self.model_mut().spellbook.clone()
    }

    /// Drain the spell ids `CastSpell` queued since the last call.
    pub fn take_spell_casts(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().spell_casts)
    }

    /// Push whether the app's cast lifecycle holds something `SpellStopCasting()` can stop — a
    /// running auto-repeat or an in-flight cast, but NOT a channel (the ref's `0x6e6e80` reads
    /// only the auto-repeat key `0xceac30` and the inflight id `0xceca88`, and the inflight id
    /// is already 0 during a channel — wow-re `esc-stopcasting.md`). Pushed each frame by the
    /// app's cast feed (`benilla::ui_cast`), before the input pass runs the ESC chain.
    pub fn set_casting(&mut self, casting: bool) {
        self.model_mut().casting = casting;
    }

    /// Drain the `SpellStopCasting()` trigger: `true` if it fired on a stoppable state since
    /// the last call — the ESC leg of the local self-cancel (`benilla::ui_cast` resolves WHICH
    /// thing dies: auto-repeat first, else the in-flight cast — the ref's branch order).
    pub fn take_spell_stop(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().spell_stop)
    }

    /// Push whether the app's spell-targeting cursor mode is active (decision 0792) — what
    /// `SpellIsTargeting()` reads and `SpellStopTargeting()` gates on. Pushed each frame by the
    /// app's targeting feed (`benilla::ui_action`), before the input pass runs the ESC chain.
    pub fn set_spell_targeting(&mut self, targeting: bool) {
        self.model_mut().spell_targeting = targeting;
    }

    /// Drain the `SpellStopTargeting()` trigger: `true` if it fired while targeting since the
    /// last call — the ESC-chain rung (`UIParent.lua:1490`); the app clears its targeting mode.
    pub fn take_stop_targeting(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().spell_stop_targeting)
    }
}

/// Which book a `bookType` argument names — the reference's own one-line test, and it is a
/// **case-insensitive compare against `"pet"` alone** (`0x4b3f27` → `SStrCmpI(arg2, "pet")`), so
/// every other string, `"spell"` included, is the player's book. Reproduced rather than tightened:
/// the shared parser also *requires* a string second argument (`0x4b3ee8 lua_isstring(2)`), which
/// is why these bindings take `String` and not `Option<String>`.
fn is_pet_book(book_type: &str) -> bool {
    book_type.eq_ignore_ascii_case(BOOKTYPE_PET)
}

/// The book-id → 0-based slot-list index seam (module docs). `None` for an id of `0` (the ref's
/// ids start at 1, so `id - 1` would otherwise underflow) — the reference's own `arg1 - 1` with its
/// `[0, 0x400)` bound, which our slot lists enforce by being shorter than that anyway.
fn slot_index(id: u32) -> Option<usize> {
    usize::try_from(id.checked_sub(1)?).ok()
}

/// The one lookup every `bookType`-taking binding shares: pick the book, then the slot. This is
/// literally the reference's shape — `isPet ? [0xb6f098 + 4*i] : [0xb700f0 + 4*i]`, one fork
/// repeated verbatim inside each binding (`0x4b3f5d`, `0x4b40e6`, `0x4b3735`, `0x4b3339`, …).
///
/// Shared with the tooltip channel (`super::tooltip_spell`'s `SetSpell`), which is a `GameTooltip`
/// method rather than a global but repeats the identical fork at `0x532e1c`/`0x532e2a` — and which
/// read the player's book for a pet hover until 1050, because it took the argument and dropped it.
pub(super) fn book_slot<'a>(
    model: &'a Model,
    id: u32,
    book_type: &str,
) -> Option<&'a SpellSlotView> {
    let slots = if is_pet_book(book_type) {
        &model.pet_book.slots
    } else {
        &model.spellbook.slots
    };
    slots.get(slot_index(id)?)
}

/// Resolve a spell **by name** against the player's book — the law behind `CastSpellByName` and,
/// through it, `/cast` and every macro's `/cast` line (decision 0983).
///
/// The grammar is the one the client documents in its own help text:
/// `MACRO_HELP_TEXT_LINE4 = "- To cast a spell from a macro use the following syntax:
/// /cast <name> (<subtext>)"`. So:
///
/// - **`Fireball`** — the highest-ranked *known* Fireball. Rank order is the book's own
///   `NameSubtext` number (`"Rank 8"` → 8) through the reference's own leading-number parse
///   ([`super::super::…`]'s twin lives in `benilla::ui_spellbook`); an unranked subtext sorts as 0,
///   and ties fall to the later book slot, which is the order the book itself lists ranks in.
/// - **`Fireball(Rank 1)`** / **`Fireball (Rank 1)`** — that exact subtext, case-insensitively.
///   Both spacings, because vanilla macros in the wild are written both ways and the reference's
///   own help text prints the spaced form while its FrameXML never re-spaces the argument.
///
/// Name matching is case-insensitive and whole-name — a *prefix* rule would silently cast
/// "Frostbolt" for "Frost" and there is nothing in the reference suggesting one. A spell the
/// player does not know resolves to `None` and the cast simply does not happen: the reference has
/// no error line for it either (`SlashCmdList["CAST"]` discards the binding's result).
///
/// Passives are skipped, matching [`pickup_spell`]'s sibling rule in `CastSpell`: a passive is
/// permanent player state, never something a macro casts.
pub fn resolve_spell_by_name<'a>(
    book: &'a SpellBookState,
    query: &str,
) -> Option<&'a SpellSlotView> {
    let (name, subtext) = split_subtext(query);
    if name.is_empty() {
        return None;
    }
    book.slots
        .iter()
        .filter(|s| !s.passive && s.name.eq_ignore_ascii_case(name))
        .filter(|s| match subtext {
            Some(want) => s
                .rank
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case(want)),
            None => true,
        })
        // Highest rank wins when no subtext pinned one; the book's own later slot breaks a tie.
        .enumerate()
        .max_by_key(|(i, s)| (rank_number(s.rank.as_deref()), *i))
        .map(|(_, s)| s)
}

/// `Name(Subtext)` / `Name (Subtext)` → `("Name", Some("Subtext"))`; a bare name → `(name, None)`.
/// An unclosed parenthesis is not a subtext — the whole string stays the name, so a spell whose
/// own name holds a `(` cannot be silently truncated.
fn split_subtext(query: &str) -> (&str, Option<&str>) {
    let q = query.trim();
    let Some(open) = q.find('(') else {
        return (q, None);
    };
    let Some(close) = q.rfind(')') else {
        return (q, None);
    };
    if close < open {
        return (q, None);
    }
    (q[..open].trim(), Some(q[open + 1..close].trim()))
}

/// The rank number inside a `NameSubtext` (`"Rank 8"` → 8), by the reference's own leading-number
/// parse: skip to the first digit, fold the digit run. A subtext with no digits (`"Racial"`,
/// `"Passive"`) and an absent one both read 0.
fn rank_number(subtext: Option<&str>) -> u32 {
    subtext
        .unwrap_or("")
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .fold(0u32, |acc, c| acc * 10 + c.to_digit(10).unwrap_or(0))
}

/// `PickupSpell(id, bookType)` — the drag/shift-click entry point (ref `SpellButton_OnClick`'s
/// other two forks, `SpellBookFrame.lua:263-290`). The book is a SOURCE, never a placement
/// target — the ref's plain click always casts unconditionally, never checking `GetCursorInfo`
/// first (unlike `UseAction`'s `checkCursor` fork) — so this refuses outright when the cursor
/// already holds ANYTHING rather than silently discarding it: the doll's own refusal precedent
/// (`cursor::doll::pickup_inventory_item`) for a payload with nowhere faithful to go, since a
/// spell button is not a fit-checked drop target the way a doll slot or bar button is.
fn pickup_spell(model: &mut Model, id: u32, book_type: &str) -> bool {
    if model.cursor.is_some() {
        return false;
    }
    let Some(slot) = book_slot(model, id, book_type) else {
        return false;
    };
    // **The pet book's payload is a different KIND**, not a Spell payload with a flag on it: the
    // pet arm of `0x4b3260` calls `0x494e20` with a pointer to the pet's raw word (cursor modes
    // 1-7), while the player arm calls `0x494d20` with a spell id (mode 9). That is why a pet
    // spell can be dropped onto the pet bar and a player spell cannot — the bar's drop accepts
    // exactly one payload kind, and the book is the *second* place that kind is produced.
    let payload = if is_pet_book(book_type) {
        // `0x494e20`'s own jump table refuses type 0 and type >= 8, so a word that could not sit
        // on the bar cannot ride the cursor either (`cursor::pet::payload_word`, same rule).
        let packed = slot.packed;
        if !(1..=7).contains(&((packed >> 24) & 0x3F)) {
            return false;
        }
        CursorPayload::PetAction(super::cursor::CursorPetAction {
            // No source slot: this word came out of the BOOK, not off the bar, so there is
            // nothing to blank behind it and nothing to swap back to. The reference is the same
            // shape — its cursor holds a pointer into the raw spell array, not into the bar.
            src_slot: 0,
            packed,
            passive: slot.passive,
            texture: slot.texture.clone(),
        })
    } else {
        CursorPayload::Spell(CursorSpell {
            book_slot: id,
            book_type: book_type.to_string(),
            spell_id: slot.spell_id,
            texture: slot.texture.clone(),
            passive: slot.passive,
        })
    };
    model.cursor = Some(payload);
    queue_cursor_update(model);
    true
}

/// Register the spellbook globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    g.set("BOOKTYPE_SPELL", BOOKTYPE_SPELL)?;
    g.set("BOOKTYPE_PET", BOOKTYPE_PET)?;

    /// The 1/nil boolean every Era binding in this file answers with.
    fn flag(b: bool) -> Value {
        if b {
            Value::Integer(1)
        } else {
            Value::Nil
        }
    }

    g.set(
        "GetNumSpellTabs",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.spellbook.tabs.len() as i64)
        })?,
    )?;

    // GetSpellTabInfo(i) -> name, texture, offset, numSpells (the Era flat tuple); 1-based `i`,
    // out of range -> a single nil (GetMerchantItemInfo's own out-of-range shape).
    g.set(
        "GetSpellTabInfo",
        lua.create_function(|lua, i: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(tab) = i.checked_sub(1).and_then(|n| model.spellbook.tabs.get(n)) else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let texture = match &tab.texture {
                Some(t) => Value::String(lua.create_string(t)?),
                None => Value::Nil,
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&tab.name)?),
                texture,
                Value::Integer(i64::from(tab.offset)),
                Value::Integer(i64::from(tab.num_spells)),
            ]))
        })?,
    )?;

    // GetSpellName(id, bookType) -> name, rank. **Always TWO values, never one, and the rank of a
    // rankless spell is the EMPTY STRING, never nil** (wow-re
    // `scratch/getspellname-return-contract.md`, §5-derived from the bytes alone).
    //
    // Both returns go through the same push helper with **no rank-specific branch anywhere in the
    // binding** — a ranked and a rankless spell run byte-identical code: `0x4b4063 call 0x6f3890`
    // pushes `SpellRec.Name[locale]`, `0x4b4076` pushes `NameSubtext[locale]`, and the single
    // DBC-resolved exit is `mov eax,0x2` at `0x4b407c`. `0x6f3890` decides on **pointer nullity
    // alone** (`test edx,edx`), and the DBC pointer is never NULL: `SpellRec::Read 0x583750` fixes
    // up every string column with an unconditional `add offset, stringBlockBase`, so an on-disk
    // offset of 0 materializes as a pointer to the string block's own byte 0 — a NUL. `0x6f3840`
    // then writes tag 4 (`LUA_TSTRING`) unconditionally; `len == 0` is not a special case. So the
    // rankless rank is a real, interned, zero-length Lua string.
    //
    // This is not an edge case: **14,403 of the shipped Spell.dbc's 22,357 rows carry NameSubtext
    // offset 0** — 64%, `Attack` (6603) among them. Our `Option<String>::None` pushed nil there,
    // and two 1.12 corpus addons walking the whole book die on it in two different idioms:
    // `Roid-Macros/Generic.lua:35` uses the rank as a TABLE KEY (nil is illegal), and
    // `CT_MasterMod/CT_Master.lua:16` passes it straight to `string.find` (nil raises). Both were
    // invisible until the survey seated a spellbook.
    //
    // The `Option` is still the right shape for the MODEL — it records "this spell has no
    // NameSubtext", which is a fact about the DBC row. What was wrong was rendering that absence
    // as nil at the Lua boundary; the reference renders it as the empty string the pointer targets.
    g.set(
        "GetSpellName",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            // An out-of-range index RAISES rather than answering nil. `0x4b3ec0` gates the index to
            // `[0, 0x400)` (`0x4b3f0a`/`0x4b3f11`) before the binding proper runs, and the miss arm
            // is `luaL_error("Invalid spell slot in GetSpellName")`, which longjmps and abandons
            // the caller's statement (`super::binding_abi`). The in-binding bound check at
            // `0x4b4018` is unreachable-as-taken. This file previously assumed the bound was
            // "enforced by our slot lists being shorter than that anyway" — a short list answers
            // nil, which is a different thing entirely.
            if id >= 0x400 {
                return Err(mlua::Error::runtime("Invalid spell slot in GetSpellName"));
            }
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(slot) = book_slot(&model, id, &book_type) else {
                // An unfilled slot inside the range answers **two** nils (`0x4b4086` → `mov eax,0x2`
                // at `0x4b4095`), not one — which `select('#', …)` and a two-name assignment can
                // both tell apart.
                return Ok(MultiValue::from_vec(vec![Value::Nil, Value::Nil]));
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&slot.name)?),
                Value::String(lua.create_string(slot.rank.as_deref().unwrap_or(""))?),
            ]))
        })?,
    )?;

    g.set(
        "GetSpellTexture",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let tex = book_slot(&model, id, &book_type).and_then(|s| s.texture.clone());
            match tex {
                Some(t) => Ok(Value::String(lua.create_string(&t)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // IsCurrentCast(id, bookType) — the spellbook button's checked ring (binding `0x4b4370` →
    // delegate `0x4b3600`; ref
    // `SpellButton_UpdateSelection` SetChecks on it). The verdict itself is app-resolved per slot
    // ([`SpellSlotView::current`]); this only reads it back.
    g.set(
        "IsCurrentCast",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let current = book_slot(&model, id, &book_type).is_some_and(|s| s.current);
            // The ref's binding convention: 1 or nil, never false.
            match current {
                true => Ok(Value::Integer(1)),
                false => Ok(Value::Nil),
            }
        })?,
    )?;

    // GetSpellCooldown(id, bookType) → start, duration, enable — the book twin of
    // `GetActionCooldown`/`GetContainerItemCooldown`, identical conventions: `GetTime`-clock
    // `(seconds, seconds, 0/1)`, enable 0 = an on-hold record (parked, full duration), and the
    // cold-at-expiry guard so an event-driven re-feed can never replay the finish flash. Pet or
    // out-of-range answers the cold `(0, 0, 1)` — the ref's own no-cooldown shape.
    g.set(
        "GetSpellCooldown",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            let now: f64 = lua.globals().get("__benilla_now").unwrap_or(0.0);
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let cooldown = book_slot(&model, id, &book_type).and_then(|s| s.cooldown);
            Ok(match cooldown {
                Some((start_ms, duration_ms, enabled)) => {
                    let (start, duration) =
                        (start_ms as f64 / 1000.0, f64::from(duration_ms) / 1000.0);
                    if start + duration > now || !enabled {
                        (start, duration, i32::from(enabled))
                    } else {
                        (0.0, 0.0, 1)
                    }
                }
                None => (0.0, 0.0, 1),
            })
        })?,
    )?;

    g.set(
        "IsSpellPassive",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(book_slot(&model, id, &book_type).is_some_and(|s| s.passive))
        })?,
    )?;

    // CastSpell(id, bookType) — the plain click (ref SpellButton_OnClick's `else` branch): queues
    // the resolved spell id UNLESS the slot is passive (module doc: a passive is permanent player
    // state, never something the player casts) or bookType/id resolve to nothing.
    //
    // **The pet arm is a different verb, not a flag.** `0x4b3300`'s tail forks on the book byte
    // one instruction before the send (`0x4b34c8 cmp ecx, 0; je` → the player's own cast
    // `0x6e5a90`), and the pet side builds `CMSG_PET_ACTION 0x175` by hand:
    // `{ u64 [0xb714a0], u32 (spellId & 0xFFFF) | 0x01000000, u64 target }` (`0x4b34ce`-`0x4b3524`)
    // — a **synthesized type-1 word**, which is why the pet book can cast a spell that is not on
    // the bar at all. The target is the passed one, falling back to the current selection
    // (`0x4b34af`-`0x4b34bb`), exactly as `CastPetAction` does; the app supplies it at the drain.
    g.set(
        "CastSpell",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(slot) = book_slot(&model, id, &book_type) {
                if !slot.passive {
                    let spell_id = slot.spell_id;
                    if is_pet_book(&book_type) {
                        model.pet_spell_casts.push(spell_id);
                    } else {
                        model.spell_casts.push(spell_id);
                    }
                }
            }
            Ok(())
        })?,
    )?;

    // HasPetSpells() → numPetSpells, petToken — no arguments, and **always exactly two returns**
    // (`0x4b4410`, `EAX = 2` on every path). Zero spells answers `(nil, nil)` (`0x4b4420`), which
    // is the gate `ToggleSpellBook` and `SpellBookFrame_Update` both read: no pet book, no tab row.
    //
    // Return 1 is the **count as a number**, not a boolean — `SpellBook_GetCurrentPage` divides by
    // it (`ceil(numPetSpells/12)`), so answering 1/nil would silently pin the pet book to one page.
    g.set(
        "HasPetSpells",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let n = model.pet_book.slots.len();
            if n == 0 {
                return Ok(MultiValue::from_vec(vec![Value::Nil, Value::Nil]));
            }
            let token = match &model.pet_book.token {
                Some(t) => Value::String(lua.create_string(t)?),
                // The reference's own unresolved-player arm pushes the literal "PET"
                // (`0x4b44a6`), never nil — a nil here would make `"PET_TYPE_"..token` error.
                None => Value::String(lua.create_string(PET_TOKEN_FALLBACK)?),
            };
            Ok(MultiValue::from_vec(vec![Value::Integer(n as i64), token]))
        })?,
    )?;

    // GetSpellAutocast(id, bookType) → autoCastAllowed, autoCastEnabled (1/nil each) — the
    // AutoCastable overlay and the sparkle model on a pet book button.
    //
    // **Pet-only, and it fails to (nil, nil) rather than (nil) for the player book**: `0x4b4180`
    // tests the book flag twice (`0x4b41cb`, `0x4b41d6`) and falls into the same two nil pushes the
    // no-record path uses, so the arity is 2 on every path including a bad index.
    g.set(
        "GetSpellAutocast",
        lua.create_function(move |lua, (id, book_type): (u32, String)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let (allowed, enabled) = book_slot(&model, id, &book_type)
                .filter(|_| is_pet_book(&book_type))
                .and_then(|s| s.autocast)
                .unwrap_or((false, false));
            Ok((flag(allowed), flag(enabled)))
        })?,
    )?;

    // ToggleSpellAutocast(id, bookType) — the pet book's right click (ref `SpellButton_OnClick`'s
    // `arg1 ~= "LeftButton"` fork). **A different binding and a different opcode from the pet
    // BAR's `TogglePetAutocast`**: `0x4b4240` indexes the pet spellbook and calls `0x4bccb0`, which
    // sends `CMSG_PET_SPELL_AUTOCAST 0x2F3` naming a **spell id**, where the bar's verb sends
    // `CMSG_PET_SET_ACTION` naming a slot. Confusing them is a wire bug that looks like a UI one.
    //
    // The gate here is the same one `0x4bccb0` applies before it sends: the word must be
    // autocast-ALLOWED (`0x4bccf5 test cl,1`). Everything else the sender does — flipping bit 30
    // in place and mirroring it onto every bar slot carrying the same action — is state the app
    // owns, so it happens at the drain.
    g.set(
        "ToggleSpellAutocast",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let spell_id = book_slot(&model, id, &book_type)
                .filter(|_| is_pet_book(&book_type))
                .filter(|s| s.autocast.is_some_and(|(allowed, _)| allowed))
                .map(|s| s.spell_id);
            if let Some(spell_id) = spell_id {
                model.pet_spell_autocasts.push(spell_id);
            }
            Ok(())
        })?,
    )?;

    // CastSpellByName(name [, onSelf]) — the reference binding `0x4b4ab0`, whose only two callers
    // share the `0x4b3300` dispatcher with `CastSpell` above (wow-re ledger), so this queues onto
    // the same `spell_casts` list and the app's one cast tail handles both. `SlashCmdList["CAST"]`
    // is literally `CastSpellByName(msg)`, which is why `/cast` needs nothing else, and it is the
    // command the whole macro system is built to run.
    //
    // `onSelf` is accepted and carried no further (benilla has no self-cast modifier yet — the
    // same named gap `UseAction`'s third argument already has).
    g.set(
        "CastSpellByName",
        lua.create_function(|lua, (name, _on_self): (String, MultiValue)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(slot) = resolve_spell_by_name(&model.spellbook, &name) {
                let spell_id = slot.spell_id;
                model.spell_casts.push(spell_id);
            }
            Ok(())
        })?,
    )?;

    g.set(
        "PickupSpell",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            Ok(pickup_spell(&mut model, id, &book_type))
        })?,
    )?;

    // SpellStopCasting() — the ref's Script::SpellStopCasting (`0x6e6e80`, §5-verified whole,
    // wow-re `esc-stopcasting.md`): stop the FIRST of {running auto-repeat (`0x6ea080`,
    // CMSG_CANCEL_AUTO_REPEAT_SPELL), in-flight cast (`AbortCast` → CMSG_CANCEL_CAST)} and
    // return 1; nil when neither runs. A CHANNEL is nil — the body's whole callee closure
    // never reaches the channel canceler `0x6e9b70`, and the inflight id `0xceca88` it gates
    // on is already 0 mid-channel (the launch CAST_RESULT(OKAY) clears it at `0x6e7408`) —
    // the vanilla "/stopcasting can't stop a channel" quirk, kept faithfully. The falsy leg is
    // load-bearing ground truth from the artifact: `ToggleGameMenu`'s ESC chain (extracted
    // `UIParent.lua:1489`, `elseif ( SpellStopCasting() ) then`) only reaches
    // `CloseAllWindows()`/the game menu through nil, so an unconditional true would eat every
    // ESC press forever. The host feeds the stoppable mirror (`set_casting`) and resolves the
    // branch order at the drain (`benilla::ui_cast::local_self_cancel`).
    g.set(
        "SpellStopCasting",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if model.casting {
                model.spell_stop = true;
                Ok(Value::Integer(1))
            } else {
                Ok(Value::Nil)
            }
        })?,
    )?;

    // SpellIsTargeting() — the ref's Script::SpellIsTargeting (`0x6e6cd0`, wow-re
    // `wave-cast.md`): true while the targeting cursor is up (`flag_word != 0`), nil otherwise.
    // Read by FrameXML (PetFrame's right-click bind fork) and by the ESC chain's callers.
    g.set(
        "SpellIsTargeting",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            if model.spell_targeting {
                Ok(Value::Boolean(true))
            } else {
                Ok(Value::Nil)
            }
        })?,
    )?;

    // SpellCanTargetUnit("unit") — the ref's `0x6e6d00`: resolve the token, then ask `0x6e6460`'s
    // UNIT leg whether the standing word can bind it. Its one shipped caller is
    // `UnitFrame_OnEnter`, and it is what picks CAST_CURSOR over CAST_ERROR_CURSOR — the only
    // lit/grey cursor split over a UI element in 1.12.
    //
    // The token is not consulted yet, and that is honest rather than lazy: the answer is `false`
    // for **every** unit while any word benilla can arm is standing (location / item / gameobject —
    // no unit satisfies those), so no token can change it. The app derives the flag from the word
    // itself, so this starts discriminating by unit the moment the residual unit-word machine
    // lands rather than silently staying wrong.
    g.set(
        "SpellCanTargetUnit",
        lua.create_function(|lua, _unit: Option<String>| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            if model.spell_can_target_unit {
                Ok(Value::Boolean(true))
            } else {
                Ok(Value::Nil)
            }
        })?,
    )?;

    // SpellStopTargeting() — the ref's Script::SpellStopTargeting (`0x6e6e30`: if IsTargeting →
    // StopTargeting `0x6e4900` → AbortCast(0x1c), which in targeting mode just clears the word,
    // no packet). The 1/nil return is load-bearing exactly like SpellStopCasting's above: the
    // ESC chain's rung (`UIParent.lua:1490`, `elseif ( SpellStopTargeting() ) then`) must fall
    // through to the game menu when nothing was targeting. The host drains the trigger
    // (`benilla::ui_action::targeting`) and clears its mode.
    g.set(
        "SpellStopTargeting",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if model.spell_targeting {
                model.spell_stop_targeting = true;
                Ok(Value::Integer(1))
            } else {
                Ok(Value::Nil)
            }
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PetBookState, SpellBookState, SpellSlotView, SpellTabView};
    use crate::script::cursor::{CursorAction, CursorPayload};
    use crate::script::UiScript;

    /// Two tabs: "Fire" (2 spells: Fireball rank1 active, Fire Blast PASSIVE — an artificial
    /// fixture just to exercise the gray/refuse gate) and "Frost" (1 spell).
    fn book() -> SpellBookState {
        SpellBookState {
            tabs: vec![
                SpellTabView {
                    name: "Fire".into(),
                    texture: Some("Interface\\Icons\\Spell_Fire_FlameBolt".into()),
                    offset: 0,
                    num_spells: 2,
                },
                SpellTabView {
                    name: "Frost".into(),
                    texture: Some("Interface\\Icons\\Spell_Frost_FrostBolt02".into()),
                    offset: 2,
                    num_spells: 1,
                },
            ],
            slots: vec![
                SpellSlotView {
                    spell_id: 133,
                    name: "Fireball".into(),
                    rank: Some("Rank 1".into()),
                    texture: Some("Interface\\Icons\\Spell_Fire_FlameBolt".into()),
                    passive: false,
                    current: false,
                    cooldown: None,
                    ..Default::default()
                },
                SpellSlotView {
                    spell_id: 2136,
                    name: "Fire Blast".into(),
                    rank: Some("Rank 1".into()),
                    texture: Some("Interface\\Icons\\Spell_Fire_FireBolt02".into()),
                    passive: true, // artificial: exercises the refusal gate
                    current: false,
                    cooldown: None,
                    ..Default::default()
                },
                SpellSlotView {
                    spell_id: 168,
                    name: "Frost Armor".into(),
                    rank: Some("Rank 1".into()),
                    texture: Some("Interface\\Icons\\Spell_Frost_FrostArmor02".into()),
                    passive: false,
                    current: false,
                    cooldown: None,
                    ..Default::default()
                },
            ],
        }
    }

    /// `IsCurrentCast` reads the app-resolved per-slot verdict back as the ref's 1-or-nil.
    #[test]
    fn is_current_cast_reads_the_slot_verdict() {
        let mut s = UiScript::new().unwrap();
        assert!(s
            .eval::<bool>("return IsCurrentCast(1, BOOKTYPE_SPELL) == nil")
            .unwrap());
        let mut b = book();
        b.slots[0].current = true;
        s.set_spellbook(b);
        assert_eq!(
            s.eval::<i64>("return IsCurrentCast(1, BOOKTYPE_SPELL)")
                .unwrap(),
            1
        );
        assert!(s
            .eval::<bool>("return IsCurrentCast(2, BOOKTYPE_SPELL) == nil")
            .unwrap());
        assert!(s
            .eval::<bool>("return IsCurrentCast(1, BOOKTYPE_PET) == nil")
            .unwrap());
        assert!(s
            .eval::<bool>("return IsCurrentCast(99, BOOKTYPE_SPELL) == nil")
            .unwrap());
    }

    /// `GetSpellCooldown` reads the app-pushed per-slot triple back as the ref's GetTime-clock
    /// `(start, duration, enable)` — cold `(0, 0, 1)` for absent/pet/out-of-range, enable 0 for
    /// an on-hold record regardless of expiry, and the cold-at-expiry guard once
    /// `start + duration` passes (`GetActionCooldown`'s own conventions, the book twin).
    #[test]
    fn get_spell_cooldown_reads_the_slot_triple() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());
        s.tick(20.0); // GetTime = 20

        // Cold slot: the ref's no-cooldown shape.
        assert_eq!(
            s.eval::<(f64, f64, i64)>("return GetSpellCooldown(1, BOOKTYPE_SPELL)")
                .unwrap(),
            (0.0, 0.0, 1)
        );

        let mut b = book();
        b.slots[0].cooldown = Some((14_000, 10_000, true)); // running: 4 s elapsed of 10
        b.slots[1].cooldown = Some((2_000, 8_000, false)); // on hold: parked since t=2
        b.slots[2].cooldown = Some((5_000, 10_000, true)); // elapsed at t=15 — cold
        s.set_spellbook(b);
        assert_eq!(
            s.eval::<(f64, f64, i64)>("return GetSpellCooldown(1, BOOKTYPE_SPELL)")
                .unwrap(),
            (14.0, 10.0, 1)
        );
        // On hold survives the expiry guard (enable 0 = the parked "hasn't begun").
        assert_eq!(
            s.eval::<(f64, f64, i64)>("return GetSpellCooldown(2, BOOKTYPE_SPELL)")
                .unwrap(),
            (2.0, 8.0, 0)
        );
        // Elapsed goes cold — an event-driven re-feed can never replay the finish flash.
        assert_eq!(
            s.eval::<(f64, f64, i64)>("return GetSpellCooldown(3, BOOKTYPE_SPELL)")
                .unwrap(),
            (0.0, 0.0, 1)
        );
        // The pet deferral and out-of-range answer cold too.
        assert_eq!(
            s.eval::<(f64, f64, i64)>("return GetSpellCooldown(1, BOOKTYPE_PET)")
                .unwrap(),
            (0.0, 0.0, 1)
        );
        assert_eq!(
            s.eval::<(f64, f64, i64)>("return GetSpellCooldown(99, BOOKTYPE_SPELL)")
                .unwrap(),
            (0.0, 0.0, 1)
        );
    }

    #[test]
    fn tab_info_shapes_and_book_id_offsets() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(s.eval::<i64>("return GetNumSpellTabs()").unwrap(), 0);
        assert!(s.eval::<bool>("return GetSpellTabInfo(1) == nil").unwrap());

        s.set_spellbook(book());
        assert_eq!(s.eval::<i64>("return GetNumSpellTabs()").unwrap(), 2);

        let (name, texture, offset, num) = s
            .eval::<(String, String, i64, i64)>("return GetSpellTabInfo(1)")
            .unwrap();
        assert_eq!(
            (name.as_str(), texture.as_str(), offset, num),
            ("Fire", "Interface\\Icons\\Spell_Fire_FlameBolt", 0, 2)
        );
        let (name2, _tex2, offset2, num2) = s
            .eval::<(String, String, i64, i64)>("return GetSpellTabInfo(2)")
            .unwrap();
        assert_eq!((name2.as_str(), offset2, num2), ("Frost", 2, 1));

        // Out of range -> nil.
        assert!(s.eval::<bool>("return GetSpellTabInfo(3) == nil").unwrap());
    }

    #[test]
    fn name_and_rank_read_through_the_book_id_seam() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());

        // Book id 1 (tab 1 offset 0 + button 1) -> slot 0 -> Fireball.
        let (name, rank) = s
            .eval::<(String, String)>(r#"return GetSpellName(1, BOOKTYPE_SPELL)"#)
            .unwrap();
        assert_eq!((name.as_str(), rank.as_str()), ("Fireball", "Rank 1"));
        assert_eq!(
            s.eval::<String>(r#"return GetSpellTexture(1, BOOKTYPE_SPELL)"#)
                .unwrap(),
            "Interface\\Icons\\Spell_Fire_FlameBolt"
        );

        // Book id 3 (tab 2 offset 2 + button 1) -> slot 2 -> Frost Armor.
        let (name3, _rank3) = s
            .eval::<(String, String)>(r#"return GetSpellName(3, BOOKTYPE_SPELL)"#)
            .unwrap();
        assert_eq!(name3, "Frost Armor");

        // Out of range and the pet deferral both answer nil.
        assert!(s
            .eval::<bool>(r#"return GetSpellName(99, BOOKTYPE_SPELL) == nil"#)
            .unwrap());
        assert!(s
            .eval::<bool>(r#"return GetSpellName(1, BOOKTYPE_PET) == nil"#)
            .unwrap());
    }

    #[test]
    fn pickup_spell_payload_and_cursor_update() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());

        s.run(r#"picked = PickupSpell(1, BOOKTYPE_SPELL)"#).unwrap();
        assert!(s.eval::<bool>("return picked").unwrap());
        assert!(s.cursor_payload().is_some());
        let (kind, book_id, book, spell_id) = s
            .eval::<(String, i64, String, i64)>(
                "local k, slot, book, id = GetCursorInfo() return k, slot, book, id",
            )
            .unwrap();
        assert_eq!(
            (kind.as_str(), book_id, book.as_str(), spell_id),
            ("spell", 1, "spell", 133)
        );

        // CURSOR_UPDATE fired (the shared cursor seam, not duplicated here) — a listener sees it.
        // Tick first to flush the FIRST pickup's already-queued CURSOR_UPDATE before the listener
        // registers, so the count below is purely about the second (refused) call.
        s.tick(0.0);
        s.run(
            r#"
            cursorUpdates = 0
            local f = CreateFrame("Frame", "CursorListener")
            f:RegisterEvent("CURSOR_UPDATE")
            f:SetScript("OnEvent", function() cursorUpdates = cursorUpdates + 1 end)
            "#,
        )
        .unwrap();
        s.run(r#"PickupSpell(3, BOOKTYPE_SPELL)"#).unwrap(); // already holding -> refused, no-op
        s.tick(0.01);
        assert_eq!(
            s.eval::<i64>("return cursorUpdates").unwrap(),
            0,
            "refused pickup fires no CURSOR_UPDATE"
        );
        // Still holding spell 133 (book slot 1) from the first pickup — a refusal never clobbers
        // it (GetCursorInfo's Spell arm: kind, book_slot, book_type, spell_id).
        assert_eq!(
            s.eval::<(String, i64, String, i64)>(
                "local k, slot, book, id = GetCursorInfo() return k, slot, book, id"
            )
            .unwrap(),
            ("spell".to_string(), 1, "spell".to_string(), 133)
        );
    }

    #[test]
    fn pickup_spell_refuses_while_already_holding_any_payload() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());
        s.set_cursor_for_test(CursorPayload::Action(CursorAction {
            src_slot: 1,
            kind: 0,
            action: 111,
            texture: None,
        }));

        assert!(!s
            .eval::<bool>(r#"return PickupSpell(1, BOOKTYPE_SPELL)"#)
            .unwrap());
        // The original (action) payload survives untouched.
        assert_eq!(
            s.eval::<String>("local k = GetCursorInfo() return k")
                .unwrap(),
            "action"
        );
    }

    #[test]
    fn passive_refuses_the_cast_but_active_queues_it() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());

        s.run(r#"CastSpell(1, BOOKTYPE_SPELL)"#).unwrap(); // Fireball: active
        assert_eq!(s.take_spell_casts(), vec![133]);

        s.run(r#"CastSpell(2, BOOKTYPE_SPELL)"#).unwrap(); // Fire Blast: passive, refused
        assert!(s.take_spell_casts().is_empty());

        assert!(s
            .eval::<bool>(r#"return IsSpellPassive(2, BOOKTYPE_SPELL)"#)
            .unwrap());
        assert!(!s
            .eval::<bool>(r#"return IsSpellPassive(1, BOOKTYPE_SPELL)"#)
            .unwrap());
    }

    /// **With no pet book fed, every pet arm answers nothing** — the old deferral's behaviour,
    /// which is also the reference's whenever `[0xb71174] == 0`. Kept as its own case so the
    /// pet-book tests below can never pass by the player book leaking into them.
    #[test]
    fn an_absent_pet_book_answers_empty_everywhere() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());

        assert!(s
            .eval::<bool>(r#"return GetSpellName(1, BOOKTYPE_PET) == nil"#)
            .unwrap());
        assert!(s
            .eval::<bool>(r#"return GetSpellTexture(1, BOOKTYPE_PET) == nil"#)
            .unwrap());
        assert!(!s
            .eval::<bool>(r#"return IsSpellPassive(1, BOOKTYPE_PET)"#)
            .unwrap());
        assert!(s
            .eval::<bool>(r#"local n, t = HasPetSpells() return n == nil and t == nil"#)
            .unwrap());

        s.run(r#"CastSpell(1, BOOKTYPE_PET)"#).unwrap();
        assert!(s.take_spell_casts().is_empty(), "pet cast is a no-op");
        assert!(s.take_pet_spell_casts().is_empty());

        assert!(!s
            .eval::<bool>(r#"return PickupSpell(1, BOOKTYPE_PET)"#)
            .unwrap());
        assert!(s.cursor_payload().is_none(), "pet pickup is a no-op");
    }

    /// A hunter's pet book: Growl (autocastable, ON, on cooldown), Claw (autocastable, OFF) and
    /// Avoidance (a passive — no autocast, `ACT_PASSIVE 0x01`).
    fn pet_book() -> PetBookState {
        PetBookState {
            token: Some("PET".into()),
            slots: vec![
                SpellSlotView {
                    spell_id: 2649,
                    name: "Growl".into(),
                    rank: Some("Rank 1".into()),
                    texture: Some("Interface\\Icons\\Ability_Physical_Taunt".into()),
                    cooldown: Some((9400, 5000, true)),
                    autocast: Some((true, true)),
                    packed: 0xC100_0000 | 2649,
                    ..Default::default()
                },
                SpellSlotView {
                    spell_id: 16827,
                    name: "Claw".into(),
                    rank: Some("Rank 1".into()),
                    texture: Some("Interface\\Icons\\Ability_Druid_Rake".into()),
                    autocast: Some((true, false)),
                    packed: 0x8100_0000 | 16827,
                    ..Default::default()
                },
                SpellSlotView {
                    spell_id: 3025,
                    name: "Avoidance".into(),
                    texture: Some("Interface\\Icons\\Spell_Nature_SpiritArmor".into()),
                    passive: true,
                    autocast: Some((false, false)),
                    packed: 0x0100_0000 | 3025,
                    ..Default::default()
                },
            ],
        }
    }

    /// `HasPetSpells` is **the count and a token**, always two returns, and the count is a NUMBER
    /// — `SpellBook_GetCurrentPage` divides by it, so a 1/nil boolean would pin the page count.
    #[test]
    fn has_pet_spells_answers_a_count_and_a_class_token() {
        let mut s = UiScript::new().unwrap();
        s.set_pet_book(pet_book());
        assert!(s
            .eval::<bool>(r#"local n, t = HasPetSpells() return n == 3 and t == "PET""#)
            .unwrap());

        // A warlock's book carries the other token, which is the whole of what makes the tab read
        // "Demon" — FrameXML does `getglobal("PET_TYPE_"..token)`.
        let mut demon = pet_book();
        demon.token = Some("DEMON".into());
        s.set_pet_book(demon);
        assert_eq!(
            s.eval::<String>("local _, t = HasPetSpells() return t")
                .unwrap(),
            "DEMON"
        );

        // No token resolved (no ChrClasses.dbc) still answers a STRING, never nil — a nil would
        // make the reference's own concatenation error.
        let mut untokened = pet_book();
        untokened.token = None;
        s.set_pet_book(untokened);
        assert_eq!(
            s.eval::<String>("local _, t = HasPetSpells() return t")
                .unwrap(),
            "PET"
        );
    }

    /// The two books are separate lists reached by the SAME id — the reference's `isPet ? petArray
    /// : playerArray` fork. Book id 1 means Fireball in one and Growl in the other.
    #[test]
    fn one_id_reads_two_different_books() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());
        s.set_pet_book(pet_book());

        assert_eq!(
            s.eval::<String>(r#"return GetSpellName(1, BOOKTYPE_SPELL)"#)
                .unwrap(),
            "Fireball"
        );
        assert_eq!(
            s.eval::<String>(r#"return GetSpellName(1, BOOKTYPE_PET)"#)
                .unwrap(),
            "Growl"
        );
        // The book type is a case-insensitive compare against "pet" ALONE (`0x4b3f27`), so every
        // other string is the player's book — including a typo'd one.
        assert_eq!(
            s.eval::<String>(r#"return GetSpellName(1, "PeT")"#)
                .unwrap(),
            "Growl"
        );
        assert_eq!(
            s.eval::<String>(r#"return GetSpellName(1, "spel")"#)
                .unwrap(),
            "Fireball"
        );
        // Past the pet book's end: one nil, the out-of-range shape.
        assert!(s
            .eval::<bool>(r#"return GetSpellName(4, BOOKTYPE_PET) == nil"#)
            .unwrap());
    }

    /// `GetSpellAutocast` is **pet-only and always two returns**: the player book short-circuits
    /// to `(nil, nil)` before it looks anything up (`0x4b41cb`/`0x4b41d6`), and so does an
    /// out-of-range pet index.
    #[test]
    fn autocast_is_a_pet_only_pair() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());
        s.set_pet_book(pet_book());

        assert!(s
            .eval::<bool>(
                r#"local a, e = GetSpellAutocast(1, BOOKTYPE_PET) return a == 1 and e == 1"#
            )
            .unwrap());
        assert!(s
            .eval::<bool>(
                r#"local a, e = GetSpellAutocast(2, BOOKTYPE_PET) return a == 1 and e == nil"#
            )
            .unwrap());
        assert!(
            s.eval::<bool>(
                r#"local a, e = GetSpellAutocast(3, BOOKTYPE_PET) return a == nil and e == nil"#
            )
            .unwrap(),
            "a passive is not autocastable"
        );
        assert!(
            s.eval::<bool>(
                r#"local a, e = GetSpellAutocast(1, BOOKTYPE_SPELL) return a == nil and e == nil"#
            )
            .unwrap(),
            "the PLAYER book never answers a pair"
        );
        assert!(
            s.eval::<bool>(
                r#"local a, e = GetSpellAutocast(9, BOOKTYPE_PET) return a == nil and e == nil"#
            )
            .unwrap(),
            "still two returns out of range"
        );
    }

    /// `ToggleSpellAutocast` queues only what `0x4bccb0` would actually send: a pet-book slot whose
    /// word is autocast-ALLOWED. A passive, a player-book id and an out-of-range id all queue
    /// nothing — and none of them may leak into the player's cast queue.
    #[test]
    fn only_an_autocastable_pet_slot_queues_a_toggle() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());
        s.set_pet_book(pet_book());

        s.run(
            r#"ToggleSpellAutocast(1, BOOKTYPE_PET)
               ToggleSpellAutocast(3, BOOKTYPE_PET)
               ToggleSpellAutocast(9, BOOKTYPE_PET)
               ToggleSpellAutocast(1, BOOKTYPE_SPELL)"#,
        )
        .unwrap();
        assert_eq!(s.take_pet_spell_autocasts(), vec![2649]);
        assert!(s.take_pet_spell_autocasts().is_empty(), "drain empties");
        assert!(s.take_spell_casts().is_empty());
    }

    /// A pet cast is a **different queue** from a player cast, because it is a different opcode at
    /// the far end (`CMSG_PET_ACTION`, not a player cast). A passive still refuses on both.
    #[test]
    fn a_pet_cast_queues_apart_from_a_player_cast() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());
        s.set_pet_book(pet_book());

        s.run(
            r#"CastSpell(1, BOOKTYPE_PET)
               CastSpell(3, BOOKTYPE_PET)
               CastSpell(1, BOOKTYPE_SPELL)"#,
        )
        .unwrap();
        assert_eq!(s.take_pet_spell_casts(), vec![2649], "the passive refused");
        assert_eq!(s.take_spell_casts(), vec![133]);
        assert!(s.take_pet_spell_casts().is_empty(), "drain empties");
    }

    /// A pet-book pickup puts a **pet action word** on the cursor, not a spell payload — which is
    /// exactly what makes it droppable on the pet bar (`cursor::pet`'s payload). The word is the
    /// server's own, autocast bits and all.
    #[test]
    fn a_pet_book_pickup_carries_the_packed_word() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(book());
        s.set_pet_book(pet_book());

        assert!(s
            .eval::<bool>(r#"return PickupSpell(1, BOOKTYPE_PET)"#)
            .unwrap());
        let Some(CursorPayload::PetAction(p)) = s.cursor_payload() else {
            panic!(
                "expected a pet action payload, got {:?}",
                s.cursor_payload()
            );
        };
        assert_eq!(p.packed, 0xC100_0000 | 2649);
        assert_eq!(p.src_slot, 0, "it came out of the book, not off the bar");

        // The player book still produces a SPELL payload — the two are not interchangeable.
        s.run("ClearCursor()").unwrap();
        assert!(s
            .eval::<bool>(r#"return PickupSpell(1, BOOKTYPE_SPELL)"#)
            .unwrap());
        assert!(matches!(s.cursor_payload(), Some(CursorPayload::Spell(_))));
    }

    /// `GetSpellCooldown(id, "pet")` reads the PET's slot — the reference reaches bank 1 with the
    /// same `0x6e2ea0(edx = isPet)` `GetPetActionCooldown` uses, so a spell on the bar and the same
    /// spell in the book must never disagree. The elapsed-goes-cold rule is the player book's.
    #[test]
    fn the_pet_books_cooldown_is_the_pets_own() {
        let mut s = UiScript::new().unwrap();
        s.tick(10.0); // GetTime == 10
        s.set_spellbook(book());
        s.set_pet_book(pet_book());

        let (start, duration, enable) = s
            .eval::<(f64, f64, i32)>(r#"return GetSpellCooldown(1, BOOKTYPE_PET)"#)
            .unwrap();
        assert!((start - 9.4).abs() < 1e-9, "start {start}");
        assert!((duration - 5.0).abs() < 1e-9);
        assert_eq!(enable, 1);
        // The player book's slot 1 has none — proof the fork reached the right list.
        assert_eq!(
            s.eval::<(f64, f64, i32)>(r#"return GetSpellCooldown(1, BOOKTYPE_SPELL)"#)
                .unwrap(),
            (0.0, 0.0, 1)
        );

        s.tick(5.0); // now == 15 > 9.4 + 5.0
        assert_eq!(
            s.eval::<(f64, f64, i32)>(r#"return GetSpellCooldown(1, BOOKTYPE_PET)"#)
                .unwrap(),
            (0.0, 0.0, 1)
        );
    }

    /// **A rankless spell's rank is the empty string, not nil** — and the arity is always two.
    ///
    /// Both returns go through one push helper with no rank-specific branch (`0x4b4063`/`0x4b4076`,
    /// single exit `mov eax,0x2` at `0x4b407c`), and the DBC pointer is never NULL, so an on-disk
    /// NameSubtext offset of 0 becomes a pointer to the string block's byte 0 — a real zero-length
    /// Lua string. 64% of the shipped Spell.dbc's rows are in that state, `Attack` among them.
    ///
    /// The two corpus idioms this was breaking are both asserted here, because "falsey either way"
    /// is exactly the reasoning that made nil look acceptable: a nil is an ILLEGAL TABLE KEY
    /// (Roid-Macros) and an ILLEGAL `string.find` argument (CT_MasterMod), while `""` is neither.
    #[test]
    fn a_rankless_spell_answers_an_empty_rank_and_still_two_values() {
        let mut s = UiScript::new().unwrap();
        s.set_spellbook(SpellBookState {
            tabs: Vec::new(),
            slots: vec![
                SpellSlotView {
                    spell_id: 6603,
                    name: "Attack".into(),
                    rank: None,
                    ..Default::default()
                },
                SpellSlotView {
                    spell_id: 78,
                    name: "Heroic Strike".into(),
                    rank: Some("Rank 1".into()),
                    ..Default::default()
                },
            ],
        });

        assert_eq!(
            s.eval::<i64>(r#"return select('#', GetSpellName(1, "spell"))"#)
                .unwrap(),
            2
        );
        let (name, rank) = s
            .eval::<(String, String)>(r#"return GetSpellName(1, "spell")"#)
            .unwrap();
        assert_eq!((name.as_str(), rank.as_str()), ("Attack", ""));
        assert!(
            s.eval::<bool>(r#"local _, r = GetSpellName(1, "spell") return r ~= nil"#)
                .unwrap(),
            "the rankless rank must be a STRING, not nil"
        );
        // Roid-Macros/Generic.lua:35 and CT_MasterMod/CT_Master.lua:16, in miniature.
        s.run(r#"local _, r = GetSpellName(1, "spell") local t = {} t[r] = 1"#)
            .expect("a rankless rank must be a legal table key");
        s.run(r#"local _, r = GetSpellName(1, "spell") string.find(r, "(%d+)")"#)
            .expect("a rankless rank must be a legal string.find argument");
        // A ranked spell is unchanged.
        assert_eq!(
            s.eval::<(String, String)>(r#"return GetSpellName(2, "spell")"#)
                .unwrap(),
            ("Heroic Strike".to_string(), "Rank 1".to_string())
        );

        // An unfilled slot INSIDE the range is two nils, not one — distinguishable by select('#').
        assert_eq!(
            s.eval::<i64>(r#"return select('#', GetSpellName(9, "spell"))"#)
                .unwrap(),
            2
        );
        assert!(s
            .eval::<bool>(r#"local a, b = GetSpellName(9, "spell") return a == nil and b == nil"#)
            .unwrap());

        // Past the reference's [0, 0x400) gate it RAISES rather than answering nil.
        let err = s
            .run(r#"GetSpellName(1024, "spell")"#)
            .expect_err("an out-of-range slot must raise");
        assert!(
            format!("{err}").contains("Invalid spell slot in GetSpellName"),
            "got {err}"
        );
        // ...and 1023 is inside it, so it answers rather than raising.
        assert_eq!(
            s.eval::<i64>(r#"return select('#', GetSpellName(1023, "spell"))"#)
                .unwrap(),
            2
        );
    }
}
