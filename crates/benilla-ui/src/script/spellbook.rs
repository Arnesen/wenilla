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
//! ## Pet book (named deferral)
//!
//! `BOOKTYPE_PET` is anticipated (decision 0216 §8: "pet book deferred, no pets streamed yet")
//! but answers empty everywhere: `GetNumSpellTabs`/`GetSpellTabInfo` don't even take a `bookType`
//! (matching the ref's own signature — they always read whatever `SpellBookFrame.bookType`
//! selected, and this engine only ever HOLDS the spell book, so there is no separate pet-book
//! state to switch between), and every `bookType`-taking binding treats `"pet"` as an instant
//! empty/no-op via [`slot_index`]'s own gate.
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

/// The book-id → 0-based [`SpellBookState::slots`] index seam (module docs): `None` for a
/// `bookType` other than `"spell"` (the pet deferral) or an id of `0` (the ref's ids start at 1,
/// so `id - 1` would otherwise underflow).
fn slot_index(id: u32, book_type: &str) -> Option<usize> {
    if book_type != BOOKTYPE_SPELL {
        return None;
    }
    usize::try_from(id.checked_sub(1)?).ok()
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
    let Some(slot) = slot_index(id, book_type).and_then(|i| model.spellbook.slots.get(i)) else {
        return false;
    };
    let payload = CursorSpell {
        book_slot: id,
        book_type: book_type.to_string(),
        spell_id: slot.spell_id,
        texture: slot.texture.clone(),
        passive: slot.passive,
    };
    model.cursor = Some(CursorPayload::Spell(payload));
    queue_cursor_update(model);
    true
}

/// Register the spellbook globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    g.set("BOOKTYPE_SPELL", BOOKTYPE_SPELL)?;
    g.set("BOOKTYPE_PET", BOOKTYPE_PET)?;

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

    // GetSpellName(id, bookType) -> name, rank (the Era tuple, `subSpellName` in the ref); pet or
    // out-of-range -> a single nil.
    g.set(
        "GetSpellName",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(slot) = slot_index(id, &book_type).and_then(|i| model.spellbook.slots.get(i))
            else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let rank = match &slot.rank {
                Some(r) => Value::String(lua.create_string(r)?),
                None => Value::Nil,
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&slot.name)?),
                rank,
            ]))
        })?,
    )?;

    g.set(
        "GetSpellTexture",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let tex = slot_index(id, &book_type)
                .and_then(|i| model.spellbook.slots.get(i))
                .and_then(|s| s.texture.clone());
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
            let current = slot_index(id, &book_type)
                .and_then(|i| model.spellbook.slots.get(i))
                .is_some_and(|s| s.current);
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
            let cooldown = slot_index(id, &book_type)
                .and_then(|i| model.spellbook.slots.get(i))
                .and_then(|s| s.cooldown);
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
            Ok(slot_index(id, &book_type)
                .and_then(|i| model.spellbook.slots.get(i))
                .is_some_and(|s| s.passive))
        })?,
    )?;

    // CastSpell(id, bookType) — the plain click (ref SpellButton_OnClick's `else` branch): queues
    // the resolved spell id UNLESS the slot is passive (module doc: a passive is permanent player
    // state, never something the player casts) or bookType/id resolve to nothing.
    g.set(
        "CastSpell",
        lua.create_function(|lua, (id, book_type): (u32, String)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(slot) =
                slot_index(id, &book_type).and_then(|i| model.spellbook.slots.get(i))
            {
                if !slot.passive {
                    let spell_id = slot.spell_id;
                    model.spell_casts.push(spell_id);
                }
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
    use super::{SpellBookState, SpellSlotView, SpellTabView};
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
                },
                SpellSlotView {
                    spell_id: 2136,
                    name: "Fire Blast".into(),
                    rank: Some("Rank 1".into()),
                    texture: Some("Interface\\Icons\\Spell_Fire_FireBolt02".into()),
                    passive: true, // artificial: exercises the refusal gate
                    current: false,
                    cooldown: None,
                },
                SpellSlotView {
                    spell_id: 168,
                    name: "Frost Armor".into(),
                    rank: Some("Rank 1".into()),
                    texture: Some("Interface\\Icons\\Spell_Frost_FrostArmor02".into()),
                    passive: false,
                    current: false,
                    cooldown: None,
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

    #[test]
    fn pet_book_answers_empty_everywhere() {
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

        s.run(r#"CastSpell(1, BOOKTYPE_PET)"#).unwrap();
        assert!(s.take_spell_casts().is_empty(), "pet cast is a no-op");

        assert!(!s
            .eval::<bool>(r#"return PickupSpell(1, BOOKTYPE_PET)"#)
            .unwrap());
        assert!(s.cursor_payload().is_none(), "pet pickup is a no-op");
    }
}
