//! Oracle-free golden tests for the gossip/vendor interaction arc's protocol layer (Phase 1 of the
//! gossip/vendor arc): `CMSG_GOSSIP_HELLO`/`SELECT_OPTION`, `CMSG_NPC_TEXT_QUERY`, and the vendor
//! `LIST_INVENTORY`/`BUY_ITEM`/`SELL_ITEM` family. Split out from `messages.rs` (same idioms —
//! `hx(...)` golden CMSG bodies, hand-built SMSG bodies round-tripped through `parse_server`) since
//! it's a self-contained wire family or size.

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages;
use benilla_protocol::ServerPacket;

fn hx(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn gossip_bodies_golden() {
    // CMSG_GOSSIP_HELLO (vmangos Npc.cpp:3) / CMSG_LIST_INVENTORY (Item.cpp:94) share the same
    // full-guid body shape.
    assert_eq!(
        messages::gossip_hello(0x1234_5678_9abc_def0),
        hx("f0debc9a78563412"),
        "CMSG_GOSSIP_HELLO body"
    );

    // CMSG_NPC_TEXT_QUERY (vmangos Npc.cpp:8-12): u32 textID, u64 guid.
    assert_eq!(
        messages::npc_text_query(55, 0x1234_5678_9abc_def0),
        hx(concat!("37000000", "f0debc9a78563412")),
        "CMSG_NPC_TEXT_QUERY body"
    );

    // CMSG_GOSSIP_SELECT_OPTION (vmangos Npc.cpp:78-86): guid, gossipListId, then an optional
    // trailing code cstring — appended only for a coded option with a real code, omitted entirely
    // otherwise (the server reads it only when the buffer is non-empty).
    assert_eq!(
        messages::gossip_select_option(0x1234_5678_9abc_def0, 3, None),
        hx(concat!("f0debc9a78563412", "03000000")),
        "CMSG_GOSSIP_SELECT_OPTION body, no code"
    );
    let mut with_code = hx(concat!("f0debc9a78563412", "03000000"));
    with_code.extend_from_slice(b"1234\0");
    assert_eq!(
        messages::gossip_select_option(0x1234_5678_9abc_def0, 3, Some("1234")),
        with_code,
        "CMSG_GOSSIP_SELECT_OPTION body, coded option with a real code"
    );
}

#[test]
fn vendor_bodies_golden() {
    // CMSG_LIST_INVENTORY (vmangos Item.cpp:94): a full guid.
    assert_eq!(
        messages::list_inventory(0x1234_5678_9abc_def0),
        hx("f0debc9a78563412"),
        "CMSG_LIST_INVENTORY body"
    );

    // CMSG_BUY_ITEM (vmangos Item.cpp:104-110): vendorGuid, item ENTRY (not muid), count, unk1(0).
    // entry 2488 is the live-verified creature-54 vendor's Gladius row (decision 0081).
    assert_eq!(
        messages::buy_item(0x1234_5678_9abc_def0, 2488, 1),
        hx(concat!("f0debc9a78563412", "b8090000", "01", "00")),
        "CMSG_BUY_ITEM body"
    );

    // CMSG_SELL_ITEM (vmangos Item.cpp:87-92): vendorGuid, itemGuid, count (0 = whole stack).
    assert_eq!(
        messages::sell_item(0x1234_5678_9abc_def0, 0x42, 0),
        hx(concat!("f0debc9a78563412", "4200000000000000", "00")),
        "CMSG_SELL_ITEM body"
    );
}

#[test]
fn gossip_message_wire() {
    use benilla_protocol::messages::{GossipOption, QuestOption};

    // SMSG_GOSSIP_MESSAGE (vmangos GossipDef.cpp:180-225, the 1.12 shape): objectGuid, textId,
    // optionCount + options (index, icon, coded, message), questOptionCount + quest options. Zero
    // quest options — the common case for a pure vendor/gossip NPC.
    let mut body = 0xAAu64.to_le_bytes().to_vec(); // objectGuid
    body.extend_from_slice(&100u32.to_le_bytes()); // textId
    body.extend_from_slice(&2u32.to_le_bytes()); // optionCount
    body.extend_from_slice(&0u32.to_le_bytes()); // option 0: index
    body.push(0); // icon: chat bubble
    body.push(0); // coded: false
    body.extend_from_slice(b"Train me\0");
    body.extend_from_slice(&1u32.to_le_bytes()); // option 1: index
    body.push(1); // icon: vendor
    body.push(1); // coded: true
    body.extend_from_slice(b"Show me your wares\0");
    body.extend_from_slice(&0u32.to_le_bytes()); // questOptionCount: 0

    match messages::parse_server(messages::opcode::SMSG_GOSSIP_MESSAGE, &body).unwrap() {
        ServerPacket::GossipMessage {
            npc,
            text_id,
            options,
            quests,
        } => {
            assert_eq!((npc, text_id), (0xAA, 100));
            assert_eq!(
                options,
                vec![
                    GossipOption {
                        index: 0,
                        icon: 0,
                        coded: false,
                        message: "Train me".into(),
                    },
                    GossipOption {
                        index: 1,
                        icon: 1,
                        coded: true,
                        message: "Show me your wares".into(),
                    },
                ]
            );
            assert!(quests.is_empty());
        }
        other => panic!("gossip message, got {}", other.name()),
    }

    // decode() surfaces a GossipMenu event carrying the (empty here) quest block.
    let packet = messages::parse_server(messages::opcode::SMSG_GOSSIP_MESSAGE, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::GossipMenu {
            npc,
            text_id,
            options,
            quests,
        } => {
            assert_eq!((npc, text_id, options.len()), (0xAA, 100, 2));
            assert!(quests.is_empty());
        }
        other => panic!("gossip menu event, got {other:?}"),
    }

    // Nonzero quest options: parsed for byte alignment, kept on the wire struct (arc ignores them
    // downstream — quest-giver flows are out of scope, decision 0081).
    let mut body_q = 0xBBu64.to_le_bytes().to_vec();
    body_q.extend_from_slice(&200u32.to_le_bytes()); // textId
    body_q.extend_from_slice(&0u32.to_le_bytes()); // optionCount: 0
    body_q.extend_from_slice(&1u32.to_le_bytes()); // questOptionCount: 1
    body_q.extend_from_slice(&42u32.to_le_bytes()); // questId
    body_q.extend_from_slice(&7u32.to_le_bytes()); // icon
    body_q.extend_from_slice(&10u32.to_le_bytes()); // level
    body_q.extend_from_slice(b"A Quest\0");

    match messages::parse_server(messages::opcode::SMSG_GOSSIP_MESSAGE, &body_q).unwrap() {
        ServerPacket::GossipMessage {
            npc,
            options,
            quests,
            ..
        } => {
            assert_eq!(npc, 0xBB);
            assert!(options.is_empty());
            assert_eq!(
                quests,
                vec![QuestOption {
                    quest_id: 42,
                    icon: 7,
                    level: 10,
                    title: "A Quest".into(),
                }]
            );
        }
        other => panic!("gossip message with quests, got {}", other.name()),
    }

    // SMSG_GOSSIP_COMPLETE — empty body.
    match messages::parse_server(messages::opcode::SMSG_GOSSIP_COMPLETE, &[]).unwrap() {
        ServerPacket::GossipComplete => {}
        other => panic!("gossip complete, got {}", other.name()),
    }
    let packet = messages::parse_server(messages::opcode::SMSG_GOSSIP_COMPLETE, &[]).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::GossipComplete => {}
        other => panic!("gossip complete event, got {other:?}"),
    }
}

#[test]
fn npc_text_update_greeting_extraction() {
    // SMSG_NPC_TEXT_UPDATE (vmangos GossipDef.cpp:298-369): always 8 blocks of {f32 probability,
    // cstr text0 (male), cstr text1 (female), u32 languageId, 3x(u32 emoteDelay, u32 emoteId)}.
    fn block(w: &mut Vec<u8>, probability: f32, text0: &str, text1: &str) {
        w.extend_from_slice(&probability.to_le_bytes());
        w.extend_from_slice(text0.as_bytes());
        w.push(0);
        w.extend_from_slice(text1.as_bytes());
        w.push(0);
        w.extend_from_slice(&0u32.to_le_bytes()); // languageId
        for _ in 0..3 {
            w.extend_from_slice(&0u32.to_le_bytes()); // emoteDelay
            w.extend_from_slice(&0u32.to_le_bytes()); // emoteId
        }
    }

    // Block 3 (probability 0.5) is the highest-probability block with a non-empty male text — it
    // wins over both lower-probability blocks and the higher-index female-only block 6.
    let mut body = 321u32.to_le_bytes().to_vec(); // textID
    block(&mut body, 0.1, "Low probability greeting", "");
    block(&mut body, 0.0, "", "");
    block(&mut body, 0.0, "", "");
    block(&mut body, 0.5, "Welcome, $N!", "Welcome, traveler!");
    block(&mut body, 0.0, "", "");
    block(&mut body, 0.0, "", "");
    block(&mut body, 0.2, "", "Female-only greeting");
    block(&mut body, 0.0, "", "");

    match messages::parse_server(messages::opcode::SMSG_NPC_TEXT_UPDATE, &body).unwrap() {
        ServerPacket::NpcText { text_id, greeting } => {
            assert_eq!(text_id, 321);
            assert_eq!(greeting, "Welcome, $N!");
        }
        other => panic!("npc text update, got {}", other.name()),
    }

    let packet = messages::parse_server(messages::opcode::SMSG_NPC_TEXT_UPDATE, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::NpcGreeting { text_id, greeting } => {
            assert_eq!((text_id, greeting.as_str()), (321, "Welcome, $N!"));
        }
        other => panic!("npc greeting event, got {other:?}"),
    }

    // Male-empty fallback: the winning block's text0 is empty, text1 is used instead.
    let mut fallback_body = 322u32.to_le_bytes().to_vec();
    for _ in 0..7 {
        block(&mut fallback_body, 0.0, "", "");
    }
    block(&mut fallback_body, 1.0, "", "Only the ladies get a hello");
    match messages::parse_server(messages::opcode::SMSG_NPC_TEXT_UPDATE, &fallback_body).unwrap() {
        ServerPacket::NpcText { text_id, greeting } => {
            assert_eq!(text_id, 322);
            assert_eq!(greeting, "Only the ladies get a hello");
        }
        other => panic!("npc text fallback, got {}", other.name()),
    }
}

#[test]
fn vendor_list_inventory_wire() {
    use benilla_protocol::messages::VendorItem;

    // SMSG_LIST_INVENTORY (vmangos ItemHandler.cpp:741-810): vendorGuid, u8 count, count x {muid,
    // entry, displayId, currentCount, price, maxDurability, buyCount}. Two rows keyed off the
    // live-verified creature-54 weapon vendor data (decision 0081): Gladius (entry
    // 2488, price 536, display 22078, maxcount 0 -> unlimited) and a limited-stock second row.
    // Trailing junk proves the parser stops reading rows at `count`.
    let mut body = 0xCCu64.to_le_bytes().to_vec();
    body.push(2); // count
    for v in [1u32, 2488, 22078, 0xFFFF_FFFF, 536, 35, 1] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    for v in [2u32, 2489, 22079, 5, 342, 40, 1] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    body.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // trailing junk, must not become a third row

    match messages::parse_server(messages::opcode::SMSG_LIST_INVENTORY, &body).unwrap() {
        ServerPacket::VendorList { vendor, items } => {
            assert_eq!(vendor, 0xCC);
            assert_eq!(
                items,
                vec![
                    VendorItem {
                        slot: 1,
                        entry: 2488,
                        display_id: 22078,
                        current_count: 0xFFFF_FFFF,
                        price: 536,
                        max_durability: 35,
                        buy_count: 1,
                    },
                    VendorItem {
                        slot: 2,
                        entry: 2489,
                        display_id: 22079,
                        current_count: 5,
                        price: 342,
                        max_durability: 40,
                        buy_count: 1,
                    },
                ]
            );
        }
        other => panic!("vendor list, got {}", other.name()),
    }

    let packet = messages::parse_server(messages::opcode::SMSG_LIST_INVENTORY, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::VendorInventory { vendor, items } => {
            assert_eq!((vendor, items.len()), (0xCC, 2));
        }
        other => panic!("vendor inventory event, got {other:?}"),
    }

    // Empty stock: count = 0 followed by the trailing error byte (ItemHandler.cpp:728-733,
    // 806-809) — the parser must tolerate it (the row loop simply never runs).
    let mut empty_body = 0xDDu64.to_le_bytes().to_vec();
    empty_body.push(0); // count
    empty_body.push(0); // trailing errorByte(0)
    match messages::parse_server(messages::opcode::SMSG_LIST_INVENTORY, &empty_body).unwrap() {
        ServerPacket::VendorList { vendor, items } => {
            assert_eq!(vendor, 0xDD);
            assert!(items.is_empty());
        }
        other => panic!("empty vendor list, got {}", other.name()),
    }
}

#[test]
fn vendor_buy_sell_result_wire() {
    use benilla_protocol::messages::{buy_result, sell_result};

    // SMSG_BUY_ITEM (vmangos Item.cpp:190-196): vendorGuid, vendorSlot (1-based), newCount
    // (0xFFFF_FFFF unlimited), purchaseCount. Only the vendor stock display changes here.
    let mut body = 0xCCu64.to_le_bytes().to_vec();
    for v in [1u32, 0xFFFF_FFFF, 1] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    match messages::parse_server(messages::opcode::SMSG_BUY_ITEM, &body).unwrap() {
        ServerPacket::BuyItem {
            vendor,
            slot,
            new_count,
            purchase_count,
        } => {
            assert_eq!(
                (vendor, slot, new_count, purchase_count),
                (0xCC, 1, 0xFFFF_FFFF, 1)
            );
        }
        other => panic!("buy item, got {}", other.name()),
    }
    let packet = messages::parse_server(messages::opcode::SMSG_BUY_ITEM, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::VendorBuyResult { vendor, slot, .. } => {
            assert_eq!((vendor, slot), (0xCC, 1));
        }
        other => panic!("vendor buy result event, got {other:?}"),
    }

    // SMSG_SELL_ITEM (vmangos Item.cpp:183-188) — the error path only: vendorGuid, itemGuid, reason.
    let mut body = 0xCCu64.to_le_bytes().to_vec();
    body.extend_from_slice(&0x42u64.to_le_bytes());
    body.push(sell_result::CANT_FIND_ITEM);
    match messages::parse_server(messages::opcode::SMSG_SELL_ITEM, &body).unwrap() {
        ServerPacket::SellItemResult {
            vendor,
            item_guid,
            reason,
        } => {
            assert_eq!(
                (vendor, item_guid, reason),
                (0xCC, 0x42, sell_result::CANT_FIND_ITEM)
            );
        }
        other => panic!("sell item error, got {}", other.name()),
    }
    let packet = messages::parse_server(messages::opcode::SMSG_SELL_ITEM, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::VendorSellFailed { reason, .. } => {
            assert_eq!(reason, sell_result::CANT_FIND_ITEM);
        }
        other => panic!("vendor sell failed event, got {other:?}"),
    }

    // SMSG_BUY_FAILED (vmangos Item.h:277): vendorGuid, itemEntry, reason.
    let mut body = 0xCCu64.to_le_bytes().to_vec();
    body.extend_from_slice(&2488u32.to_le_bytes());
    body.push(buy_result::NOT_ENOUGH_MONEY);
    match messages::parse_server(messages::opcode::SMSG_BUY_FAILED, &body).unwrap() {
        ServerPacket::BuyFailed {
            vendor,
            item_entry,
            reason,
        } => {
            assert_eq!(
                (vendor, item_entry, reason),
                (0xCC, 2488, buy_result::NOT_ENOUGH_MONEY)
            );
        }
        other => panic!("buy failed, got {}", other.name()),
    }
    let packet = messages::parse_server(messages::opcode::SMSG_BUY_FAILED, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::VendorBuyFailed { reason, .. } => {
            assert_eq!(reason, buy_result::NOT_ENOUGH_MONEY);
        }
        other => panic!("vendor buy failed event, got {other:?}"),
    }

    // Result enum values (vmangos ItemDefines.h:120-141) — pinned so a future edit can't drift them.
    assert_eq!(
        (
            buy_result::CANT_FIND_ITEM,
            buy_result::ITEM_ALREADY_SOLD,
            buy_result::NOT_ENOUGH_MONEY,
            buy_result::SELLER_DONT_LIKE_YOU,
            buy_result::DISTANCE_TOO_FAR,
            buy_result::ITEM_SOLD_OUT,
            buy_result::CANT_CARRY_MORE,
            buy_result::RANK_REQUIRE,
            buy_result::REPUTATION_REQUIRE,
        ),
        (0, 1, 2, 4, 5, 7, 8, 11, 12)
    );
    assert_eq!(
        (
            sell_result::CANT_FIND_ITEM,
            sell_result::CANT_SELL_ITEM,
            sell_result::CANT_FIND_VENDOR,
            sell_result::YOU_DONT_OWN_THAT_ITEM,
            sell_result::UNK,
            sell_result::ONLY_EMPTY_BAG,
        ),
        (1, 2, 3, 4, 5, 6)
    );
}
