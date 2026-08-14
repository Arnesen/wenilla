//! The app-side **bank feed** (decision 0604 phase 2) — the inward half of the bank seam around
//! [`benilla_ui::script`]'s `bank` module, the twin of [`crate::ui_merchant`]'s merchant seam.
//!
//! The net bridge fills [`BankOpen`] from the wire (`SMSG_SHOW_BANK` → the banker guid — sent for
//! our own `CMSG_BANKER_ACTIVATE` *and* volunteered for the gossip menu's bank option, so the arm
//! never assumes we asked). The vault's contents never pass through here: the 24 bank slots and
//! the 6 bank bags are player-array descriptor fields streamed at login (decision 0604 — the
//! window only reveals them), fed by [`crate::ui_items`] as containers `-1`/`5..=10` beside the
//! backpack. Each frame [`feed_bank`] pushes the purchase-row snapshot
//! ([`benilla_ui::script::BankState`]: the `PLAYER_BYTES_2` byte-2 purchased count + the next
//! slot's `BankBagSlotPrices.dbc` cost), fires `BANKFRAME_OPENED` on open / `BANKFRAME_CLOSED` on
//! clear / `PLAYERBANKBAGSLOTS_CHANGED` when the purchased count moves (a successful buy has **no
//! packet** — that descriptor delta is the confirmation), and resolves the banker's name for the
//! title (the merchant feed's ask-once pattern). [`drain_bank`] pulls the Lua intents back out:
//! `PurchaseSlot()` → [`ClientCommand::BuyBankSlot`], `CloseBankFrame()` → a local clear (no
//! close opcode exists — decision 0604). The standardized NPC-session range guard
//! ([`crate::ui_session`]) applies the same client-side close out of service range.
//!
//! The right-click auto-move (deposit/withdraw while the bank is open) lives one module over in
//! [`crate::ui_items`]'s drain, the sell affordance's exact pattern.

use benilla_protocol::messages::bank_slot_result;
use bevy::prelude::*;

use benilla_ui::script::{BankState, ScriptValue, UiScript};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::names::NameCache;
use crate::net::{ClientCommand, NetCommands, ObjectStore, SelfPlayer};
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, npc_switched, NpcSession};

/// The client-side `BankBagSlotPrices.dbc` table (decision 0604: the purchase ladder is client
/// data — 10s/1g/10g/25g/50g/100g, then the 999999999 sentinel). Optional resource — absent, the
/// purchase row shows cost 0 and the popup still works (the server re-prices authoritatively).
#[derive(Resource)]
pub(crate) struct BankPrices(pub(crate) benilla_formats::BankBagSlotPrices);

/// The open bank session, filled by the net bridge (`SMSG_SHOW_BANK`) and read by [`feed_bank`].
/// Holds only the banker guid — the vault's contents are descriptor fields the container feed
/// already carries. Cleared on a client-side close and on disconnect.
#[derive(Resource, Default)]
pub(crate) struct BankOpen {
    /// The banker whose window is open; `None` = no bank open.
    pub(crate) banker: Option<u64>,
}

impl BankOpen {
    /// Open (or re-point) the window at a banker.
    pub(crate) fn open(&mut self, banker: u64) {
        self.banker = Some(banker);
    }

    /// Whether the bank window is currently open.
    pub(crate) fn is_open(&self) -> bool {
        self.banker.is_some()
    }

    /// Close the open window (a client-side close — no packet exists, decision 0604).
    pub(crate) fn clear(&mut self) {
        self.banker = None;
    }

    /// Disconnect: drop the open window (mirrors the merchant/gossip session clears).
    pub(crate) fn clear_session(&mut self) {
        self.clear();
    }
}

/// The bank window is an NPC session: the standardized range guard ([`crate::ui_session`])
/// client-side-closes it — the exact `CloseBankFrame` clear — when the player walks out of the
/// banker's service range or the banker despawns.
impl NpcSession for BankOpen {
    fn npc(&self) -> Option<u64> {
        self.banker
    }

    fn close(&mut self) {
        self.clear();
    }
}

/// Bank-slot purchase refusals (`SMSG_BUY_BANK_SLOT_RESULT` — vmangos sends it only on failure)
/// queued by the net bridge for the red error line.
#[derive(Resource, Default)]
pub(crate) struct BankErrors(pub Vec<u32>);

pub(crate) struct UiBankPlugin;

impl Plugin for UiBankPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BankOpen>()
            .init_resource::<BankErrors>()
            .add_systems(
                Update,
                (
                    // Range-close before the feed so the clear turns into BANKFRAME_CLOSED the
                    // same frame; push before the input pass; drain after it (the merchant shape).
                    close_npc_session_out_of_range::<BankOpen>.before(feed_bank),
                    feed_bank.before(UiInput),
                    drain_bank.after(UiInput),
                ),
            );
    }
}

/// The red error line for a `SMSG_BUY_BANK_SLOT_RESULT` code — the reference GlobalStrings'
/// `ERR_BANKSLOT_*` texts verbatim (decision 0604: the codes map 1:1 onto them). `OK` never
/// prints (vmangos doesn't send it; tolerated silently if a server ever does).
fn bank_slot_error_text(result: u32) -> Option<String> {
    match result {
        bank_slot_result::FAILED_TOO_MANY => Some("You've reached your limit of bag slots!".into()),
        bank_slot_result::INSUFFICIENT_FUNDS => Some("You can't afford that.".into()),
        bank_slot_result::NOTBANKER => Some("That unit is not a banker!".into()),
        bank_slot_result::OK => None,
        other => Some(format!("Bank slot purchase failed ({other}).")),
    }
}

/// Push the purchase-row snapshot + the open/close/purchased-count events (module doc). The
/// banker's name rides the `BANKFRAME_OPENED` event arg (the merchant feed's title pattern —
/// ask-once through the [`NameCache`], `BANKFRAME_UPDATE` is not a real event, so a late name
/// lands via a fresh `BANKFRAME_OPENED` only on re-open; the title shows "Banker" until then).
#[allow(clippy::too_many_arguments)]
fn feed_bank(
    script: Option<NonSendMut<UiScript>>,
    open: Res<BankOpen>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    prices: Option<Res<BankPrices>>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    commands: Res<NetCommands>,
    mut names: ResMut<NameCache>,
    mut errors: ResMut<BankErrors>,
    mut last: Local<crate::ui_script::VmMemo<Option<BankState>>>,
    mut last_banker: Local<crate::ui_script::VmMemo<Option<u64>>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let last = last.get(&script);
    let last_banker = last_banker.get(&script);
    // Purchase refusals surface as the client's red error line.
    for result in errors.0.drain(..) {
        if let Some(text) = bank_slot_error_text(result) {
            script.fire_event("UI_ERROR_MESSAGE", vec![ScriptValue::Str(text)]);
        }
    }
    let store = self_q.iter().next();
    let purchased = store
        .and_then(|s| s.0.player_bank_bag_slots_purchased())
        .unwrap_or(0);
    let fresh = open.banker.map(|_| {
        // The bag buttons' icons: each held bank bag's item → template → icon, the container
        // feed's own resolution (ask-once; `None` while the answer is in flight or slot empty).
        let mut bag_textures: [Option<String>; 6] = Default::default();
        if let Some(store) = store {
            for (i, tex) in bag_textures.iter_mut().enumerate() {
                *tex = store
                    .0
                    .player_bank_bag_slot(i as u8)
                    .filter(|g| *g != 0)
                    .and_then(|guid| {
                        let entry = items.object(guid)?.object_entry()?;
                        let display_id = items.template(entry, guid, &commands)?.display_info_id;
                        icons
                            .as_deref()
                            .and_then(|ic| ic.catalog.get(display_id))
                            .and_then(|d| d.icon.clone())
                    });
            }
        }
        BankState {
            num_purchased: u32::from(purchased),
            next_cost: prices
                .as_ref()
                .and_then(|p| p.0.next_slot_price(purchased))
                .unwrap_or(0),
            bag_textures,
        }
    });
    let banker_name = open
        .banker
        .and_then(|g| names.resolve(g, &commands).map(str::to_string));
    // A different banker while the window is already open is a real close+open (decision 0096 /
    // [`npc_switched`]) — both sounds, and the OnHide-queued close intent is consumed so the
    // drain doesn't clear the session we just re-opened.
    let switched = npc_switched(*last_banker, open.banker);
    if fresh == *last && !switched {
        return;
    }
    script.set_bank(fresh.clone());
    let name_arg = || vec![ScriptValue::Str(banker_name.clone().unwrap_or_default())];
    if switched {
        script.fire_event("BANKFRAME_CLOSED", vec![]);
        script.fire_event("BANKFRAME_OPENED", name_arg());
        let _ = script.take_bank_close();
    } else {
        match (&*last, &fresh) {
            (None, Some(_)) => script.fire_event("BANKFRAME_OPENED", name_arg()),
            // The purchased count moved while open — the no-packet buy confirmation (decision
            // 0604): the reference event repaints the bag row + purchase frame.
            (Some(_), Some(_)) => script.fire_event("PLAYERBANKBAGSLOTS_CHANGED", vec![]),
            (Some(_), None) => script.fire_event("BANKFRAME_CLOSED", vec![]),
            (None, None) => {}
        }
    }
    *last = fresh;
    *last_banker = open.banker;
}

/// Drain the Lua intents: `PurchaseSlot()` → the buy wire, `CloseBankFrame()` → the local clear.
fn drain_bank(
    script: Option<NonSendMut<UiScript>>,
    mut open: ResMut<BankOpen>,
    net: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    if script.take_bank_purchase() {
        if let Some(banker) = open.banker {
            debug!("bank: purchase next bag slot at {banker:#x}");
            let _ = net.0.send(ClientCommand::BuyBankSlot { guid: banker });
        }
    }
    if script.take_bank_close() && open.is_open() {
        debug!("bank: client-side close");
        open.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal-code → red-line map: the three vmangos failure codes print the reference
    /// GlobalStrings verbatim, OK stays silent, an unknown code prints its number.
    #[test]
    fn bank_slot_error_texts() {
        assert_eq!(
            bank_slot_error_text(bank_slot_result::FAILED_TOO_MANY).as_deref(),
            Some("You've reached your limit of bag slots!")
        );
        assert_eq!(
            bank_slot_error_text(bank_slot_result::INSUFFICIENT_FUNDS).as_deref(),
            Some("You can't afford that.")
        );
        assert_eq!(
            bank_slot_error_text(bank_slot_result::NOTBANKER).as_deref(),
            Some("That unit is not a banker!")
        );
        assert_eq!(bank_slot_error_text(bank_slot_result::OK), None);
        assert!(bank_slot_error_text(99).unwrap().contains("99"));
    }
}
