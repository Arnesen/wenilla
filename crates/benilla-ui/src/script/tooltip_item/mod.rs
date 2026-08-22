//! The engine **item-tooltip renderer** (decision 0274 P1) — the one line law every item hover
//! renders through, mirroring the real client's single shared C++ renderer (`0x52b650`, behind
//! 8 of the 9 `Set*Item` bindings — wow-re `ui/scratch/tooltip-money.md`). The entry methods
//! (`SetItemById`, `SetBagItem`, `SetMerchantItem`, `SetBuybackItem`) register into the
//! GameTooltip kind table beside the widget verbs ([`super::tooltip`]).
//!
//! **The line law is BYTE-VERIFIED** — the 0274 §5 verdict on `0x52b650`'s emission order
//! (wow-re `ui/scratch/tooltip-content-law.md`, §5-cross-checked; folded back 2026-07-10):
//! every family's order, gate, and color pointer is the binary's, with the enUS text from the
//! extracted GlobalStrings. Not yet built (feeds pending, laws recorded): the instance
//! families (soulbound override, enchants, made-by, live durability, cooldown-remaining).
//! Residual INTERIMs cited inline: the dual-wield/off-hand proficiency exception
//! (`0x5eab70`), the type cell's override red, the set-owned count source.
//!
//! **Compare mode** (0274 P4): `SetInventoryItem` on an ARMED shopping tooltip renders the byte
//! law's compare shape — gray "Currently Equipped" first (`[arg+0x18]≠0`), the NAME white
//! instead of quality-colored, and the compact early-return at `0x52e14c` (`[arg+0x14]≠0`):
//! nothing after the charges/cooldown block. The engine fires `SHOW_COMPARE_TOOLTIP(slot, n)`
//! for an equippable item on the MAIN GameTooltip while shift is held (ref PaperDollFrame.lua's
//! listener seats `ShoppingTooltip<n>` on the paperdoll slot — so compares only show with the
//! character window open, exactly the 1.12 behavior). When the real engine fires (render vs
//! shift edge) is untraced — INTERIM: both, `arm_compare` + `on_shift_edge`.
//!
//! **The red "you can't use this" law** (the director's explicit ask) — byte-verified §1-RED:
//! red is the AddLine color `0xffff2020`, applied to requirement lines the ACTIVE player fails
//! (level, class/race lists, skill rank, required spell), to LOCKED, to broken durability, and
//! unconditionally to "Already known"; the NAME never recolors. Compared against
//! [`super::PlayerReqState`] + the spellbook.
//!
//! The sell-price money row is the byte-verified law: only a REAL-INSTANCE source (`SetBagItem`)
//! while the merchant window is open and repair mode is off — the engine computes
//! `SellPrice × stack` and fires the `OnTooltipAddMoney` script; FrameXML renders the coins
//! (`SetTooltipMoney`). A zero sell price in that context prints `ITEM_UNSELLABLE`. Template
//! sources (`SetMerchantItem`/`SetItemById`/quest rows) never show money, per the same law.

use mlua::{Lua, MultiValue, Table, Value};

use super::object::frame_handle_of;
use super::tooltip::{append_line, clear_content, fire_cleared, show_or_hide_empty};
use super::{ItemTemplateView, Model};

mod names;
mod render;

use names::{quality_color, GRAY, WHITE};
use render::render_view;

/// Look up the store; a miss records the ask (the app sends `CMSG_ITEM_QUERY` and pushes back —
/// the hover's re-enter loop repaints on arrival).
fn view_of(lua: &Lua, item_id: u32) -> Option<ItemTemplateView> {
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let v = model.item_templates.get(&item_id).cloned();
    if v.is_none() && item_id != 0 {
        model.item_stat_asks.insert(item_id);
    }
    v
}

/// The enchant lines a **random-suffix roll** contributes, out of the pushed roll table — the
/// reference's §E5 copy of the suffix row's five ids into enchant slots 2..6, which every
/// block-supplying source with no item object relies on (loot, links, auction rows, the roll
/// window). `0`, or an id the table doesn't name, contributes nothing.
fn roll_enchants(lua: &Lua, random_property_id: u32) -> Vec<crate::script::EnchantView> {
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    model
        .random_properties
        .get(&random_property_id)
        .map(|v| v.enchants.clone())
        .unwrap_or_default()
}

/// The bracketed name out of an `|Hitem:…|h[Name]|h` link (the container slot's own display
/// name — the miss-path fallback line's source).
fn link_name(link: &str) -> Option<&str> {
    let start = link.find('[')? + 1;
    let end = link[start..].find(']')? + start;
    Some(&link[start..end])
}

/// Fire the engine→Lua money hand-off (`OnTooltipAddMoney(copper)` — the byte-verified protocol:
/// the engine computes, FrameXML's `SetTooltipMoney` renders).
fn fire_add_money(lua: &Lua, h: crate::widget::FrameHandle, copper: u64) {
    let id = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        model.frame_id(h)
    };
    if let Err(e) = super::event::fire_widget_handler(
        lua,
        id,
        "OnTooltipAddMoney",
        vec![Value::Number(copper as f64)],
    ) {
        lua.app_data_mut::<Model>()
            .expect("model app_data")
            .errors
            .push(e.to_string());
    }
}

/// The `|Hitem:` payload's numeric fields — the full escaped shape
/// (`|cff…|Hitem:2947:0:584:0|h[Name]|h|r`) or a bare `item:2947`.
///
/// The four are `(item id, enchant id, randomPropertyId, uniqueId)` — the reference's own link
/// format `"%s|Hitem:%d:%d:%d:%d|h[%s]|h%s"` (`0x8549c8`), and `SetHyperlink 0x532181` parses them
/// straight into the tooltip's instance block: token 1 → enchant slot 0 (`+0x3d0`), **token 2 →
/// `+0x424`, the roll** (which §E5 then expands into slots 2..6), token 3 → `+0x420`, which
/// nothing reads. Missing fields read `0`, like the bare `item:id` shape an addon may pass.
fn hyperlink_item_fields(link: &str) -> Option<(u32, u32, u32)> {
    let at = link.find("item:")? + 5;
    let tail = &link[at..];
    let end = tail.find("|h").unwrap_or(tail.len());
    let mut fields = tail[..end]
        .split(':')
        .map(|f| f.trim().parse().unwrap_or(0));
    let item_id: u32 = fields.next()?;
    let enchant_id = fields.next().unwrap_or(0);
    let random_property_id = fields.next().unwrap_or(0);
    (item_id != 0).then_some((item_id, enchant_id, random_property_id))
}

/// The shared id-keyed render (`SetItemById`/`SetHyperlink`): template hit → the full line law
/// (+ the compare arm when this is the main GameTooltip); miss → the ask + a name-only line.
fn render_by_id(
    lua: &Lua,
    this: &Table,
    item_id: u32,
    fb_name: Option<String>,
    fb_q: Option<u32>,
) -> mlua::Result<()> {
    let h = frame_handle_of(lua, this)?;
    {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        clear_content(&mut model, h);
    }
    fire_cleared(lua, h);
    match view_of(lua, item_id) {
        Some(v) => {
            render_view(lua, this, &v, false, None)?;
            arm_compare(lua, h, &v);
        }
        None => {
            if let Some(name) = fb_name {
                append_line(
                    lua,
                    this,
                    (name, quality_color(fb_q.unwrap_or(1))),
                    None,
                    false,
                )?;
            }
        }
    }
    show_or_hide_empty(lua, h);
    Ok(())
}

/// The paperdoll slots an item of this InventoryType equips into (the 1.12
/// `GetInventorySlotInfo` slot ids) — the `SHOW_COMPARE_TOOLTIP` targets. Two-slot families
/// (rings, trinkets, one-hand weapons, and a two-hander displacing both hands) fire two
/// shopping tooltips, ref PaperDollFrame.lua's `arg2`. Twin of the app-side
/// `ui_items::find_equip_slot` (the equip-click fit rule) — one law, two consumers.
fn equip_slots_for(inventory_type: u32) -> &'static [u32] {
    match inventory_type {
        1 => &[1],                  // head
        2 => &[2],                  // neck
        3 => &[3],                  // shoulder
        4 => &[4],                  // shirt
        5 | 20 => &[5],             // chest / robe
        6 => &[6],                  // waist
        7 => &[7],                  // legs
        8 => &[8],                  // feet
        9 => &[9],                  // wrist
        10 => &[10],                // hands
        11 => &[11, 12],            // finger
        12 => &[13, 14],            // trinket
        16 => &[15],                // back
        19 => &[19],                // tabard
        13 => &[16, 17],            // one-hand
        21 => &[16],                // main hand
        14 | 22 | 23 => &[17],      // shield / off-hand weapon / held
        17 => &[16, 17],            // two-hand (displaces both hands)
        15 | 25 | 26 | 28 => &[18], // bow / thrown / wand-gun / relic
        _ => &[],
    }
}

/// After an item render on the MAIN GameTooltip: remember the compare targets and, when shift
/// is already held, fire the compare event now. (Other tooltip frames — shopping, ItemRef —
/// never arm; the shift-edge drive (`on_shift_edge`) covers pressing shift mid-hover. Whether
/// the real engine fires at render, at the edge, or both is untraced — INTERIM, both.)
fn arm_compare(lua: &Lua, h: crate::widget::FrameHandle, v: &ItemTemplateView) {
    let (is_main, shift) = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        let is_main = model.arena.lookup("GameTooltip") == Some(h);
        let shift = model.modifiers.0;
        if is_main {
            let slots = equip_slots_for(v.inventory_type);
            if let Ok(t) = super::tooltip::tip_mut(&mut model, h) {
                t.compare_slots = slots.to_vec();
            }
        }
        (is_main, shift)
    };
    if is_main && shift {
        fire_compare(lua, h);
    }
}

/// Fire `SHOW_COMPARE_TOOLTIP(slot, n)` for the main tooltip's remembered targets, ARMING
/// `ShoppingTooltip<n>` for the compare render first (the listener's `SetOwner` +
/// `SetInventoryItem` run inside the fire — ref PaperDollFrame.lua:621-640; the arm survives
/// the SetOwner content clear, kinds.rs has the seam note).
fn fire_compare(lua: &Lua, h: crate::widget::FrameHandle) {
    let slots: Vec<u32> = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        match super::tooltip::tip_mut(&mut model, h) {
            Ok(t) => t.compare_slots.clone(),
            Err(_) => return,
        }
    };
    for (i, slot) in slots.iter().take(2).enumerate() {
        let n = (i + 1) as i64;
        {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(sh) = model.arena.lookup(&format!("ShoppingTooltip{n}")) {
                if let Ok(t) = super::tooltip::tip_mut(&mut model, sh) {
                    t.compare_armed = true;
                }
            }
        }
        super::event::fire_global(
            lua,
            "SHOW_COMPARE_TOOLTIP",
            &[
                super::ScriptValue::Int(i64::from(*slot)),
                super::ScriptValue::Int(n),
            ],
        );
    }
}

/// The shift-edge compare drive (called from `UiScript::set_modifiers` on a shift transition):
/// pressing shift over a live equippable item-hover fires the compares; releasing hides the
/// shopping tooltips (INTERIM lifecycle — the real engine's edge handling is untraced).
pub(super) fn on_shift_edge(lua: &Lua, down: bool) {
    let (main, shown, has_targets) = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        let Some(h) = model.arena.lookup("GameTooltip") else {
            return;
        };
        let shown = model.arena.frame(h).map(|f| f.shown).unwrap_or(false);
        let has = match super::tooltip::tip_mut(&mut model, h) {
            Ok(t) => !t.compare_slots.is_empty(),
            Err(_) => false,
        };
        (h, shown, has)
    };
    if down {
        if shown && has_targets {
            fire_compare(lua, main);
        }
    } else {
        for n in 1..=2 {
            let sh = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                model.arena.lookup(&format!("ShoppingTooltip{n}"))
            };
            if let Some(sh) = sh {
                super::tooltip::hide_tooltip(lua, sh);
            }
        }
    }
}

/// Register the item content channels into the GameTooltip kind method table.
pub(super) fn install_methods(lua: &Lua, m: &Table) -> mlua::Result<()> {
    // GameTooltip:SetItemById(itemId [, fallbackName, fallbackQuality]) — the id-keyed hover
    // (quest reward rows, loot rows; a benilla extension beside the era surface). Template
    // source: no money row, per the byte-verified sell-price law.
    m.set(
        "SetItemById",
        lua.create_function(
            |lua, (this, item_id, fb_name, fb_q): (Table, u32, Option<String>, Option<u32>)| {
                render_by_id(lua, &this, item_id, fb_name, fb_q)
            },
        )?,
    )?;

    // GameTooltip:SetHyperlink(link) — the chat-link tooltip (ref ItemRef.lua's SetItemRef →
    // ItemRefTooltip:SetHyperlink). Accepts the full escaped link or a bare "item:<id>";
    // non-item links (player/spell/quest) have no tooltip surface yet and no-op.
    m.set(
        "SetHyperlink",
        lua.create_function(|lua, (this, link): (Table, String)| {
            let Some((id, _enchant_id, roll)) = hyperlink_item_fields(&link) else {
                return Ok(());
            };
            let h = frame_handle_of(lua, &this)?;
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            // A link is a **block source** (`SetHyperlink 0x532181` passes p6=1), so it never
            // reaches the `<Random enchantment>` arm and it does show the roll's own lines — the
            // half of §E5 decision 0920's prose had backwards. The NAME comes off the link's own
            // brackets: the sender's client built that text with the suffix already joined
            // (`0x5d8b00` feeds the link builder), so re-deriving it would only invite the two to
            // disagree.
            //
            // The link's ENCHANT field (slot 0, a permanent enchant like "Crusader") is parsed but
            // not yet rendered: naming it needs the `SpellItemEnchantment` row, and no benilla
            // surface writes that field into a link it composes today. A stated gap, not drift.
            let inst = render::ItemInstance {
                name: link_name(&link).map(str::to_string),
                enchants: roll_enchants(lua, roll),
                ..Default::default()
            };
            match view_of(lua, id) {
                Some(v) => {
                    render_view(lua, &this, &v, false, Some(&inst))?;
                    // Unchanged from the `render_by_id` this leg used to share: a link dropped on
                    // the MAIN tooltip still arms the shopping compare (`arm_compare`'s own gate).
                    arm_compare(lua, h, &v);
                }
                None => {
                    if let Some(name) = link_name(&link) {
                        append_line(
                            lua,
                            &this,
                            (name.to_string(), quality_color(1)),
                            None,
                            false,
                        )?;
                    }
                }
            }
            show_or_hide_empty(lua, h);
            Ok(())
        })?,
    )?;

    // GameTooltip:SetInventoryItem(unit, slot) → hasItem, hasCooldown — the equipped-slot hover
    // (paperdoll slots, buff-frame weapon enchants) and the shopping-compare listener's render
    // (ref PaperDollFrame.lua:626). **Unit-keyed** through `Model::inv_slot`, the same router the
    // `GetInventoryItem*` getters use: `"player"` from the self feed, the inspected token from the
    // PUBLIC visible-item view (decision 0631 — the ref's inspect slot OnEnter calls exactly this,
    // `InspectPaperDollFrame.xml:20`). An inspected item carries no durability/creator, so those
    // lines simply don't render — the reference's own inspect tooltip shape. On an ARMED shopping
    // tooltip this renders the byte law's compare shape; the arm is consumed either way.
    // hasCooldown is nil INTERIM (no equipped-cooldown feed yet — its truthiness gates the ref's
    // re-poll only).
    m.set(
        "SetInventoryItem",
        lua.create_function(|lua, (this, unit, slot): (Table, String, usize)| {
            let h = frame_handle_of(lua, &this)?;
            let (item_id, name, quality, inst, compare) = {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                let armed = match super::tooltip::tip_mut(&mut model, h) {
                    Ok(t) => std::mem::take(&mut t.compare_armed),
                    Err(_) => false,
                };
                let view = model
                    .inv_slot(&unit, slot)
                    .filter(|s| s.item_id != 0)
                    .map(|s| {
                        (
                            s.item_id,
                            s.name.clone(),
                            s.quality,
                            render::ItemInstance {
                                // The slot's own name — app-composed, so it carries the
                                // random-suffix roll off `ITEM_FIELD_RANDOM_PROPERTIES_ID`.
                                name: s.name.clone(),
                                durability: s.durability,
                                creator: s.creator.clone(),
                                has_text: false,
                                flags: s.flags,
                                enchants: s.enchants.clone(),
                                // `SetInventoryItem 0x532ee0` also has p6=0 legs (`0x533106`,
                                // `0x5332ad`) — the "this binding can never emit OPENABLE" claim
                                // is dead here too (wow-re `right-click-open.md` §1.2). Which leg
                                // each takes is not pinned, and the case is unobservable anyway:
                                // nothing openable is equippable, so the doll hover has no clam to
                                // show. Left `false` deliberately — inventing a selector we have
                                // not read would be the §4 trade, and there is nothing to gain.
                                openable_source: false,
                            },
                        )
                    });
                match view {
                    Some((id, name, q, inst)) => (id, name, q, inst, armed),
                    None => return Ok(MultiValue::from_vec(vec![Value::Nil])),
                }
            };
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            match view_of(lua, item_id) {
                Some(v) => render_view(lua, &this, &v, compare, Some(&inst))?,
                None => {
                    // Template in flight — the slot view's own name holds the plate (the same
                    // 0138 posture as SetBagItem's miss path).
                    if compare {
                        append_line(lua, &this, ("Currently Equipped".into(), GRAY), None, false)?;
                    }
                    if let Some(name) = name {
                        let color = if compare {
                            WHITE
                        } else {
                            quality_color(quality.max(0) as u32)
                        };
                        append_line(lua, &this, (name, color), None, false)?;
                    }
                }
            }
            show_or_hide_empty(lua, h);
            Ok(MultiValue::from_vec(vec![Value::Integer(1), Value::Nil]))
        })?,
    )?;

    // GameTooltip:SetBagItem(bag, slot) → hasCooldown, repairCost — the real-instance hover.
    // The one money-eligible source built so far: merchant open + repair off ⇒ the engine fires
    // OnTooltipAddMoney(SellPrice × stack), or prints ITEM_UNSELLABLE at price 0 (wow-re
    // tooltip-money.md's gate, engine-side at last). repairCost is 0 INTERIM (the per-item
    // durability/repair feed is the paper-doll arc's).
    m.set(
        "SetBagItem",
        lua.create_function(|lua, (this, bag, slot): (Table, i64, u32)| {
            let h = frame_handle_of(lua, &this)?;
            let (item_id, count, has_cd, link, quality, inst) = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                match model
                    .containers
                    .get(&bag)
                    .and_then(|c| c.slots.get(&slot))
                    .filter(|s| s.item_id != 0)
                {
                    // The reference's own leg selector, byte-read: `SetBagItem 0x534620` asks the
                    // item-cooldown query `0x6e2ed0` and takes the p6=1 (instance-block) leg iff
                    // **all three** of enable/start/duration are non-zero — a genuinely running
                    // cooldown. That one boolean is both the Lua `hasCooldown` return and the
                    // openable gate's inverse, so they are computed once, here (decision 0896).
                    Some(s) => {
                        let has_cd = s
                            .cooldown
                            .is_some_and(|(start, dur, en)| en && start > 0 && dur > 0);
                        (
                            s.item_id,
                            s.count.max(1),
                            has_cd,
                            s.link.clone(),
                            s.quality.unwrap_or(1),
                            render::ItemInstance {
                                // The bag slot's display name rides its LINK (the reference
                                // builds that link out of `0x5d8b00`'s output, so the two are the
                                // same string by construction — the roll's suffix included).
                                name: s.link.as_deref().and_then(link_name).map(str::to_string),
                                durability: s.durability,
                                creator: s.creator.clone(),
                                has_text: s.readable,
                                flags: s.flags,
                                enchants: s.enchants.clone(),
                                // p6 == 0 ⇔ no running cooldown. A clam shows the green line; the
                                // same clam mid-cooldown would show ITEM_COOLDOWN_TIME instead
                                // (that line has no feed here yet — a separate, pre-existing gap).
                                openable_source: !has_cd,
                            },
                        )
                    }
                    None => return Ok(MultiValue::from_vec(vec![Value::Nil])),
                }
            };
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            match view_of(lua, item_id) {
                Some(v) => {
                    render_view(lua, &this, &v, false, Some(&inst))?;
                    let (merchant_open, repairing) = {
                        let model = lua.app_data_mut::<Model>().expect("model app_data");
                        (model.merchant.is_some(), model.repair_mode)
                    };
                    if merchant_open && !repairing {
                        if v.sell_price > 0 {
                            fire_add_money(lua, h, u64::from(v.sell_price) * u64::from(count));
                        } else {
                            append_line(lua, &this, ("No sell price".into(), WHITE), None, false)?;
                        }
                    }
                    arm_compare(lua, h, &v);
                }
                None => {
                    // Template in flight. The real client early-outs to an EMPTY tooltip here
                    // (tooltip-money.md's uncached path); benilla keeps a name-only line instead
                    // (decision 0138 — the name is already on the slot's link, and a blank plate
                    // under an on-screen name reads broken). The re-enter loop repaints the
                    // moment the push lands.
                    if let Some(name) = link.as_deref().and_then(link_name) {
                        append_line(
                            lua,
                            &this,
                            (name.to_string(), quality_color(quality)),
                            None,
                            false,
                        )?;
                    }
                }
            }
            show_or_hide_empty(lua, h);
            Ok(MultiValue::from_vec(vec![
                Value::Boolean(has_cd),
                Value::Integer(0),
            ]))
        })?,
    )?;

    // GameTooltip:SetLootItem(slot) — the loot-row hover (the reference's own binding,
    // `0x533470`, which its `LootButtonTemplate` <OnEnter> runs behind `LootSlotIsItem`).
    // **A block source, not a template one** (§1-SESSION's writer table: the leg at `0x533564`
    // supplies the instance block, p6=1), and that is the whole point of it existing here: a loot
    // slot is no item object, so its rolled random-suffix enchants reach the builder through the
    // block — never through `ITEM_FIELD_ENCHANTMENT`, which the wire does not carry for loot.
    // Hovering a "… of the Monkey" drop through the template path `SetItemById` instead printed
    // the `<Random enchantment>` placeholder until the item was in the bag (decision 1547).
    //
    // `slot` is the 1-based display row, like every other loot getter; the coin pile and a
    // cleared row have no tooltip.
    m.set(
        "SetLootItem",
        lua.create_function(|lua, (this, slot): (Table, usize)| {
            let h = frame_handle_of(lua, &this)?;
            let row = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .loot
                    .as_ref()
                    .and_then(|l| slot.checked_sub(1).and_then(|n| l.rows.get(n)))
                    .and_then(|r| r.clone())
                    .filter(|r| !r.is_coin && r.item_id != 0)
            };
            let Some(row) = row else {
                return Ok(());
            };
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            // The row's own name is the app-composed one — the roll's suffix already joined, so
            // the plate reads "Chipped Claw of the Bear" like the reference's `0x5d8b00` output.
            // The lines come from the roll id through the pushed table, which is where the
            // reference reads them too (its `+0x424` against the DBC store, §E5).
            let inst = render::ItemInstance {
                name: row.name.clone(),
                enchants: roll_enchants(lua, row.random_property_id),
                ..Default::default()
            };
            match view_of(lua, row.item_id) {
                Some(v) => render_view(lua, &this, &v, false, Some(&inst))?,
                // Template in flight — the row's name holds the plate (SetBagItem's 0138 posture;
                // the re-enter loop repaints when the push lands).
                None => {
                    if let Some(name) = row.name.clone() {
                        append_line(
                            lua,
                            &this,
                            (name, quality_color(row.quality.unwrap_or(1))),
                            None,
                            false,
                        )?;
                    }
                }
            }
            show_or_hide_empty(lua, h);
            Ok(())
        })?,
    )?;

    // GameTooltip:SetLootRollItem(rollId) — the group-loot roll window's hover (the reference's
    // own `0x5364a0`). The same shape as SetLootItem beside it, byte-verified on the same
    // dispatch: p6=1 with `+0x424 ← [roll+0x20]` (the roll's randomPropertyId), all 7 enchant
    // slots zero, and BOTH guid args `&{0,0}` — no item object, so §E5's suffix-row copy is the
    // only enchant source and the placeholder arm is unreachable.
    m.set(
        "SetLootRollItem",
        lua.create_function(|lua, (this, roll_id): (Table, u32)| {
            let h = frame_handle_of(lua, &this)?;
            let entry = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .loot_rolls
                    .rolls
                    .iter()
                    .find(|r| r.roll_id == roll_id)
                    .cloned()
                    .filter(|r| r.item_id != 0)
            };
            let Some(entry) = entry else {
                return Ok(());
            };
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            let inst = render::ItemInstance {
                name: entry.name.clone(),
                enchants: roll_enchants(lua, entry.random_property_id),
                ..Default::default()
            };
            match view_of(lua, entry.item_id) {
                Some(v) => render_view(lua, &this, &v, false, Some(&inst))?,
                None => {
                    if let Some(name) = entry.name.clone() {
                        append_line(
                            lua,
                            &this,
                            (name, quality_color(entry.quality.unwrap_or(1))),
                            None,
                            false,
                        )?;
                    }
                }
            }
            show_or_hide_empty(lua, h);
            Ok(())
        })?,
    )?;

    // GameTooltip:SetMerchantItem(index) / SetBuybackItem(index) — template-id sources through
    // the same store (the real SetMerchantItem is the template path — tooltip-money.md); the
    // merchant feed's own stat head is the fallback while the template answer is in flight, so
    // the vendor hover always shows at least the head it showed pre-0274.
    for (method, buyback) in [("SetMerchantItem", false), ("SetBuybackItem", true)] {
        m.set(
            method,
            lua.create_function(move |lua, (this, index): (Table, usize)| {
                let h = frame_handle_of(lua, &this)?;
                let row = {
                    let model = lua.app_data_mut::<Model>().expect("model app_data");
                    let Some(merchant) = &model.merchant else {
                        return Ok(());
                    };
                    let list = if buyback {
                        &merchant.buyback
                    } else {
                        &merchant.items
                    };
                    list.get(index.saturating_sub(1)).cloned()
                };
                let Some(row) = row else { return Ok(()) };
                {
                    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                    clear_content(&mut model, h);
                }
                fire_cleared(lua, h);
                match view_of(lua, row.item_id) {
                    Some(v) => {
                        render_view(lua, &this, &v, false, None)?;
                        arm_compare(lua, h, &v);
                    }
                    None => {
                        // Template in flight: the row's own stat head as a minimal view.
                        let head = row.stats.unwrap_or_default();
                        let v = ItemTemplateView {
                            name: row.name.clone().unwrap_or_default(),
                            quality: head.quality,
                            class: head.class,
                            subclass: head.subclass,
                            inventory_type: head.inventory_type,
                            damages: if head.dmg_max > 0.0 {
                                vec![(head.dmg_min, head.dmg_max, head.dmg_type)]
                            } else {
                                Vec::new()
                            },
                            delay_ms: head.delay_ms,
                            armor: head.armor,
                            block: head.block,
                            ..Default::default()
                        };
                        render_view(lua, &this, &v, false, None)?;
                    }
                }
                show_or_hide_empty(lua, h);
                Ok(())
            })?,
        )?;
    }

    // GameTooltip:SetInboxItem(index) — the mail window's enclosed-item hover (decision 0544): the
    // inbox row's item entry through the same id-keyed store (MailFrame.lua l.218/470). A row with no
    // enclosed item (item_id 0) is a no-op, like the reference (which only calls this when hasItem).
    m.set(
        "SetInboxItem",
        lua.create_function(|lua, (this, index): (Table, usize)| {
            let h = frame_handle_of(lua, &this)?;
            let (item_id, roll, name) = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                let Some(mail) = &model.mail else {
                    return Ok(());
                };
                match mail.inbox.get(index.saturating_sub(1)) {
                    Some(r) => (r.item_id, r.item_random_property_id, r.item_name.clone()),
                    None => (0, 0, None),
                }
            };
            if item_id == 0 {
                return Ok(());
            }
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            // A block source (`SetInboxItem 0x5355fa`, p6=1) carrying the attachment's roll — so
            // an "… of the Monkey" in the mail reads like the same drop in the loot window, and
            // the placeholder arm is unreachable here too (decision 1547).
            let inst = render::ItemInstance {
                name,
                enchants: roll_enchants(lua, roll),
                ..Default::default()
            };
            if let Some(v) = view_of(lua, item_id) {
                render_view(lua, &this, &v, false, Some(&inst))?;
                arm_compare(lua, h, &v);
            }
            show_or_hide_empty(lua, h);
            Ok(())
        })?,
    )?;

    // GameTooltip:SetAuctionItem(type, index) — an auction ROW's hover (decision 1511). The row's
    // item entry through the same id-keyed store, keyed by list type ("list"/"bidder"/"owner").
    //
    // An auction row's tooltip deliberately emits **no** "Made by", "Gift from" or openable lines,
    // and the reason is structural rather than a rule anyone applies: the reference's binding
    // zeroes the GUID arguments before it resolves anything, so the gate those lines hang off
    // fails outright (wow-re §5, claim 6). Our id-keyed store has no instance to report either,
    // so we land on the same output by the same shape of reasoning.
    m.set(
        "SetAuctionItem",
        lua.create_function(|lua, (this, kind, index): (Table, String, usize)| {
            let h = frame_handle_of(lua, &this)?;
            let (item_id, roll, name) = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                let Some(auction) = &model.auction else {
                    return Ok(());
                };
                let Some(list) = super::auction::list_index_of(&kind) else {
                    return Ok(());
                };
                match auction.lists[list].rows.get(index.saturating_sub(1)) {
                    Some(r) => (r.item_id, r.random_property_id, r.name.clone()),
                    None => (0, 0, None),
                }
            };
            if item_id == 0 {
                return Ok(());
            }
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            // A block source (`SetAuctionItem 0x5359d9`, p6=1) carrying the listing's roll: a
            // rolled auction shows its real lines, never the placeholder (decision 1547). The
            // zeroed GUIDs the note above describes are unaffected — a roll is not an instance.
            let inst = render::ItemInstance {
                name,
                enchants: roll_enchants(lua, roll),
                ..Default::default()
            };
            if let Some(v) = view_of(lua, item_id) {
                render_view(lua, &this, &v, false, Some(&inst))?;
                arm_compare(lua, h, &v);
            }
            show_or_hide_empty(lua, h);
            Ok(())
        })?,
    )?;

    // GameTooltip:SetAuctionSellItem() — the create-auction slot's hover (decision 1511). The item
    // staged in the sell slot, through the same id-keyed store. A no-op when the slot is empty
    // (the reference gates the call on GetAuctionSellItemInfo()).
    //
    // Unlike an auction ROW, this one names a real item the player owns — the reference passes the
    // live sell-slot GUID here and resolves an actual object, so its creator line is structurally
    // reachable (wow-re §5, claim 6, REFINED). Ours is still the id-keyed template view, so we do
    // not print one; that is a known narrowing, not a claim about the reference.
    m.set(
        "SetAuctionSellItem",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let item_id = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .auction_sell_item
                    .as_ref()
                    .map(|it| it.item_id)
                    .unwrap_or(0)
            };
            if item_id == 0 {
                return Ok(());
            }
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            if let Some(v) = view_of(lua, item_id) {
                render_view(lua, &this, &v, false, None)?;
                arm_compare(lua, h, &v);
            }
            show_or_hide_empty(lua, h);
            Ok(())
        })?,
    )?;

    // GameTooltip:SetSendMailItem() — the mail Send tab's attached-item hover (decision 0544): the
    // cursor item attached to the send slot, through the same id-keyed store (MailFrame.lua l.952).
    // A no-op when nothing is attached (the reference gates the call on GetSendMailItem()).
    m.set(
        "SetSendMailItem",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let item_id = {
                let model = lua.app_data_mut::<Model>().expect("model app_data");
                model
                    .mail_send_item
                    .as_ref()
                    .map(|it| it.item_id)
                    .unwrap_or(0)
            };
            if item_id == 0 {
                return Ok(());
            }
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            if let Some(v) = view_of(lua, item_id) {
                render_view(lua, &this, &v, false, None)?;
                arm_compare(lua, h, &v);
            }
            show_or_hide_empty(lua, h);
            Ok(())
        })?,
    )?;

    // GameTooltip:SetTradeSkillItem(skillIndex [, reagentIndex]) — the tradeskill window's item
    // hover (decision 0437 phase 2): with reagentIndex, that recipe's reagent's item; without, the
    // recipe's PRODUCT item. Routes through the same id-keyed renderer SetItemById uses. A product
    // id of 0 (a pure-effect recipe) or an in-flight ask-once answer both fall to render_by_id's own
    // name-only fallback line (the recipe's name for the product channel, the reagent's own —
    // possibly still-nil — name for the reagent channel) rather than a no-op: that fallback already
    // exists and reads better than a blank hover. An out-of-range skill/reagent index is a no-op.
    m.set(
        "SetTradeSkillItem",
        lua.create_function(
            |lua, (this, skill_index, reagent_index): (Table, usize, Option<usize>)| {
                let found = {
                    let model = lua.app_data_ref::<Model>().expect("model app_data");
                    // The VISIBLE-row mapping (headers interleave since the TU-B grouping
                    // landed) — never a raw recipes[] index; a header index resolves to None.
                    let Some(recipe) = super::tradeskill::recipe_at(&model, skill_index) else {
                        return Ok(());
                    };
                    match reagent_index {
                        Some(ri) => ri
                            .checked_sub(1)
                            .and_then(|i| recipe.reagents.get(i))
                            .map(|r| (r.item, r.name.clone())),
                        None => Some((recipe.product_item, Some(recipe.name.clone()))),
                    }
                };
                let Some((item_id, fb_name)) = found else {
                    return Ok(());
                };
                render_by_id(lua, &this, item_id, fb_name, None)
            },
        )?,
    )?;

    // GameTooltip:SetCraftItem(craftIndex, reagentIndex) — the craft window's reagent hover
    // (decision 0437 phase 3, ref `CraftItemTemplate`'s OnEnter, `Blizzard_CraftUI.xml:33`):
    // ALWAYS the REAGENT's item — unlike SetTradeSkillItem, Craft has no product-item concept to
    // fall back to when reagentIndex is omitted (craft.rs's own module doc, "no product-item
    // concept"), so this signature has no optional arm; the ref itself never calls it with one
    // argument either. Routes through the same id-keyed renderer. An out-of-range craft/reagent
    // index is a no-op.
    m.set(
        "SetCraftItem",
        lua.create_function(
            |lua, (this, craft_index, reagent_index): (Table, usize, usize)| {
                let found = {
                    let model = lua.app_data_ref::<Model>().expect("model app_data");
                    let Some(c) = &model.craft else {
                        return Ok(());
                    };
                    let Some(recipe) = craft_index.checked_sub(1).and_then(|i| c.recipes.get(i))
                    else {
                        return Ok(());
                    };
                    reagent_index
                        .checked_sub(1)
                        .and_then(|i| recipe.reagents.get(i))
                        .map(|r| (r.item, r.name.clone()))
                };
                let Some((item_id, fb_name)) = found else {
                    return Ok(());
                };
                render_by_id(lua, &this, item_id, fb_name, None)
            },
        )?,
    )?;

    Ok(())
}
