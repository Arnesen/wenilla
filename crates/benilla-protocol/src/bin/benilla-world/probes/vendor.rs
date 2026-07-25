//! `--vendor`: the vendor wire (decision 0081 phase 4). Auto-find the nearest streamed vendor,
//! `CMSG_LIST_INVENTORY` it, buy the cheapest row, and require a reaction — plus confirm the money
//! accessor (`PLAYER_FIELD_COINAGE`) reads and that a coinage delta lands on that field.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use benilla_protocol::{decode, SessionEvent};

use crate::probes::{Ctx, Probe};

/// vmangos `INTERACTION_DISTANCE` (`Objects/ObjectDefines.h:24`) — the range every NPC service
/// opcode is gated on, silently.
const INTERACTION_DISTANCE: f32 = 5.0;

pub(crate) struct Vendor;

impl Probe for Vendor {
    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        let world = &mut *cx.world;
        let session = &mut *cx.session;
        let self_guid = world.self_guid;

        // --vendor: list the nearest streamed vendor and buy its cheapest row; also confirm the money
        // accessor (`PLAYER_FIELD_COINAGE`) reads and that any coinage delta lands on that field.
        let sf = world
            .self_fields
            .as_mut()
            .context("no self descriptor — can't read coinage / find a vendor")?;
        let coinage_before = sf.player_money();
        println!(
            "\nPLAYER_FIELD_COINAGE at login = {} copper (money accessor)",
            coinage_before
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unsent".into())
        );

        let self_pos = world.tracked.get(&self_guid).map(|t| t.position);
        // The nearest advertised vendor (2D distance from us; z ignored — Northshire is ~flat here).
        let vendor = self_pos.and_then(|p| {
            world
                .vendors
                .iter()
                .map(|(&g, vp)| (g, *vp, (vp[0] - p[0]).hypot(vp[1] - p[1])))
                .min_by(|a, b| a.2.total_cmp(&b.2))
        });
        let (vendor_guid, vendor_pos, dist) = vendor.context(
            "no UNIT_NPC_FLAG_VENDOR creature streamed in range (fresh Northshire spawn has none — \
             re-run with a longer --seconds after moving toward a vendor, or grant GM to teleport)",
        )?;
        // The server refuses SILENTLY past interaction range, so an out-of-range send looks exactly
        // like a broken wire: no packet, a 5s drain, "nothing arrived". Name the real cause here
        // instead of leaving it to be re-diagnosed. VERIFIED vmangos `INTERACTION_DISTANCE = 5.0f`
        // (`Objects/ObjectDefines.h:24`), checked by `Player::CanInteractWithNPC`'s last gate
        // (`Objects/Player.cpp:2556`) — a 3D check with both bounding radii allowed for, so this 2D
        // screen is approximate and deliberately generous.
        if dist > INTERACTION_DISTANCE {
            bail!(
                "nearest vendor {vendor_guid:#x} is {dist:.1} yd away; vmangos refuses \
                 CMSG_LIST_INVENTORY past INTERACTION_DISTANCE ({INTERACTION_DISTANCE:.0} yd) and \
                 answers nothing at all. Stand on it and re-run:\n    \
                 --say \".go xyz {:.1} {:.1} {:.1} 0\"",
                vendor_pos[0],
                vendor_pos[1],
                vendor_pos[2],
            );
        }
        println!(
            "nearest vendor: guid {vendor_guid:#x} ({dist:.1} yd) — sending CMSG_LIST_INVENTORY"
        );
        session.list_inventory(vendor_guid)?;

        // Drain for the vendor's stock list.
        let mut items: Option<Vec<benilla_protocol::messages::VendorItem>> = None;
        let drain_until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < drain_until && items.is_none() {
            let Ok(msg) = session.recv() else { continue };
            for ev in decode(msg) {
                if let SessionEvent::VendorInventory {
                    vendor,
                    items: rows,
                } = ev
                {
                    if vendor == vendor_guid {
                        items = Some(rows);
                    }
                }
            }
        }
        let rows = items.context("no SMSG_LIST_INVENTORY arrived within 5s")?;
        println!("✅ list: SMSG_LIST_INVENTORY parsed {} row(s):", rows.len());
        for r in rows.iter().take(8) {
            let stock = if r.current_count == 0xFFFF_FFFF {
                "∞".to_string()
            } else {
                r.current_count.to_string()
            };
            println!(
                "  slot {:>2}  entry {:>6}  price {:>7}c  stock {stock}  buy_count {}",
                r.slot, r.entry, r.price, r.buy_count
            );
        }
        if rows.is_empty() {
            bail!("the vendor listed 0 rows — can't exercise a buy");
        }

        // Buy the cheapest priced, in-stock row (price > 0; current_count 0 = sold out — a limited
        // row we already drained; 0xFFFF_FFFF = unlimited, always buyable), one stack.
        let cheapest = rows
            .iter()
            .filter(|r| r.price > 0 && r.current_count != 0)
            .min_by_key(|r| r.price)
            .context("no priced, in-stock row to buy")?;
        println!(
            "buying cheapest: entry {} @ {}c → CMSG_BUY_ITEM",
            cheapest.entry, cheapest.price
        );
        session.buy_item(vendor_guid, cheapest.entry, 1)?;

        // Drain for a reaction: the item arriving, the stock updating, a coinage delta on exactly the
        // COINAGE field, or a decoded refusal — any one proves the round trip. Keep draining the full
        // window (not stopping at the first) so a coinage delta — the strongest evidence, it confirms
        // the accessor *index* — is captured even when the item-create lands first in the same batch.
        let mut round_trip: Option<String> = None;
        let mut coinage_delta: Option<String> = None;
        let drain_until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < drain_until && (round_trip.is_none() || coinage_delta.is_none()) {
            let Ok(msg) = session.recv() else { continue };
            for ev in decode(msg) {
                match ev {
                    SessionEvent::ObjectValues { guid: g, fields } if g == self_guid => {
                        sf.merge(fields);
                        if let (Some(before), Some(after)) = (coinage_before, sf.player_money()) {
                            if after != before && coinage_delta.is_none() {
                                coinage_delta = Some(format!(
                                    "coinage {before} → {after} copper (paid {}) — the delta landed on \
                                     PLAYER_FIELD_COINAGE (index-confirmed accessor)",
                                    before as i64 - after as i64
                                ));
                            }
                        }
                    }
                    SessionEvent::VendorBuyResult {
                        slot, new_count, ..
                    } => {
                        round_trip.get_or_insert(format!(
                            "SMSG_BUY_ITEM: vendor slot {slot} stock now {new_count} (purchase ok)"
                        ));
                    }
                    SessionEvent::ItemCreate {
                        guid: g, fields, ..
                    } => {
                        let entry = fields.object_entry().unwrap_or(0);
                        if entry == cheapest.entry {
                            round_trip.get_or_insert(format!(
                                "the bought item arrived (ItemCreate guid {g:#x}, entry {entry})"
                            ));
                        }
                    }
                    SessionEvent::VendorBuyFailed {
                        item_entry, reason, ..
                    } => {
                        round_trip.get_or_insert(format!(
                            "SMSG_BUY_FAILED for entry {item_entry} (reason {reason}) — round trip proven"
                        ));
                    }
                    _ => {}
                }
            }
        }
        let v = round_trip.context("no reaction to CMSG_BUY_ITEM within 5s")?;
        println!("✅ vendor buy: {v}.");
        if let Some(d) = coinage_delta {
            println!("✅ money accessor: {d}.");
        }
        Ok(())
    }
}
