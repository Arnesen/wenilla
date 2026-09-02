//! World opcode numbers (verified against `wow_world_messages` 0.3 / wowdev + vmangos
//! `Opcodes_1_12_1.h`). All are 16-bit values — the client packet header's opcode field is 4 bytes
//! wide but every value fits `u16`, so we keep one natural type and the senders widen to the wire field.

// Server → client.
pub const SMSG_CHAR_CREATE: u16 = 0x003A;
pub const SMSG_CHAR_ENUM: u16 = 0x003B;
// The delete pair (VERIFIED vmangos `Opcodes_1_12_1.h`: 56/60): CMSG body a full u64 guid,
// SMSG body the result byte (`CHAR_DELETE_SUCCESS` = 0x39, `SharedDefines.h` ResponseCodes).
pub const CMSG_CHAR_DELETE: u16 = 0x0038;
pub const SMSG_CHAR_DELETE: u16 = 0x003C;
pub const SMSG_NAME_QUERY_RESPONSE: u16 = 0x0051; // 81
pub const SMSG_CREATURE_QUERY_RESPONSE: u16 = 0x0061; // 97
/// VERIFIED vmangos `Opcodes_1_12_1.h:86`: 83. Answers `CMSG_PET_NAME_QUERY` — the only query that
/// can name a pet, whose guid carries no template entry ([`crate::guid::pet_number`]). Body: `u32
/// petNumber`, cstring name, `u32 nameTimestamp` (`Server/Packets/Pet.cpp:79-84`). The server simply
/// does not reply when the guid is not a live pet bearing that number (`PetHandler.cpp:190-192`), so
/// there is no "unknown" answer shape to model.
pub const SMSG_PET_NAME_QUERY_RESPONSE: u16 = 0x0053; // 83
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 95 (decision 0236). Answers `CMSG_GAMEOBJECT_QUERY`; body
/// (`GameObjectQueryInfo`) in [`super::gameobject`].
pub const SMSG_GAMEOBJECT_QUERY_RESPONSE: u16 = 0x005F; // 95
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 93. Answers `CMSG_QUEST_QUERY`; body in
/// [`super::quest::read_quest_query_response`] ([`super::QuestTemplate`]).
pub const SMSG_QUEST_QUERY_RESPONSE: u16 = 0x005D; // 93

// The **page-text** query pair (VERIFIED vmangos `Opcodes_1_12_1.h`: 90/91) — the ask-once cache
// behind every readable BOOK, and a different one from the mail letter's `CMSG_ITEM_TEXT_QUERY`
// further down: a `PageText.wdb` id names one *page*, chained by `nextPageId`, and it is what a
// readable item TEMPLATE (`PageText`) and a world book/plaque GameObject (`GAMEOBJECT_TYPE_TEXT`'s
// `data[0]`) both read from. Bodies in [`super::page_text`]; decision 1105.
pub const CMSG_PAGE_TEXT_QUERY: u16 = 0x005A; // 90
pub const SMSG_PAGE_TEXT_QUERY_RESPONSE: u16 = 0x005B; // 91

pub const SMSG_NEW_WORLD: u16 = 0x003E;
// The far-teleport preamble pair (VERIFIED vmangos `Opcodes_1_12_1.h`: 63/64). TRANSFER_PENDING's
// optional transport block decides how NEW_WORLD's coordinates read — boat-local when present,
// world otherwise (decision 0455; vmangos `Player.cpp:2065-2068` / `SendNewWorld`).
pub const SMSG_TRANSFER_PENDING: u16 = 0x003F;
pub const SMSG_TRANSFER_ABORTED: u16 = 0x0040;
pub const SMSG_LOGIN_SETTIMESPEED: u16 = 0x0042;
pub const SMSG_BINDPOINTUPDATE: u16 = 0x0155;
/// 295 — the per-item-class proficiency mask (VERIFIED vmangos `Opcodes_1_12_1.h` + Skill.h).
pub const SMSG_SET_PROFICIENCY: u16 = 0x0127;
pub const SMSG_LOGOUT_RESPONSE: u16 = 0x004C;
pub const SMSG_LOGOUT_COMPLETE: u16 = 0x004D;
/// 79 — the ack for [`CMSG_LOGOUT_CANCEL`] (VERIFIED vmangos `Opcodes_1_12_1.h`), empty body.
pub const SMSG_LOGOUT_CANCEL_ACK: u16 = 0x004F;
pub const SMSG_UPDATE_OBJECT: u16 = 0x00A9;
pub const SMSG_DESTROY_OBJECT: u16 = 0x00AA;
// The cinematic pair (VERIFIED vmangos `Opcodes_1_12_1.h`: 250/252). The server sends the trigger
// on a character's first-ever login (the race intro) and for GameObject type-13 cameras; while one
// runs UNACKED, vmangos re-anchors object visibility to the flying cinematic camera
// (`Player::UpdateCinematic`), despawning everything around the body — so the client must answer.
pub const SMSG_TRIGGER_CINEMATIC: u16 = 0x00FA;
/// Sent when playback advances from one camera of a multi-camera `CinematicSequences` row to the
/// next — empty body, exactly like the completion ack. VERIFIED in the reference at `0x48efe0`
/// (`NextCamera`): it bumps the camera index, and the send is `push 0xfb; call 0x418190` with
/// nothing written between the CDataStore open and `call 0x5ab630`.
pub const CMSG_NEXT_CINEMATIC_CAMERA: u16 = 0x00FB;
pub const CMSG_COMPLETE_CINEMATIC: u16 = 0x00FC;
pub const SMSG_MONSTER_MOVE: u16 = 0x00DD;
pub const SMSG_INITIALIZE_FACTIONS: u16 = 0x0122;
/// A faction became visible in the reputation pane (VERIFIED vmangos `Opcodes_1_12_1.h`: 291,
/// sender `ReputationMgr::SendVisible`) — body one `u32` reputation-list slot. The server sets the
/// slot's `FACTION_FLAG_VISIBLE` on first contact and pushes *only* this, never a fresh standing:
/// a client that ignores it keeps a correct standing on a row the pane refuses to list.
pub const SMSG_SET_FACTION_VISIBLE: u16 = 0x0123;
pub const SMSG_SET_FACTION_STANDING: u16 = 0x0124;
/// The reputation pane's three send verbs (VERIFIED vmangos `Opcodes.cpp`'s `DEFINE_HANDLER`
/// registrations: 293, 791, 792). Bodies in [`super::reputation`]; none is acked.
pub const CMSG_SET_FACTION_ATWAR: u16 = 0x0125; // 293
pub const CMSG_SET_FACTION_INACTIVE: u16 = 0x0317; // 791
pub const CMSG_SET_WATCHED_FACTION: u16 = 0x0318; // 792
pub const SMSG_AUTH_CHALLENGE: u16 = 0x01EC;
pub const SMSG_AUTH_RESPONSE: u16 = 0x01EE;
/// The Warden anticheat challenge. A server that sends this starts a response-timeout clock
/// (vmangos `Warden::BeginTimeoutClock`, `Warden.ClientResponseDelay` — default 30 s) and kicks
/// unconditionally when it expires (`Warden::Update`), so a client that can't answer cannot stay
/// in the world. benilla does not implement Warden; [`crate::WorldSession::connect`] refuses such
/// a server at the handshake rather than entering a 30-second kick/reconnect cycle.
pub const SMSG_WARDEN_DATA: u16 = 0x02E6;
pub const SMSG_COMPRESSED_UPDATE_OBJECT: u16 = 0x01F6;
/// A zlib envelope holding a **batch of whole movement packets** (763, VERIFIED vmangos
/// `Opcodes_1_12_1.h`). Not an edge case: vmangos moves a session onto this carrier the moment it
/// has pushed 300 movement packets to it inside ten seconds (`Compression.Movement.Count`), which
/// one nearby player moving at frame cadence reaches in about five. See
/// [`super::ServerPacket::CompressedMoves`].
pub const SMSG_COMPRESSED_MOVES: u16 = 0x02FB;
pub const SMSG_LOGIN_VERIFY_WORLD: u16 = 0x0236;
// The server-pushed sound trio (1.12.1 values VERIFIED vmangos `Opcodes_1_12_1.h`:
// 631/632/722).
pub const SMSG_PLAY_MUSIC: u16 = 0x0277;
pub const SMSG_PLAY_OBJECT_SOUND: u16 = 0x0278;
pub const SMSG_PLAY_SOUND: u16 = 0x02D2;
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 756.
pub const SMSG_WEATHER: u16 = 0x02F4;
// The emote pair + our send side (VERIFIED vmangos `Opcodes_1_12_1.h`: 259/260/261).
pub const SMSG_EMOTE: u16 = 0x0103;
pub const CMSG_TEXT_EMOTE: u16 = 0x0104;
pub const SMSG_TEXT_EMOTE: u16 = 0x0105;
// The spell-book/action-bar/cast/attack set (VERIFIED vmangos `Opcodes_1_12_1.h`:
// 297/298/304/323/324 in, 302/321/322 out). Bodies in [`super::spells`].
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 88. Body in [`items`].
pub const SMSG_ITEM_QUERY_SINGLE_RESPONSE: u16 = 0x0058;
// The item-move CMSG family (VERIFIED vmangos `Opcodes_1_12_1.h:269-273`; bodies in [`items`]).
pub const CMSG_AUTOEQUIP_ITEM: u16 = 0x010A; // 266
pub const CMSG_AUTOSTORE_BAG_ITEM: u16 = 0x010B; // 267
pub const CMSG_SWAP_ITEM: u16 = 0x010C; // 268
pub const CMSG_SWAP_INV_ITEM: u16 = 0x010D; // 269
pub const CMSG_SPLIT_ITEM: u16 = 0x010E; // 270
pub const CMSG_DESTROYITEM: u16 = 0x0111; // 273 — the popup-confirmed world-drop delete (0216 §3)
pub const SMSG_INVENTORY_CHANGE_FAILURE: u16 = 0x0112; // 274

// The player-to-player trade family (VERIFIED vmangos `Opcodes_1_12_1.h:278-289`,
// `Handlers/TradeHandler.cpp`, `Server/Packets/Trade.{h,cpp}`; bodies/parses in [`trade`],
// decision 0592 P0). CMSG bodies: INITIATE = u64 target; ACCEPT = u32 (read-skipped);
// SET_ITEM = 3×u8 (tradeSlot, bag, slot); CLEAR_ITEM = u8 tradeSlot; SET_GOLD = u32 copper;
// BEGIN/BUSY/IGNORE/UNACCEPT/CANCEL are empty. SMSG: TRADE_STATUS = u32 status (+ a per-status
// tail); TRADE_STATUS_EXTENDED = the 444-byte item/gold snapshot.
pub const CMSG_INITIATE_TRADE: u16 = 0x0116; // 278
pub const CMSG_BEGIN_TRADE: u16 = 0x0117; // 279
pub const CMSG_BUSY_TRADE: u16 = 0x0118; // 280
pub const CMSG_IGNORE_TRADE: u16 = 0x0119; // 281
pub const CMSG_ACCEPT_TRADE: u16 = 0x011A; // 282
pub const CMSG_UNACCEPT_TRADE: u16 = 0x011B; // 283
pub const CMSG_CANCEL_TRADE: u16 = 0x011C; // 284
pub const CMSG_SET_TRADE_ITEM: u16 = 0x011D; // 285
pub const CMSG_CLEAR_TRADE_ITEM: u16 = 0x011E; // 286
pub const CMSG_SET_TRADE_GOLD: u16 = 0x011F; // 287
pub const SMSG_TRADE_STATUS: u16 = 0x0120; // 288
pub const SMSG_TRADE_STATUS_EXTENDED: u16 = 0x0121; // 289
/// Load ammo into the ammo "slot" (`PLAYER_AMMO_ID`) — VERIFIED wow-re `cursor-dragdrop-slots.md`
/// (the client's auto-equip sender `0x5e1480` forks ammo-class here) + vmangos `Opcodes_1_12_1.h`:
/// 616. Body in [`items::set_ammo`]. Note the hex/decimal trap: `0x0268` (this) ≠ `0x010C`
/// (`CMSG_SWAP_ITEM`, "opcode 268" in decimal). Decision 0526.
pub const CMSG_SET_AMMO: u16 = 0x0268; // 616
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 296 — `MiscHandler.cpp:885`'s `HandleSetActionButtonOpcode`
/// (decision 0216 §7/0218 §4). Body in [`super::action_bar::set_action_button`].
pub const CMSG_SET_ACTION_BUTTON: u16 = 0x0128; // 296
/// The four extra action bars' visibility byte (`PLAYER_FIELD_BYTES` byte 2) — VERIFIED at the
/// bytes, wow-re `system/ui/scratch/action-bar-toggles.md` §3: `0x4e771d push 0x2bf` is the ONE
/// site image-wide that emits this opcode, and the frame it builds is `u32 opcode` + a single `u8`
/// and nothing else (`0x418190` PutUInt32 then `0x418070` PutUInt8; `0x5379b3` computes the
/// payload as size − read = **5**). Body in [`super::action_bar::set_actionbar_toggles`].
///
/// Corroborated by vmangos `Opcodes_1_12_1.h`: 703 → `HandleSetActionBarTogglesOpcode`
/// (`Packets/Misc.cpp:150-153` reads one `uint8`; `Handlers/MiscHandler.cpp:923-932` stores it with
/// `SetByteValue(PLAYER_FIELD_BYTES, 2, …)`).
pub const CMSG_SET_ACTIONBAR_TOGGLES: u16 = 0x02BF; // 703
pub const SMSG_ACTION_BUTTONS: u16 = 0x0129; // 297
pub const SMSG_INITIAL_SPELLS: u16 = 0x012A; // 298

// The incremental spell-learn pair (VERIFIED vmangos `Opcodes_1_12_1.h`: 299/300) — a spell added to
// the book after login (trainer purchase / quest reward / level-up), and the rank-up in-place swap.
// Bodies in [`super::spells`] (decision 0237).
pub const SMSG_LEARNED_SPELL: u16 = 0x012B; // 299
pub const SMSG_SUPERCEDED_SPELL: u16 = 0x012C; // 300

/// The **third** member of that family and the only one that shrinks the book (VERIFIED vmangos
/// `Opcodes_1_12_1.h`: 515; `Player::SendSpellRemoved`, the tail of `Player::RemoveSpell`). Sent
/// once per spell the server takes away — a talent wipe sends one for every rank of every talent,
/// which is the whole reason decision 1584 exists. Body in [`super::spellbook`]: one `u16`.
pub const SMSG_REMOVED_SPELL: u16 = 0x0203; // 515
pub const SMSG_CAST_RESULT: u16 = 0x0130; // 304

// The pet action bar's inbound wire (decision 0982; VERIFIED vmangos `Opcodes_1_12_1.h`:
// 377/378/710/312). Bodies in [`super::pet`].
/// The control handoff (VERIFIED vmangos `Opcodes_1_12_1.h`: 345; built at
/// `Server/Packets/Misc.cpp:677-682`). Body: **packed** mover guid, then a `u8` `allowMove`.
///
/// It is the whole of possession's control half, and it is a *statement about one unit*, not a
/// swap: the server always sends it to our own session, naming some unit and whether we may drive
/// it. Mind Control's start sends the caster `(victim, 1)` and the victim `(victim, 0)`; the end
/// sends the caster `(self, 1)` **then** `(victim, 0)`, and the victim `(victim, 1)`.
///
/// **Two consequences the server leans on us for.** It expects a
/// [`CMSG_SET_ACTIVE_MOVER`] reply and drops every `MSG_MOVE_*` for the new mover until it
/// arrives (`Player::GetConfirmedMover`). And it never immobilises a possessed *player* — no root,
/// no speed change, and its `StopMoving()` is a documented no-op for one — nor does it validate
/// their movement, so `allowMove = 0` for our own guid is the **only** thing standing between a
/// mind-controlled player and walking away. Enforcing that is the client's job.
pub const SMSG_CLIENT_CONTROL_UPDATE: u16 = 0x0159; // 345
/// The whole pet bar in one body — the ten slots, the react/command state, the pet's spell list
/// and its cooldowns. Its **8-byte guid-only form is the teardown** (`Player::RemovePetActionBar`),
/// and the only signal that the bar has gone away.
pub const SMSG_PET_SPELLS: u16 = 0x0179; // 377
/// The state-only refresh: the same four state bytes, with no bar behind them.
pub const SMSG_PET_MODE: u16 = 0x017A; // 378
/// One reason byte for a refused pet order (the red error line).
pub const SMSG_PET_ACTION_FEEDBACK: u16 = 0x02C6; // 710
/// The pet's twin of [`SMSG_CAST_RESULT`] — the same `SpellCastResult` vocabulary, its own opcode
/// because the caster that failed is the pet, not us.
pub const SMSG_PET_CAST_FAILED: u16 = 0x0138; // 312

/// The client's ack that a server-authored spline (`SMSG_MONSTER_MOVE` to our own guid — Charge,
/// knockback, taxi) finished. Body: a `MovementInfo` at the endpoint, the `splineId` being acked,
/// and a trailing float the server `read_skip`s (VERIFIED vmangos `Opcodes_1_12_1.h`: 713;
/// `MoveSplineDone::ReadFromWorldPacket`).
pub const CMSG_MOVE_SPLINE_DONE: u16 = 0x02C9; // 713
pub const SMSG_ATTACKSTART: u16 = 0x0143; // 323
pub const SMSG_ATTACKSTOP: u16 = 0x0144; // 324
pub const SMSG_ATTACKERSTATEUPDATE: u16 = 0x014A; // 330
/// Creature aggro/alert flare (VERIFIED vmangos `Opcodes_1_12_1.h`: 316; body in
/// [`super::attack::read_ai_reaction`]).
pub const SMSG_AI_REACTION: u16 = 0x013C; // 316

// The spell-visual pipeline wire (VERIFIED vmangos `Opcodes_1_12_1.h`: 305/306/499/668/678;
// decision 0099 phase 1). Bodies in [`super::spells`].
pub const SMSG_SPELL_START: u16 = 0x0131; // 305
pub const SMSG_SPELL_GO: u16 = 0x0132; // 306
pub const SMSG_PLAY_SPELL_VISUAL: u16 = 0x01F3; // 499
pub const SMSG_CANCEL_AUTO_REPEAT: u16 = 0x029C; // 668
pub const SMSG_SPELL_FAILED_OTHER: u16 = 0x02A6; // 678

/// `SMSG_SPELL_UPDATE_CHAIN_TARGETS` (VERIFIED vmangos `Opcodes_1_12_1.h`: 816) — the **only**
/// source of the client's chain-target list, and so the only way a beam gets more than one hop.
/// The client's handler is `0x6e9820`; it fills the growable array at `unit+0xd44`, which the
/// chain `CharProc` consumes once and zeroes (decision 0955). Body in [`super::spells`].
pub const SMSG_SPELL_UPDATE_CHAIN_TARGETS: u16 = 0x0330; // 816

// The cooldown wire (VERIFIED vmangos `Opcodes_1_12_1.h`: 308/176/309/478/481; the client
// handlers are byte-verified in wow-re `wave-handlers.md` — 0x6e9460/0x6e95d0/0x6e9670/0x6e9730;
// decision 0137 phase 4). Bodies in [`super::spells`].
pub const SMSG_SPELL_COOLDOWN: u16 = 0x0134; // 308
pub const SMSG_ITEM_COOLDOWN: u16 = 0x00B0; // 176
pub const SMSG_COOLDOWN_EVENT: u16 = 0x0135; // 309
pub const SMSG_CLEAR_COOLDOWN: u16 = 0x01DE; // 478
pub const SMSG_COOLDOWN_CHEAT: u16 = 0x01E1; // 481

/// `SMSG_ITEM_ENCHANT_TIME_UPDATE` (VERIFIED vmangos `Opcodes_1_12_1.h`: 491) — the **only** source
/// of a temporary enchant's remaining time. The item's `ITEM_FIELD_ENCHANTMENT` duration field is
/// never read for it: the reference's tooltip reads a client-side per-slot deadline array
/// (`[obj + slot*4 + 0x324]`) whose only writers are this packet's handler `0x5e4f82` and the item
/// refresh `0x5ebe34` (wow-re `ui/scratch/tooltip-content-law.md` §E3, byte-verified). Body in
/// [`super::items::read_item_enchant_time`]; decision 0920.
pub const SMSG_ITEM_ENCHANT_TIME_UPDATE: u16 = 0x01EB; // 491

/// Pushback (VERIFIED vmangos `Opcodes_1_12_1.h`: 482): sent to the caster when a cast takes damage
/// and the server delays it (`Spell::Delayed`, `Spell.cpp:7472` — a raw `u64` caster guid + `u32`
/// delaytime ms). The cast bar slides its window out by it (`SPELLCAST_DELAYED`); a normal hit never
/// interrupts a cast, it only pushes it back (decision 0256).
pub const SMSG_SPELL_DELAYED: u16 = 0x01E2; // 482

// The self-only channel UI pair (VERIFIED vmangos `Opcodes_1_12_1.h`: 313/314; sent only to the
// casting player — `Spell::SendChannelStart` {spellId u32, duration_ms u32},
// `Player::SendChannelUpdate` {remaining_ms u32, 0 = the channel is over}; decision 0137 phase 1,
// the cast bar's channel half). Bodies in [`super::spells`].
pub const MSG_CHANNEL_START: u16 = 0x0139; // 313
pub const MSG_CHANNEL_UPDATE: u16 = 0x013A; // 314

/// VERIFIED vmangos `Opcodes_1_12_1.h`: 311. Body in [`super::spells::read_update_aura_duration`] —
/// `{u8 slot, u32 remaining_ms}`. **The only aura fact not carried by the descriptor**, and it is
/// self-only: `SpellAuraHolder::UpdateAuraDuration` (`SpellAuras.cpp:7511-7523`) writes it to the
/// aura's *target* (gated on `TYPEID_PLAYER`) and skips permanent auras entirely. So we learn how
/// long our own buffs last, and never how long anyone else's do. It is also sent **before** the
/// `UNIT_FIELD_AURA` delta that names the slot's spell — the server writes the descriptor dirty
/// (flushed at end of tick) and then sends this immediately (decision 0255).
pub const SMSG_UPDATE_AURA_DURATION: u16 = 0x0137; // 311

/// The ding (VERIFIED vmangos `Opcodes_1_12_1.h`: 468) — our own level-up, self-addressed only
/// (`Player::GiveLevel` builds it with no guid and sends straight to the leveling session). Body
/// in [`super::progression::read_level_up_info`] — decision 0304.
pub const SMSG_LEVELUP_INFO: u16 = 0x01D4; // 468
/// A newly explored area — area id + the XP it granted (0 at max level). Body
/// in [`super::progression::read_exploration_xp`] — decision 0828.
pub const SMSG_EXPLORATION_EXPERIENCE: u16 = 0x01F8; // 504 (VERIFIED vmangos `Opcodes_1_12_1.h:505`)
/// The honor payout (VERIFIED vmangos `Opcodes_1_12_1.h`: 652) — the XP-gain line's PvP twin, and
/// the only attributed notice of an honor award: the descriptor's contribution fields move too,
/// but they are running totals. Sent for **dishonorable** kills as well, carrying negative honor.
/// Body in [`super::pvp::read_pvp_credit`] ([`super::PvpCredit`]) — decision 1512.
pub const SMSG_PVP_CREDIT: u16 = 0x028C; // 652

// The combat-log wire (VERIFIED vmangos `Opcodes_1_12_1.h`: 464/587/590/591/592) — decision 0137
// phase 2's floating-combat-text data feed. Bodies in [`super::spells`].
pub const SMSG_ENVIRONMENTALDAMAGELOG: u16 = 0x01FC; // 508
pub const SMSG_LOG_XPGAIN: u16 = 0x01D0; // 464
pub const SMSG_SPELLLOGMISS: u16 = 0x024B; // 587
pub const SMSG_PERIODICAURALOG: u16 = 0x024E; // 590
pub const SMSG_SPELLDAMAGESHIELD: u16 = 0x024F; // 591
pub const SMSG_SPELLNONMELEEDAMAGELOG: u16 = 0x0250; // 592

// The combat log's remaining wire (VERIFIED vmangos `Opcodes_1_12_1.h`; decision 1703 — the
// families 1571 §5 named as "deliberately out because their wire sources are undecoded"). Bodies
// in [`super::combat_log`].
pub const SMSG_ENCHANTMENTLOG: u16 = 0x01D7; // 471
pub const SMSG_PARTYKILLLOG: u16 = 0x01F5; // 501
pub const SMSG_SPELLLOGEXECUTE: u16 = 0x024C; // 588
pub const SMSG_PROCRESIST: u16 = 0x0260; // 608
pub const SMSG_DISPEL_FAILED: u16 = 0x0262; // 610
pub const SMSG_SPELLORDAMAGE_IMMUNE: u16 = 0x0263; // 611
pub const SMSG_SPELLDISPELLOG: u16 = 0x027B; // 635
pub const SMSG_SPELLINSTAKILLLOG: u16 = 0x032F; // 815

// The heal/energize pair (VERIFIED vmangos `Opcodes_1_12_1.h`: 336/337) — the center combat
// text's HEAL / power-gain feed (decision 0578). Bodies in [`super::spells`].
pub const SMSG_SPELLHEALLOG: u16 = 0x0150; // 336
pub const SMSG_SPELLENERGIZELOG: u16 = 0x0151; // 337

// Client → server.
pub const CMSG_CHAR_CREATE: u16 = 0x0036;
pub const CMSG_CHAR_ENUM: u16 = 0x0037;
pub const CMSG_PLAYER_LOGIN: u16 = 0x003D;
pub const CMSG_LOGOUT_REQUEST: u16 = 0x004B;
/// 78 — call off a pending logout (VERIFIED vmangos `Opcodes_1_12_1.h`), empty body; answered by
/// [`SMSG_LOGOUT_CANCEL_ACK`]. The CAMP/QUIT dialog's Cancel (decision 0674).
pub const CMSG_LOGOUT_CANCEL: u16 = 0x004E;
pub const CMSG_NAME_QUERY: u16 = 0x0050; // 80
pub const CMSG_CREATURE_QUERY: u16 = 0x0060; // 96
/// VERIFIED vmangos `Opcodes_1_12_1.h:85`: 82. Body in [`super::client::pet_name_query`]. Answered
/// by [`SMSG_PET_NAME_QUERY_RESPONSE`].
pub const CMSG_PET_NAME_QUERY: u16 = 0x0052; // 82

// The pet action bar's outbound verbs (decision 0982; VERIFIED vmangos `Opcodes_1_12_1.h`:
// 373/372/755/746). Bodies in [`super::pet`].
/// One pet bar press: the slot's own packed word, echoed back with a target guid. The server
/// re-splits it and dispatches on the type byte — command, reaction, or cast.
pub const CMSG_PET_ACTION: u16 = 0x0175; // 373
/// Move/swap a pet bar slot (the drag). One or two `(position, packed)` pairs; the server tells
/// the two forms apart **by body size alone**.
pub const CMSG_PET_SET_ACTION: u16 = 0x0174; // 372
/// Ask for a pet spell's autocast bit to be set to a given value (the right-click).
pub const CMSG_PET_SPELL_AUTOCAST: u16 = 0x02F3; // 755
/// Call the pet off — the Attack button's second press.
pub const CMSG_PET_STOP_ATTACK: u16 = 0x02EA; // 746
/// Take an aura off the **pet** — a pet bar spell click on a spell already running on it. The
/// player's own `CMSG_CANCEL_AURA` cannot serve: its body is a bare spell id with no room to name
/// whose aura to drop.
pub const CMSG_PET_CANCEL_AURA: u16 = 0x026B; // 619

// The pet right-click menu's two outbound verbs (decision 1066; VERIFIED vmangos
// `Opcodes_1_12_1.h`: 374/375). Bodies in [`super::pet`].
/// **Give the pet up permanently** — the right-click menu's Abandon row, and `PetAbandon
/// 0x4be4c0`'s only packet (wow-re `ui/scratch/pet-action-bar-api.md` §11c, `push 0x176`
/// @`0x4bd765`). `HandlePetAbandon` deletes a hunter pet (`PET_SAVE_AS_DELETED`) and merely
/// unsummons anything else (`PET_SAVE_NOT_IN_SLOT`, `PetHandler.cpp:347-374`).
///
/// The menu's **Dismiss** row is NOT this opcode, which is worth stating because the two would be
/// indistinguishable in play: `PetDismiss 0x4be4d0` opens no packet at all and hands the packed
/// word `0x07000003` to the pet bar's own dispatcher, so it leaves as [`CMSG_PET_ACTION`].
pub const CMSG_PET_ABANDON: u16 = 0x0176; // 374
/// Rename the pet — `PetRename 0x4be4e0` → `0x4bd840` (`push 0x177` @`0x4bd8fe`). Gated server-side
/// on `UNIT_FLAG_PET_RENAME` and **one-shot**: the handler clears that bit on success, so the row
/// disappears after the first rename (`PetHandler.cpp:302-345`). Nothing client-side clears it —
/// wow-re's census found no writer of that byte anywhere in `.text`.
///
/// A refused name answers with `SMSG_PET_NAME_INVALID` (0x178), which is **not modelled** and does
/// not need to be: vmangos sends it with an empty body — `SendPetNameInvalid` drops both the reason
/// code and the name with the comment "not read by vanilla client" (`PetHandler.cpp:542-548`) — and
/// the shipped 1.12 `FrameXML` has no event and no string for it. It lands in `ServerPacket::Other`
/// under its own name, which is the whole of what there is to do with it.
pub const CMSG_PET_RENAME: u16 = 0x0177; // 375

/// VERIFIED vmangos `Opcodes_1_12_1.h`: 94 (decision 0236). Body in
/// [`super::gameobject::gameobject_query`] — the ask-once GO template lookup, identical shape to
/// `CMSG_CREATURE_QUERY`.
pub const CMSG_GAMEOBJECT_QUERY: u16 = 0x005E; // 94
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 92. Body in [`super::quest::quest_query`].
pub const CMSG_QUEST_QUERY: u16 = 0x005C; // 92
pub const CMSG_ITEM_QUERY_SINGLE: u16 = 0x0056; // 86
pub const CMSG_USE_ITEM: u16 = 0x00AB; // 171
/// VERIFIED vmangos `Opcodes_1_12_1.h:175`: 172. Body in [`super::items::open_item`] — the bag
/// position of an *openable* item (a clam, an unlocked lockbox, a wrapped gift). The right-click
/// fork `CMSG_USE_ITEM` never takes: the server answers `SMSG_LOOT_RESPONSE` on the **item's own
/// guid** (`HandleOpenItemOpcode` → `SendLoot(item, LOOT_CORPSE)`), so the loot window opens over
/// a thing in your bag rather than a corpse in the world.
pub const CMSG_OPEN_ITEM: u16 = 0x00AC; // 172
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 177. Body in [`super::gameobj_use`] — a full guid, the
/// GameObject to use (decision 0236). Not interchangeable with `CMSG_LOOT`: the server rejects a
/// GameObject guid on `CMSG_LOOT`, so a chest opens its loot through this opcode.
pub const CMSG_GAMEOBJ_USE: u16 = 0x00B1; // 177
/// A GameObject plays a one-shot **custom** animation (VERIFIED vmangos `Opcodes_1_12_1.h`: 179;
/// body in [`super::read_gameobject_custom_anim`] — `u64 guid + u32 animId`, from
/// `GameObject::SendGameObjectCustomAnim`). The client arms GO substate `8 + animId`
/// (Custom0..3, AnimationData ids 153..156), rejecting `animId >= 4` — wow-re
/// `gameobject-anim-arm.md` §"one-shot channel" step 8. The load-bearing sender: the fishing
/// bobber's bite (`animId 0` — the splash; decision 1086).
pub const SMSG_GAMEOBJECT_CUSTOM_ANIM: u16 = 0x00B3; // 179
/// The **other** one-shot GameObject arm channel (VERIFIED vmangos `Opcodes_1_12_1.h`: 533,
/// `WorldObject::SendObjectDeSpawnAnim` → `WorldPackets::Misc::GameObjectDespawnAnim`). Body in
/// [`super::gameobject`]: a bare `u64` guid. The client arms substate **12** — AnimationData id
/// **157 Despawn** — and the object then survives its own `SMSG_DESTROY_OBJECT` for the length of
/// that play (wow-re `gameobject-anim-arm.md` §2c's code table `0x80b0e0[6]` → the `0x8607e4` LUT,
/// and `go-display-sound-events.md` §6d's arm-time pin). The load-bearing sender: a TRAP that has
/// spent its charges — UBRS's Rookery Eggs hatch on this packet (decision 1404).
///
/// It is **not** GameObject-only: `SendObjectDeSpawnAnim` lives on `WorldObject`, so a totem's
/// death and a DynamicObject's expiry send it too. The consumer treats a guid with nothing armable
/// as an ordinary destroy.
pub const SMSG_GAMEOBJECT_DESPAWN_ANIM: u16 = 0x0215; // 533
/// VERIFIED vmangos `Opcodes_1_12_1.h:183`: 180 — and the reference writes this literal
/// (`0x5e2110`, its area-trigger check, builds `0xb4` + the trigger id). Body in
/// [`super::area_trigger`]: the `AreaTrigger.dbc` id the player just walked into.
pub const CMSG_AREATRIGGER: u16 = 0x00B4; // 180
/// VERIFIED vmangos `Opcodes_1_12_1.h:697`: 696. Body in
/// [`super::area_trigger::read_area_trigger_message`] — why a trigger refused (level, ghost form,
/// battleground faction). The reference displays it through the same system-message sink as
/// `SMSG_NOTIFICATION` (`0x4945b0`).
pub const SMSG_AREA_TRIGGER_MESSAGE: u16 = 0x02B8; // 696
pub const CMSG_MESSAGECHAT: u16 = 0x0095;
pub const SMSG_MESSAGECHAT: u16 = 0x0096;
// The channel family (VERIFIED vmangos `Server/Protocol/Opcodes_1_12_1.h:154-171`, decimal
// 151-168): join/leave/list + the whole moderation set. Every CMSG here is `cstring channelName [+
// cstring playerName]` (bodies in [`super::channel`]/[`super::client`]); no channel id rides the
// 1.12 wire (that's a TBC+ addition). `SMSG_CHANNEL_NOTIFY`/`_LIST` layouts in [`super::channel`].
pub const CMSG_JOIN_CHANNEL: u16 = 0x0097; // 151
pub const CMSG_LEAVE_CHANNEL: u16 = 0x0098; // 152
pub const SMSG_CHANNEL_NOTIFY: u16 = 0x0099; // 153
pub const CMSG_CHANNEL_LIST: u16 = 0x009A; // 154
pub const SMSG_CHANNEL_LIST: u16 = 0x009B; // 155
pub const CMSG_CHANNEL_PASSWORD: u16 = 0x009C; // 156
pub const CMSG_CHANNEL_SET_OWNER: u16 = 0x009D; // 157
pub const CMSG_CHANNEL_OWNER: u16 = 0x009E; // 158
pub const CMSG_CHANNEL_MODERATOR: u16 = 0x009F; // 159
pub const CMSG_CHANNEL_UNMODERATOR: u16 = 0x00A0; // 160
pub const CMSG_CHANNEL_MUTE: u16 = 0x00A1; // 161
pub const CMSG_CHANNEL_UNMUTE: u16 = 0x00A2; // 162
pub const CMSG_CHANNEL_INVITE: u16 = 0x00A3; // 163
pub const CMSG_CHANNEL_KICK: u16 = 0x00A4; // 164
pub const CMSG_CHANNEL_BAN: u16 = 0x00A5; // 165
pub const CMSG_CHANNEL_UNBAN: u16 = 0x00A6; // 166
pub const CMSG_CHANNEL_ANNOUNCEMENTS: u16 = 0x00A7; // 167
pub const CMSG_CHANNEL_MODERATE: u16 = 0x00A8; // 168

// The remaining decision-0288 "wire completeness" opcodes — small, single-purpose chat-adjacent
// packets that don't belong to any of the families above.
/// The ignore-list self-notice ("Name is now ignoring you" — VERIFIED vmangos `Opcodes_1_12_1.h`:
/// 549). Body in [`super::client::chat_ignored`] — a raw `u64` guid
/// (`WorldPackets::Misc::ChatIgnored::ReadFromWorldPacket`, `Server/Packets/Misc.cpp:127-130`).
pub const CMSG_CHAT_IGNORED: u16 = 0x0225; // 549
/// A whisper target wasn't found online (VERIFIED vmangos `Opcodes_1_12_1.h`: 681). Body:
/// [`super::chat::read_chat_player_not_found`] (cstring name, `Server/Packets/Chat.cpp:26-29`).
pub const SMSG_CHAT_PLAYER_NOT_FOUND: u16 = 0x02A9; // 681
/// A cross-faction whisper was refused (VERIFIED vmangos `Opcodes_1_12_1.h`: 537); empty body
/// (`WorldPackets::Chat::ChatWrongFaction::AppendBodyTo`, `Server/Packets/Chat.cpp:16-18`).
pub const SMSG_CHAT_WRONG_FACTION: u16 = 0x0219; // 537
/// The fishing channel ended with nothing on the hook (VERIFIED vmangos `Opcodes_1_12_1.h`: 456;
/// empty body — `GameObject::Update`'s bobber-expiry arm and `Use(FISHINGNODE)`'s
/// clicked-too-early arm both send size 0). The red `ERR_FISH_NOT_HOOKED` toast — "No fish are
/// hooked." (decision 1086).
pub const SMSG_FISH_NOT_HOOKED: u16 = 0x01C8; // 456
/// The hooked fish got away — the fishing-skill roll failed on the click (VERIFIED vmangos
/// `Opcodes_1_12_1.h`: 457; empty body — `GameObject::Use`'s FISHINGNODE failure arm). The red
/// `ERR_FISH_ESCAPED` toast — "Your fish got away!" (decision 1086).
pub const SMSG_FISH_ESCAPED: u16 = 0x01C9; // 457
/// A server notice the real client flashes in the red UIErrorsFrame (VERIFIED vmangos
/// `Opcodes_1_12_1.h`: 459): one cstring (`WorldSession::SendNotification`,
/// `Server/WorldSession.cpp:900-915`) — "You do not know that language", trade refusals, and kin.
pub const SMSG_NOTIFICATION: u16 = 0x01CB; // 459

// The **world broadcasts** — sent to everyone, or to everyone in a zone, rather than to one player
// about their own doing. Bodies and the client's own handlers: [`super::broadcast`].
/// An area is under attack by enemy players (VERIFIED vmangos `Opcodes_1_12_1.h`: 596): one `u32`
/// `AreaTable.dbc` id (`WorldPackets::Misc::ZoneUnderAttack::AppendBodyTo`,
/// `Server/Packets/Misc.cpp:451-454`). The client (`0x49dcc0`) formats FrameXML's
/// `ZONE_UNDER_ATTACK` global string with the area's name and delivers it as `CHAT_MSG_CHANNEL` on
/// the joined defense channels — **not** as a system line.
pub const SMSG_ZONE_UNDER_ATTACK: u16 = 0x0254; // 596
/// A shutdown/restart countdown or an operator's broadcast (VERIFIED vmangos `Opcodes_1_12_1.h`:
/// 657): `u32 messageType` + one cstring (`WorldPackets::Misc::ServerMessage::AppendBodyTo`,
/// `Server/Packets/Misc.cpp:341-345`; layout in [`super::broadcast::read_server_message`]). The
/// type indexes `ServerMessages.dbc`, whose text is the format string the packet's text fills; the
/// client (`0x49df80`) shows the result as `CHAT_MSG_SYSTEM`.
pub const SMSG_SERVER_MESSAGE: u16 = 0x0291; // 657
/// A trial account hit its whisper cap (VERIFIED vmangos `Opcodes_1_12_1.h`: 765); **empty body**
/// (`WorldPackets::Chat::ChatRestricted::AppendBodyTo`, `Server/Packets/Chat.cpp:21-23`), and the
/// client's arm (`0x5e4a09`) reads none — it is three instructions, `DisplayError(0x1c3)`.
pub const SMSG_CHAT_RESTRICTED: u16 = 0x02FD; // 765
/// A world-defense broadcast — the Eastern Plaguelands tower captures (VERIFIED vmangos
/// `Opcodes_1_12_1.h`: 827): `u32 zoneId`, `u32 length`, then the text (`Map::SendDefenseMessage`,
/// `src/game/Maps/Map.cpp:1868-1884`, built raw rather than through a packet class and
/// **1.12-only**). Same destination as [`SMSG_ZONE_UNDER_ATTACK`] and the same handler shape
/// (`0x49de30`), but the text rides the wire instead of a global string.
pub const SMSG_DEFENSE_MESSAGE: u16 = 0x033B; // 827

/// `/played` (VERIFIED vmangos `Opcodes_1_12_1.h`: 460/461): empty CMSG body
/// (`NullClientPacket`), SMSG body `u32 total + u32 level` seconds
/// (`WorldPackets::Misc::PlayedTime::AppendBodyTo`, `Server/Packets/Misc.cpp:278-282`; layout in
/// [`super::chat::read_played_time`]).
pub const CMSG_PLAYED_TIME: u16 = 0x01CC; // 460
pub const SMSG_PLAYED_TIME: u16 = 0x01CD; // 461
/// The server's wall clock (VERIFIED vmangos `Opcodes_1_12_1.h`: 462/463): empty CMSG body
/// (`NullClientPacket`), SMSG body one `u32` = the server's `time(nullptr)`, i.e. **unix-epoch
/// seconds** (`WorldSession::SendQueryTimeResponse`, `Handlers/QueryHandler.cpp:418-423`).
///
/// This is the epoch the quest-log slot's timer field is expressed in — a timed quest's deadline
/// is written as `time(nullptr) + limitTime` (`Player::AddQuest`), an absolute stamp with no
/// relative form anywhere on the wire. So a countdown needs the *server's* now, not ours: that is
/// what this pair is for, and why benilla sends it (decision 1150). The CMSG body is
/// [`super::query_time`]; the response is decoded inline beside `SMSG_LOGIN_SETTIMESPEED`, the
/// other clock packet.
pub const CMSG_QUERY_TIME: u16 = 0x01CE; // 462
pub const SMSG_QUERY_TIME_RESPONSE: u16 = 0x01CF; // 463
/// `/random` (VERIFIED vmangos `Opcodes_1_12_1.h`: 507) — same opcode both directions, different
/// bodies: client sends `u32 min + u32 max` ([`super::client::random_roll`],
/// `WorldPackets::Group::RandomRoll::ReadFromWorldPacket`, `Server/Packets/Group.cpp:39-43`); the
/// server broadcasts `u32 min + u32 max + u32 roll + u64 guid`
/// (`WorldSession::HandleRandomRollOpcode`, `Handlers/GroupHandler.cpp:394-422`; layout in
/// [`super::chat::read_random_roll`]).
pub const MSG_RANDOM_ROLL: u16 = 0x01FB; // 507

/// VERIFIED vmangos `Opcodes_1_12_1.h`: 257. Body in [`super::stand_state_change`].
pub const CMSG_STANDSTATECHANGE: u16 = 0x0101; // 257
pub const CMSG_CAST_SPELL: u16 = 0x012E; // 302
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 310. Body in [`super::spells::cancel_aura`] — a lone `u32`
/// spell id (`Server/Packets/Spell.h:55-62`): the server cancels **by spell, never by slot**, and
/// its handler (`SpellHandler.cpp:333-405`) refuses passives, `SPELL_ATTR_NO_AURA_CANCEL` spells,
/// and debuffs. wow-re independently records the real client's sender (`Spell_C::CancelAura`
/// `0x6e7040`, `push 0x136`). Decision 0255.
/// Body: one `u32` spell id (vmangos `HandleCancelCastOpcode`). The wand-only auto-repeat
/// handoff sends it for the cached wand Shoot before the local cancel (`0x6095b8`, wow-re
/// `nocked-ammo-cancel.md` §Q-B-5).
pub const CMSG_CANCEL_CAST: u16 = 0x012F; // 303
pub const CMSG_CANCEL_AURA: u16 = 0x0136; // 310
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 315. Body: one `u32` spell id — the server ignores it
/// (`Server/Packets/Spell.h:69` "not used by server"; `HandleCancelChanneling` interrupts the
/// current channel unconditionally) but the real client writes it, so ours does too. Sent by the
/// cast bar's local self-cancel (`benilla::ui_cast`) when movement/Esc ends our own channel.
pub const CMSG_CANCEL_CHANNELLING: u16 = 0x013B; // 315
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 276. Body: a raw 8-byte guid ([`super::full_guid`] —
/// `WorldPackets::Misc::Inspect`). The server sets our selection to it, refuses beyond
/// `INSPECT_DISTANCE` (10.0y) or on `IsValidAttackTarget`, and otherwise replies `SMSG_INSPECT`
/// (277) carrying **only the echoed guid** (`MiscHandler.cpp:943-960`). We deliberately do not
/// parse that reply: it carries no data, the real client's inspect frame paints from the already-
/// streamed PUBLIC `PLAYER_VISIBLE_ITEM_*` fields without waiting on it, and no FrameXML handler
/// registers an inspect event. Decision 0631.
pub const CMSG_INSPECT: u16 = 0x0114; // 276
/// 726 (VERIFIED vmangos `Opcodes_1_12_1.h`) — the inspect window's **Honor tab**, and an `MSG_`:
/// the one opcode number carries both directions. Our request is a raw 8-byte guid
/// ([`super::inspect_honor_stats`]); the server's reply, on the same number, is the 50-byte
/// [`super::InspectHonorStats`] body. Nothing needs to disambiguate the two shapes — direction
/// does: [`super::parse_server`] only ever sees inbound bodies, so a 0x2D6 there is always the
/// reply, and our outbound body never passes through it.
///
/// Same three silent refusals as [`CMSG_INSPECT`] (`MiscHandler.cpp:962-972`), but *unlike* it
/// this handler does not set our selection. Decision 1512.
pub const MSG_INSPECT_HONOR_STATS: u16 = 0x02D6; // 726
pub const CMSG_SET_SELECTION: u16 = 0x013D; // 317
pub const CMSG_ATTACKSWING: u16 = 0x0141; // 321
pub const CMSG_ATTACKSTOP: u16 = 0x0142; // 322
/// VERIFIED vmangos `Opcodes_1_12_1.h`: 480. Body in [`super::pose::set_sheathed`].
pub const CMSG_SETSHEATHED: u16 = 0x01E0; // 480
pub const CMSG_AUTH_SESSION: u16 = 0x01ED;
pub const CMSG_SET_ACTIVE_MOVER: u16 = 0x026A;
/// The other half of the mover handshake (VERIFIED vmangos `Opcodes_1_12_1.h`: 721,
/// `MovementHandler.cpp:886-965`). Body: the full u64 guid of the mover we are *giving up*, then a
/// whole `MovementInfo`. It clears the server's `m_clientMoverGuid` and re-broadcasts a stop under
/// the old guid, so skipping it strands observers on that mover's last relayed pose.
pub const CMSG_MOVE_NOT_ACTIVE_MOVER: u16 = 0x02D1; // 721
/// The far-sight **toggle vote** (VERIFIED vmangos `Opcodes_1_12_1.h`: 634,
/// `MiscHandler.cpp:1138-1155`). Body is a single `u8`: `1` = look through the object, `0` = look
/// through my own body again.
///
/// **The client never names the object.** The server resolves it from `PLAYER_FARSIGHT`, which
/// only the server writes — so this is a vote on a view it already chose, not a request. Both
/// branches pass `update_far_sight_field = false`, so **neither touches the field**: it keeps
/// naming the object while the view toggles under it.
///
/// **Sending `0` is destructive and must be deliberate.** It moves the server's *visibility*
/// source back to our body while `PLAYER_FARSIGHT` still names the object — the world around that
/// object stops streaming while the camera is still anchored to it. Sending nothing is the safe
/// default: the server tracks no reply, and it has already attached the viewpoint before we could
/// answer. `1` is a no-op in every normal flow (`Camera::SetView` early-returns on an unchanged
/// source); it exists for the buff-click toggle, whose only shipped user is Sentry Totem.
pub const CMSG_FAR_SIGHT: u16 = 0x027A; // 634
/// Empty body. The real client sends it from exactly ONE site — inside the local auto-repeat
/// cancel `0x6ea080` (`0x6ea0c6`) — so it rides along with *every* cancel trigger (wow-re
/// `nocked-ammo-cancel.md`). vmangos `HandleCancelAutoRepeatSpellOpcode` interrupts the held
/// auto-repeat spell (idempotent when the server already cancelled it first).
pub const CMSG_CANCEL_AUTO_REPEAT_SPELL: u16 = 0x026D; // 621

// The keepalive pair (VERIFIED twice: vmangos `Opcodes_1_12_1.h` 476/477 AND wow-re net W1 —
// `SendPing 0x537e10` pushes opcode 0x1dc, `HandlePong 0x537d60` matches 0x1dd). The real client
// pings every 30 000 ms (wow-re `0x537ff0`, the connection drain's cadence check); vmangos kicks a
// player socket that pings *faster* than 27 s apart more than `MaxOverspeedPings` (default 2) times
// (`WorldSocket::_HandlePing`). Bodies: CMSG `{u32 sequence, u32 lastRtt}`; SMSG echoes `{u32
// sequence}`. vmangos handles the ping inline on the socket (pre-session-queue) and stores the
// latency field for `.server info`.
pub const CMSG_PING: u16 = 0x01DC; // 476
pub const SMSG_PONG: u16 = 0x01DD; // 477

// The force-speed-change family (VERIFIED vmangos `Opcodes_1_12_1.h`: 226-231 + 730-735): the
// server tells the CONTROLLER of a unit its speed changed (an aura slow, a mount, GM `.modify
// speed`) as `[packed mover guid][u32 movementCounter][f32 newSpeed]` (flat yd/s —
// `MovementPacketSender::SendSpeedChangeToController`, 5875 = the `> 1_9_4` branch), and the client
// MUST answer the matching ACK: `[u64 FULL guid][u32 movementCounter][MovementInfo][f32 speed]`
// (`MoveSpeedAck::ReadFromWorldPacket` — plain `ObjectGuid` extraction reads a full 8 bytes,
// `ObjectGuid.cpp:180`; the echoed speed must match the sent one ±0.01 and the counter must match
// a pending change, `Unit::FindPendingMovementSpeedChange`). Unacked, the server force-resolves
// after `Movement.PendingAckResponseTime` (4 s default) and flags the anticheat's
// `OnFailedToAckChange` — every speed change desyncs for 4 s and looks like cheating.
pub const SMSG_FORCE_RUN_SPEED_CHANGE: u16 = 0x00E2; // 226
pub const CMSG_FORCE_RUN_SPEED_CHANGE_ACK: u16 = 0x00E3; // 227
pub const SMSG_FORCE_RUN_BACK_SPEED_CHANGE: u16 = 0x00E4; // 228
pub const CMSG_FORCE_RUN_BACK_SPEED_CHANGE_ACK: u16 = 0x00E5; // 229
pub const SMSG_FORCE_SWIM_SPEED_CHANGE: u16 = 0x00E6; // 230
pub const CMSG_FORCE_SWIM_SPEED_CHANGE_ACK: u16 = 0x00E7; // 231
pub const SMSG_FORCE_WALK_SPEED_CHANGE: u16 = 0x02DA; // 730
pub const CMSG_FORCE_WALK_SPEED_CHANGE_ACK: u16 = 0x02DB; // 731
pub const SMSG_FORCE_SWIM_BACK_SPEED_CHANGE: u16 = 0x02DC; // 732
pub const CMSG_FORCE_SWIM_BACK_SPEED_CHANGE_ACK: u16 = 0x02DD; // 733
pub const SMSG_FORCE_TURN_RATE_CHANGE: u16 = 0x02DE; // 734
pub const CMSG_FORCE_TURN_RATE_CHANGE_ACK: u16 = 0x02DF; // 735

// The OBSERVER speed-change legs (VERIFIED vmangos `MovementPacketSender`, decision 0441): a
// speed change on a unit we don't control reaches us one of two ways, neither acked. A
// server-controlled mover (creature), or a player mid-spline, broadcasts
// `SMSG_SPLINE_SET_*_SPEED` = `[packed guid][f32 speed]` (`SendSpeedChangeToAll` / the
// non-finalized `SendSpeedChangeToObservers` branch; 766-771). A freely-moving player's change —
// the common case: another player mounting up — broadcasts `MSG_MOVE_SET_*_SPEED` =
// `[packed guid][MovementInfo][f32 speed]` (the finalized-spline branch; 205-216, gaps real),
// which also carries a fresh authoritative pose.
pub const SMSG_SPLINE_SET_RUN_SPEED: u16 = 0x02FE; // 766
pub const SMSG_SPLINE_SET_RUN_BACK_SPEED: u16 = 0x02FF; // 767
pub const SMSG_SPLINE_SET_SWIM_SPEED: u16 = 0x0300; // 768
pub const SMSG_SPLINE_SET_WALK_SPEED: u16 = 0x0301; // 769
pub const SMSG_SPLINE_SET_SWIM_BACK_SPEED: u16 = 0x0302; // 770
pub const SMSG_SPLINE_SET_TURN_RATE: u16 = 0x0303; // 771

// **The OBSERVER movement-mode family** (decision 1780) — the spline-move twelve. Where the
// `SMSG_FORCE_*`/`SMSG_MOVE_*` family above hands the *controlling client* a mode to ack, this one
// tells everyone else that some unit's mode changed, and it is a different wire in three ways:
// the body is a **bare packed guid** (no counter, no `MovementInfo`), there is **no ack**, and the
// target is **any unit**, not our mover.
//
// VERIFIED both ends. Server: vmangos `MovementPacketSender::SendMovementFlagChangeToAll` /
// `SendToggleRunWalkToAll` (`MovementPacketSender.cpp:399-462`) write `data << unit->GetPackGUID()`
// and stop — the `+ 4` / `9` in the `WorldPacket` ctor is a capacity hint, not content. Client:
// all twelve register the single handler `0x603c80`, which reads one packed guid (`0x642ed0`),
// resolves it with `ClntObjMgrObjectPtr(TYPEMASK_UNIT)` and calls the dispatcher `0x601420`
// (`lea eax,[edi-0x304]; cmp eax,0x16; ja` — so `0x304..=0x31A`, and `SMSG_SPLINE_MOVE_ROOT`'s
// out-of-band `0x31A` is deliberately the range's top). An unresolvable guid is dropped silently.
//
// The dispatcher's twelve arms, in registration order (`0x603775`-`0x603830`), are a 1:1 map onto
// the twelve opcodes; six of the arms are independently VERIFIED in wow-re's collision ledger:
//
// | opcode | arm | effect on `CMovement+0x40` |
// |---|---|---|
// | `0x31A` ROOT | `0x619a10` | `SetRoot 0x7c7340` — sets `0x1000`, and **wipes `0xffe07f00`'s complement once at apply** |
// | `0x304` UNROOT | `0x619a40` | `ClearRoot 0x7c7370(1)` — clears `0x1000` |
// | `0x305`/`0x306` | `0x61a4e0` | `SetFeatherFall 0x7c72e0` — `0x20000000` |
// | `0x307`/`0x308` | `0x61a620` | `SetHover 0x7c7310` — `0x40000000` |
// | `0x309`/`0x30A` | `0x61a3d0` | `SetWaterWalk 0x7c7280` — `0x10000000` |
// | `0x30B`/`0x30C` | `0x61a130`/`0x61a160` | `SetSwim 0x7c6e50`/`0x7c6e80` — `0x200000` |
// | `0x30D`/`0x30E` | `0x617e80` | `SetRunMode 0x7c71c0` — `0x100`, **inverted** (see below) |
//
// After any arm the dispatcher re-runs the unit's animation selector (`0x6014ec push edi; call
// 0x60e480` then `push -1; call 0x5fd9e0`), which is why this family is *visual*: the mode change
// re-picks the gait on the spot rather than waiting for the next pose.
//
// (wow-re `collision/scratch/moveflag-family.md` §§2-5, `collision/scratch/walk-mode-law.md` §5,
// `collision/scratch/remote-swim-decision.md` §1.4, `collision/ledger.tsv` rows for each arm.)
pub const SMSG_SPLINE_MOVE_UNROOT: u16 = 0x0304; // 772
pub const SMSG_SPLINE_MOVE_FEATHER_FALL: u16 = 0x0305; // 773
pub const SMSG_SPLINE_MOVE_NORMAL_FALL: u16 = 0x0306; // 774
pub const SMSG_SPLINE_MOVE_SET_HOVER: u16 = 0x0307; // 775
pub const SMSG_SPLINE_MOVE_UNSET_HOVER: u16 = 0x0308; // 776
pub const SMSG_SPLINE_MOVE_WATER_WALK: u16 = 0x0309; // 777
pub const SMSG_SPLINE_MOVE_LAND_WALK: u16 = 0x030A; // 778
pub const SMSG_SPLINE_MOVE_START_SWIM: u16 = 0x030B; // 779
pub const SMSG_SPLINE_MOVE_STOP_SWIM: u16 = 0x030C; // 780
                                                    // **The run/walk pair is inverted against the flag bit, and that is the reference's own sign.**
                                                    // `0x617e80` passes the opcode's bool straight to `CMovement::SetRunMode 0x7c71c0`, whose argument
                                                    // is *run* — so `SET_RUN_MODE` CLEARS `MOVEFLAG_WALK_MODE` (`0x100`) and `SET_WALK_MODE` sets it.
                                                    // Modelled as one [`super::movement::SplineMode::WalkMode`] whose `apply` is the flag's direction,
                                                    // so every arm of the family means the same thing by "apply": set this bit.
pub const SMSG_SPLINE_MOVE_SET_RUN_MODE: u16 = 0x030D; // 781
pub const SMSG_SPLINE_MOVE_SET_WALK_MODE: u16 = 0x030E; // 782
pub const SMSG_SPLINE_MOVE_ROOT: u16 = 0x031A; // 794
pub const MSG_MOVE_SET_RUN_SPEED: u16 = 0x00CD; // 205
pub const MSG_MOVE_SET_RUN_BACK_SPEED: u16 = 0x00CF; // 207
pub const MSG_MOVE_SET_WALK_SPEED: u16 = 0x00D1; // 209
pub const MSG_MOVE_SET_SWIM_SPEED: u16 = 0x00D3; // 211
pub const MSG_MOVE_SET_SWIM_BACK_SPEED: u16 = 0x00D5; // 213
pub const MSG_MOVE_SET_TURN_RATE: u16 = 0x00D8; // 216

// Mount feedback (VERIFIED vmangos `Opcodes_1_12_1.h` 366-367, `Player::SendMountResult` /
// `SendDismountResult` — one u32 result code; decision 0441). The success codes
// (`MOUNTRESULT_OK` = 10 / `DISMOUNTRESULT_OK` = 3) are silent in the reference; the failure
// codes map to red error lines (a P2 trimming — decoded and logged for now).
pub const SMSG_MOUNTRESULT: u16 = 0x016E; // 366
pub const SMSG_DISMOUNTRESULT: u16 = 0x016F; // 367

// The mounted space-bar flourish (VERIFIED vmangos `HandleMountSpecialAnimOpcode`,
// `MovementHandler.cpp:967`): the CMSG is an EMPTY body; the server rebroadcasts
// `SMSG_MOUNTSPECIAL_ANIM` — one raw u64 guid — to everyone in range, INCLUDING the sender
// on vmangos's non-broadcaster delivery path (its `self=false` only gates cheat-logging;
// live-verified by the double-flourish probe 2026-07-17). The app plays its own flourish
// locally at send time and self-suppresses the echo; receivers play MountSpecial(94) on
// that unit's mount (decision 0441 P2).
pub const CMSG_MOUNTSPECIAL_ANIM: u16 = 0x0171; // 369
pub const SMSG_MOUNTSPECIAL_ANIM: u16 = 0x0172; // 370

// Movement (`MSG_MOVE_*`, both directions). The player-movement relay set: the server rebroadcasts
// each opcode bound to its `HandleMovementOpcodes` (VERIFIED vmangos `Opcodes.cpp`) to nearby clients
// as `[packed mover guid][MovementInfo]`, same opcode. We *send* the locomotion subset to drive our
// own avatar and *receive* the whole set as other players' movement (see [`is_movement_relay`]). The
// two acks (TELEPORT/WORLDPORT) ride the same family but carry different bodies, handled separately.
pub const MSG_MOVE_START_FORWARD: u16 = 0x00B5; // 181
pub const MSG_MOVE_START_BACKWARD: u16 = 0x00B6; // 182
pub const MSG_MOVE_STOP: u16 = 0x00B7; // 183
pub const MSG_MOVE_START_STRAFE_LEFT: u16 = 0x00B8; // 184
pub const MSG_MOVE_START_STRAFE_RIGHT: u16 = 0x00B9; // 185
pub const MSG_MOVE_STOP_STRAFE: u16 = 0x00BA; // 186
pub const MSG_MOVE_JUMP: u16 = 0x00BB; // 187
pub const MSG_MOVE_START_TURN_LEFT: u16 = 0x00BC; // 188
pub const MSG_MOVE_START_TURN_RIGHT: u16 = 0x00BD; // 189
pub const MSG_MOVE_STOP_TURN: u16 = 0x00BE; // 190
pub const MSG_MOVE_START_PITCH_UP: u16 = 0x00BF; // 191
pub const MSG_MOVE_START_PITCH_DOWN: u16 = 0x00C0; // 192
pub const MSG_MOVE_STOP_PITCH: u16 = 0x00C1; // 193
pub const MSG_MOVE_SET_RUN_MODE: u16 = 0x00C2; // 194
pub const MSG_MOVE_SET_WALK_MODE: u16 = 0x00C3; // 195
pub const MSG_MOVE_TELEPORT_ACK: u16 = 0x00C7; // 199
pub const MSG_MOVE_FALL_LAND: u16 = 0x00C9; // 201
pub const MSG_MOVE_START_SWIM: u16 = 0x00CA; // 202
pub const MSG_MOVE_STOP_SWIM: u16 = 0x00CB; // 203
pub const MSG_MOVE_WORLDPORT_ACK: u16 = 0x00DC; // 220
pub const MSG_MOVE_SET_FACING: u16 = 0x00DA; // 218
pub const MSG_MOVE_SET_PITCH: u16 = 0x00DB; // 219
pub const MSG_MOVE_HEARTBEAT: u16 = 0x00EE; // 238

// The gossip/vendor interaction set — "right-click a friendly NPC → dialog/vendor" (VERIFIED
// vmangos `Opcodes_1_12_1.h`: 379-384 gossip/NPC-text, 414-421 vendor). Bodies in
// [`super::gossip`] (gossip + NPC text) and [`super::vendor`] (vendor list/buy/sell).
pub const CMSG_GOSSIP_HELLO: u16 = 0x017B; // 379
pub const CMSG_GOSSIP_SELECT_OPTION: u16 = 0x017C; // 380
pub const SMSG_GOSSIP_MESSAGE: u16 = 0x017D; // 381
pub const SMSG_GOSSIP_COMPLETE: u16 = 0x017E; // 382
pub const CMSG_NPC_TEXT_QUERY: u16 = 0x017F; // 383
pub const SMSG_NPC_TEXT_UPDATE: u16 = 0x0180; // 384

// The guard's directions marker. Volunteered by a gossip option carrying an `action_poi_id`
// (vmangos `Player::OnGossipSelect` → `PlayerMenu::SendPointOfInterest`, `GossipDef.cpp:253`), so
// it answers no request of ours — a gossip-family SMSG with no CMSG beside it, which is why it
// sits out at 548 instead of inside the 379-384 block. Body in [`super::gossip`].
pub const SMSG_GOSSIP_POI: u16 = 0x0224; // 548

// The questgiver panel set — "accept/turn-in a quest at an NPC" (VERIFIED vmangos
// `Opcodes_1_12_1.h`: 386-402). Bodies + SMSG layouts in [`super::quest`] (decision 0088).
pub const CMSG_QUESTGIVER_STATUS_QUERY: u16 = 0x0182; // 386
pub const SMSG_QUESTGIVER_STATUS: u16 = 0x0183; // 387
pub const CMSG_QUESTGIVER_HELLO: u16 = 0x0184; // 388
pub const SMSG_QUESTGIVER_QUEST_LIST: u16 = 0x0185; // 389
pub const CMSG_QUESTGIVER_QUERY_QUEST: u16 = 0x0186; // 390
pub const SMSG_QUESTGIVER_QUEST_DETAILS: u16 = 0x0188; // 392
pub const CMSG_QUESTGIVER_ACCEPT_QUEST: u16 = 0x0189; // 393
pub const CMSG_QUESTGIVER_COMPLETE_QUEST: u16 = 0x018A; // 394
pub const SMSG_QUESTGIVER_REQUEST_ITEMS: u16 = 0x018B; // 395
pub const CMSG_QUESTGIVER_REQUEST_REWARD: u16 = 0x018C; // 396
pub const SMSG_QUESTGIVER_OFFER_REWARD: u16 = 0x018D; // 397
pub const CMSG_QUESTGIVER_CHOOSE_REWARD: u16 = 0x018E; // 398
pub const SMSG_QUESTGIVER_QUEST_INVALID: u16 = 0x018F; // 399
pub const SMSG_QUESTGIVER_QUEST_COMPLETE: u16 = 0x0191; // 401
pub const SMSG_QUESTGIVER_QUEST_FAILED: u16 = 0x0192; // 402

// The quest-log wire (VERIFIED vmangos `Opcodes_1_12_1.h`: 403-410) — swap/abandon a logged
// quest, the full-log refusal, and the log's live progress/state pushes. Bodies + the
// `SMSG_QUEST_QUERY_RESPONSE` full template in [`super::quest`].
pub const CMSG_QUESTLOG_SWAP_QUEST: u16 = 0x0193; // 403
pub const CMSG_QUESTLOG_REMOVE_QUEST: u16 = 0x0194; // 404
pub const SMSG_QUESTLOG_FULL: u16 = 0x0195; // 405
pub const SMSG_QUESTUPDATE_FAILED: u16 = 0x0196; // 406
pub const SMSG_QUESTUPDATE_FAILEDTIMER: u16 = 0x0197; // 407
pub const SMSG_QUESTUPDATE_COMPLETE: u16 = 0x0198; // 408
pub const SMSG_QUESTUPDATE_ADD_KILL: u16 = 0x0199; // 409
pub const SMSG_QUESTUPDATE_ADD_ITEM: u16 = 0x019A; // 410

// The party quest-**share** set (VERIFIED vmangos `Opcodes_1_12_1.h`: 411-413 + 630) — decision
// 1733. `MSG_QUEST_PUSH_RESULT` is the one bidirectional opcode of the quest family: the receiver
// sends its own verdict up, the server relays every verdict (its own and the receiver's) back down
// to the SHARER. Bodies in [`super::quest`]'s `share` submodule.
pub const CMSG_QUEST_CONFIRM_ACCEPT: u16 = 0x019B; // 411
pub const SMSG_QUEST_CONFIRM_ACCEPT: u16 = 0x019C; // 412
pub const CMSG_PUSHQUESTTOPARTY: u16 = 0x019D; // 413
pub const MSG_QUEST_PUSH_RESULT: u16 = 0x0276; // 630

pub const CMSG_LIST_INVENTORY: u16 = 0x019E; // 414
pub const SMSG_LIST_INVENTORY: u16 = 0x019F; // 415
pub const CMSG_SELL_ITEM: u16 = 0x01A0; // 416
pub const SMSG_SELL_ITEM: u16 = 0x01A1; // 417
pub const CMSG_BUY_ITEM: u16 = 0x01A2; // 418
/// Buy into a **specific** bag slot. This is what a vendor row dropped out of the merchant
/// cursor sends (`PickupMerchantItem` mode 5 → the container/doll drop), as against
/// `CMSG_BUY_ITEM`'s auto-place, which is what clicking the row sends. Both ship (decision 1797).
pub const CMSG_BUY_ITEM_IN_SLOT: u16 = 0x01A3; // 419
pub const SMSG_BUY_ITEM: u16 = 0x01A4; // 420
pub const SMSG_BUY_FAILED: u16 = 0x01A5; // 421
/// Buy a sold item back (VERIFIED wow-re `BuybackItem 0x4fb950`'s PutUint32 opcode immediate,
/// ui/scratch/buyback-data-path.md + vmangos `Opcodes_1_12_1.h` 656 — NOT in the 0x19E vendor run).
pub const CMSG_BUYBACK_ITEM: u16 = 0x0290; // 656
/// Repair one item (guid) or everything (guid 0) at a repair-capable vendor (VERIFIED wow-re
/// ui/scratch/repair-machinery.md: `68 a8 02 00 00` at all 4 client send sites; vmangos 680).
pub const CMSG_REPAIR_ITEM: u16 = 0x02A8; // 680

// The taxi/flight-master set (VERIFIED vmangos `Opcodes_1_12_1.h`: 425-431 the status/menu/
// activate family, 786 the express-route activate — added later in the opcode table but the same
// wire family, kept here rather than off with the vendor-era outliers). Guid fields on every one
// of these (both directions) are a PLAIN u64, never a packed guid (vmangos `ObjectGuid::operator
// >>/<<`, `ObjectGuid.cpp:174-186` — `PackedGuid` is a distinct type these packets never use).
// Bodies in [`super::taxi`] (decision 0484).
pub const SMSG_SHOWTAXINODES: u16 = 0x01A9; // 425
pub const CMSG_TAXINODE_STATUS_QUERY: u16 = 0x01AA; // 426
pub const SMSG_TAXINODE_STATUS: u16 = 0x01AB; // 427
pub const CMSG_TAXIQUERYAVAILABLENODES: u16 = 0x01AC; // 428
pub const CMSG_ACTIVATETAXI: u16 = 0x01AD; // 429
pub const SMSG_ACTIVATETAXIREPLY: u16 = 0x01AE; // 430
pub const SMSG_NEW_TAXI_PATH: u16 = 0x01AF; // 431
/// `> CLIENT_BUILD_1_9_4` — active for 5875. Multi-hop route activate (guid, totalcost, node
/// list); a single hop still goes through plain `CMSG_ACTIVATETAXI` above (vmangos accepts both).
pub const CMSG_ACTIVATETAXIEXPRESS: u16 = 0x0312; // 786

// The class/profession trainer set (VERIFIED vmangos `Opcodes_1_12_1.h`: 432-436) — one window for
// both, reached through the gossip trainer option (`GOSSIP_OPTION_TRAINER`), not a trainer-specific
// open verb. `CMSG_TRAINER_LIST` requests/refreshes the list; buy is `CMSG_TRAINER_BUY_SPELL`.
// Bodies in [`super::trainer`] (decision 0237).
pub const CMSG_TRAINER_LIST: u16 = 0x01B0; // 432
pub const SMSG_TRAINER_LIST: u16 = 0x01B1; // 433
pub const CMSG_TRAINER_BUY_SPELL: u16 = 0x01B2; // 434
pub const SMSG_TRAINER_BUY_SUCCEEDED: u16 = 0x01B3; // 435
pub const SMSG_TRAINER_BUY_FAILED: u16 = 0x01B4; // 436

// The innkeeper bind set (VERIFIED vmangos `Opcodes_1_12_1.h`: 437, 344, 747; decision 1331).
// Selecting an innkeeper's `GOSSIP_OPTION_INNKEEPER` line makes the server close the gossip menu
// and send `SMSG_BINDER_CONFIRM` — a QUESTION, not a bind. Nothing is bound until the client
// answers `CMSG_BINDER_ACTIVATE`, which is what the confirm dialog's Accept sends; the server then
// has the innkeeper cast spell 3286 "Bind" on the player, whose `SPELL_EFFECT_BIND` sends
// `SMSG_BINDPOINTUPDATE` (above) and `SMSG_PLAYERBOUND` (vmangos `SpellEffects.cpp` `EffectBind`).
// Bodies in [`super::binder`]. `SMSG_PLAYERBINDERROR` (0x01B6) is the refusal and is not parsed:
// vmangos never sends it on any path (`SendBindPoint` simply returns inside an instance).
pub const CMSG_BINDER_ACTIVATE: u16 = 0x01B5; // 437
pub const SMSG_PLAYERBOUND: u16 = 0x0158; // 344
pub const SMSG_BINDER_CONFIRM: u16 = 0x02EB; // 747

// The GM trouble-ticket set (VERIFIED vmangos `Opcodes_1_12_1.h` + `Opcodes.cpp:608-630`; decision
// 1673). Five request/answer pairs behind the Help window, and the client asks for all of them —
// there is no ticket state pushed at login, so `CMSG_GMTICKET_GETTICKET` right after world entry is
// the client's own (seen in the 1.12.1 retail sniff, answered `TicketStatus: 10 (NoText)`).
//
// **Two of these answers can also arrive UNSOLICITED on vmangos**, which is why they are decoded
// rather than correlated to a pending ask: `.ticket viewid`/`viewname`/`escalate`/`complete` push a
// fresh `SMSG_GMTICKET_GETTICKET` at the ticket's author (`GMTicketMgr.cpp:153-159`,
// `TicketCommands.cpp:265,443-452`), and `.ticket delete <id>` pushes a
// `SMSG_GMTICKET_DELETETICKET` = `TICKET_DELETED` (`TicketCommands.cpp:100-103`).
//
// **And two requests can be answered with SILENCE**, which is why nothing in the client blocks on a
// reply: `HandleGMTicketCreateOpcode` simply returns — no packet at all — when the queue is off,
// when the player is under `GMTickets.MinLevel`, or when the category is >= 11
// (`GMTicketHandler.cpp:91,106-113`), and delete-with-no-ticket likewise returns silently (`:73-86`).
// Bodies in [`super::gm_ticket`].
pub const CMSG_GMTICKET_CREATE: u16 = 0x0205; // 517
pub const SMSG_GMTICKET_CREATE: u16 = 0x0206; // 518
pub const CMSG_GMTICKET_UPDATETEXT: u16 = 0x0207; // 519
pub const SMSG_GMTICKET_UPDATETEXT: u16 = 0x0208; // 520
pub const CMSG_GMTICKET_GETTICKET: u16 = 0x0211; // 529
pub const SMSG_GMTICKET_GETTICKET: u16 = 0x0212; // 530
pub const CMSG_GMTICKET_DELETETICKET: u16 = 0x0217; // 535
pub const SMSG_GMTICKET_DELETETICKET: u16 = 0x0218; // 536
pub const CMSG_GMTICKET_SYSTEMSTATUS: u16 = 0x021A; // 538
pub const SMSG_GMTICKET_SYSTEMSTATUS: u16 = 0x021B; // 539

/// The GM's own ticket-state push (VERIFIED vmangos `Opcodes_1_12_1.h`: 808). Body is a bare `u32`:
/// 1 = updated, 2 = closed, 3 = a survey is offered.
///
/// **vmangos never constructs this packet on any path** — it is registered in its opcode table and
/// nothing in its source sends it — so on the server benilla talks to this arm is dead. cmangos-
/// classic makes it the core of its notification model (escalation, first GM read, re-sort, text
/// update, assignment, queue toggle, close), which is why it is parsed rather than dropped.
///
/// It is decoded for one reason beyond completeness: the reference engine answers value 1 by
/// re-asking for the ticket (`0x5e7932`, wow-re §5), the same leg the create/update success codes
/// take. Values 2 and 3 are recorded but not acted on — 3 is the GM-survey trigger, and the survey
/// window is deferred (decision 1673).
pub const SMSG_GM_TICKET_STATUS_UPDATE: u16 = 0x0328; // 808

// The bank set (VERIFIED vmangos `Opcodes_1_12_1.h`: 439,440-441,642-643).
// `CMSG_BANKER_ACTIVATE` right-clicks a pure banker;
// `SMSG_SHOW_BANK` also arrives unprompted from the `GOSSIP_OPTION_BANKER` gossip option. Slot
// purchase and deposit/withdraw bodies in [`super::bank`].
pub const CMSG_BANKER_ACTIVATE: u16 = 0x01B7; // 439
pub const SMSG_SHOW_BANK: u16 = 0x01B8; // 440
pub const CMSG_BUY_BANK_SLOT: u16 = 0x01B9; // 441
pub const SMSG_BUY_BANK_SLOT_RESULT: u16 = 0x01BA; // 442
pub const CMSG_AUTOSTORE_BANK_ITEM: u16 = 0x0282; // 642
pub const CMSG_AUTOBANK_ITEM: u16 = 0x0283; // 643

// The pet-stable set (VERIFIED vmangos `Opcodes_1_12_1.h`: 623-629). `MSG_LIST_STABLED_PETS` is one
// opcode in BOTH directions: the gossip stable option makes the server send the list unprompted
// (that is how the window opens), and the client sends the same number back — one guid — to
// refresh. The four mutations are all answered by a single `SMSG_STABLE_RESULT` byte and nothing
// else, so a successful one is a cue to re-ask the list. Bodies in [`super::stable`]
// (decision 1676). `CMSG_STABLE_REVIVE_PET` (0x0274, 628) is deliberately absent: vmangos's handler
// is an empty no-op and whether the 5875 client ever sends it is an open RE question.
/// Forget a cached player name (VERIFIED vmangos `Opcodes_1_12_1.h`: 796) — body `{u64 guid}`.
/// The client's name cache has **no TTL**: eviction is explicit, and this is the one packet that
/// does it for a player (wow-re `system/dbcache/dbcache.md` Contracts, remove-by-key `0x556ff0`).
/// Decision 1689. vmangos never sends it, so this is the mechanism present and correct rather
/// than a path our own server exercises.
pub const SMSG_INVALIDATE_PLAYER: u16 = 0x031C; // 796

pub const MSG_LIST_STABLED_PETS: u16 = 0x026F; // 623
pub const CMSG_STABLE_PET: u16 = 0x0270; // 624
pub const CMSG_UNSTABLE_PET: u16 = 0x0271; // 625
pub const CMSG_BUY_STABLE_SLOT: u16 = 0x0272; // 626
pub const SMSG_STABLE_RESULT: u16 = 0x0273; // 627
pub const CMSG_STABLE_SWAP_PET: u16 = 0x0275; // 629

/// Spend talent points (VERIFIED vmangos `Opcodes_1_12_1.h`: 593) — body in
/// [`super::progression::learn_talent`]; the server answers with the rank spell's learn effects
/// (`SMSG_LEARNED_SPELL` etc.) and the refreshed `PLAYER_CHARACTER_POINTS1`. Decision 0304.
pub const CMSG_LEARN_TALENT: u16 = 0x0251; // 593

/// The respec question and its answer — **one opcode, both directions** (VERIFIED vmangos
/// `Opcodes_1_12_1.h`: 682 + `Opcodes.cpp`'s `HandleTalentWipeConfirmOpcode` registration; decision
/// 1580). Selecting a class trainer's "I wish to unlearn my talents" line
/// (`GOSSIP_OPTION_UNLEARNTALENTS`, `Player.cpp:12330`) makes the server close the gossip menu and
/// send this **inbound** carrying the trainer's guid and the current cost — a QUESTION, the talent
/// twin of [`SMSG_BINDER_CONFIRM`]. Nothing is unlearned until the client sends the same opcode
/// **outbound** with that guid, which is the `CONFIRM_TALENT_WIPE` dialog's Accept; the server then
/// runs `Player::ResetTalents` and has the trainer cast 14867 "Untalent Visual Effect".
///
/// Byte-verified client-side too (wow-re `system/ui/scratch/talent-api.md` §ConfirmTalentWipe,
/// `0x48dc40` → `0x5df980`): both directions run through one range-gated function, whose outbound
/// leg puts the *latched* trainer guid on the wire. Bodies in [`super::progression`].
pub const MSG_TALENT_WIPE_CONFIRM: u16 = 0x02AA; // 682

/// The player-summon pair — the question and the accept (VERIFIED vmangos `Opcodes_1_12_1.h`:
/// 683/684 + `Opcodes.cpp`'s `HandleSummonResponseOpcode` registration; decision 1747). A
/// warlock's Ritual of Summoning, a meeting stone and a GM `.summon` all arrive as the same
/// inbound question, and the client answers only to say **yes**: there is no decline opcode and no
/// accept flag in the body, so a declined summon is silence and the server's own two-minute timer.
///
/// Byte-verified client-side as well as against vmangos (wow-re: the `0x5ab650` registration site
/// maps `0x2ab` to handler `0x5e6140`, and the `ConfirmSummon` binding `0x48b770` writes `0x2ac`).
/// Bodies in [`super::summon`].
pub const SMSG_SUMMON_REQUEST: u16 = 0x02AB; // 683
pub const CMSG_SUMMON_RESPONSE: u16 = 0x02AC; // 684

/// Unlearn (abandon) a whole skill line — the skills pane's red circle-slash (VERIFIED vmangos
/// `Opcodes.cpp` handler registration + `Opcodes_1_9_4.h`: 514). Body in
/// [`super::skills::unlearn_skill`]; no ack — the removal returns as a `PLAYER_SKILL_INFO`
/// field update.
pub const CMSG_UNLEARN_SKILL: u16 = 0x0202; // 514

/// Ask to flip our own PvP flag (decision 0646). A **two-way** opcode: vmangos reads a `bool`
/// target state iff the body is exactly one byte (`Server/Packets/Misc.cpp`
/// `TogglePvP::ReadFromWorldPacket`) and otherwise *toggles* `PLAYER_FLAGS_PVP_DESIRED`
/// (`Handlers/MiscHandler.cpp` `HandleTogglePvP`). benilla sends the **empty** body — the verb is
/// a toggle. No ack: the answer arrives as the `UNIT_FIELD_FLAGS` PvP bit in a descriptor update,
/// and turning the preference *off* changes nothing until the server's 300 s drop timer expires.
pub const CMSG_TOGGLE_PVP: u16 = 0x0253; // 595

/// The two **equipment-display** toggles — "show my helm" / "show my cloak" (VERIFIED vmangos
/// `Opcodes_1_12_1.h`: 697/698, handlers `HandleShowingHelmOpcode`/`HandleShowingCloakOpcode` in
/// `Handlers/CharacterHandler.cpp:753-761`). Both are **empty-bodied pure toggles**: the handler
/// is a bare `ToggleFlag(PLAYER_FLAGS, PLAYER_FLAGS_HIDE_HELM | HIDE_CLOAK)` with no target-state
/// form at all, unlike [`CMSG_TOGGLE_PVP`] above — so a client that wants a *specific* state must
/// compare against the flag it already holds and send only on a difference.
///
/// No ack, exactly like the PvP toggle: the answer is the `PLAYER_FLAGS` bit arriving in the next
/// descriptor update. That field is `UF_FLAG_PUBLIC` (vmangos `UpdateFields_1_12_1`), which is what
/// makes the preference *everyone's* — a remote player's hidden helm hides on our screen too, off
/// their own descriptor. The server round-trips the same preference through the char-enum record's
/// `CHARACTER_FLAG_HIDE_HELM`/`HIDE_CLOAK` at load and save (`Player.cpp:14839-14842` /
/// `16504-16505`), so the glue lane's flags and this one are the same stored bit (decision 1472).
pub const CMSG_TOGGLE_HELM: u16 = 0x02B9; // 697
/// The cloak half of [`CMSG_TOGGLE_HELM`] — same shape, `PLAYER_FLAGS_HIDE_CLOAK`.
pub const CMSG_TOGGLE_CLOAK: u16 = 0x02BA; // 698

// The solo-loot wire family (VERIFIED vmangos `Opcodes_1_12_1.h`: 264, 349-355, 357-358).
// Bodies in [`super::loot`].
pub const CMSG_AUTOSTORE_LOOT_ITEM: u16 = 0x0108; // 264
pub const CMSG_LOOT: u16 = 0x015D; // 349
pub const CMSG_LOOT_MONEY: u16 = 0x015E; // 350
pub const CMSG_LOOT_RELEASE: u16 = 0x015F; // 351
pub const SMSG_LOOT_RESPONSE: u16 = 0x0160; // 352
pub const SMSG_LOOT_RELEASE_RESPONSE: u16 = 0x0161; // 353
pub const SMSG_LOOT_REMOVED: u16 = 0x0162; // 354
pub const SMSG_LOOT_MONEY_NOTIFY: u16 = 0x0163; // 355
pub const SMSG_LOOT_CLEAR_MONEY: u16 = 0x0165; // 357
pub const SMSG_ITEM_PUSH_RESULT: u16 = 0x0166; // 358

// The group-loot roll family (VERIFIED vmangos `Opcodes_1_12_1.h:671-675`) — the Need/Greed/Pass
// flow the `GroupLootFrame`s drive when the group's loot method is group/need-before-greed and a
// drop is at or above the quality threshold (decision 0591). Bodies in [`super::loot`].
pub const SMSG_LOOT_ALL_PASSED: u16 = 0x029E; // 670
pub const SMSG_LOOT_ROLL_WON: u16 = 0x029F; // 671
pub const CMSG_LOOT_ROLL: u16 = 0x02A0; // 672
pub const SMSG_LOOT_START_ROLL: u16 = 0x02A1; // 673
pub const SMSG_LOOT_ROLL: u16 = 0x02A2; // 674

// Master loot (VERIFIED vmangos `Opcodes_1_12_1.h:676-677`) — the other answer to an
// above-threshold drop: no roll, the master looter is handed the eligible-member list at
// window-open and assigns each row from a dropdown (decision 1675). Bodies in [`super::loot`].
pub const CMSG_LOOT_MASTER_GIVE: u16 = 0x02A3; // 675
pub const SMSG_LOOT_MASTER_LIST: u16 = 0x02A4; // 676

// The death arc (decision 0308) — release/repop, corpse query, reclaim, spirit healer, resurrect
// requests (all VERIFIED vmangos `Opcodes_1_12_1.h`: 346-348, 466, 534, 540, 546, 617). Bodies in
// [`super::death`]. `CMSG_REPOP_REQUEST` and our `MSG_CORPSE_QUERY` request are EMPTY bodies.
pub const CMSG_REPOP_REQUEST: u16 = 0x015A; // 346
pub const SMSG_RESURRECT_REQUEST: u16 = 0x015B; // 347
pub const CMSG_RESURRECT_RESPONSE: u16 = 0x015C; // 348
pub const CMSG_RECLAIM_CORPSE: u16 = 0x01D2; // 466
pub const MSG_CORPSE_QUERY: u16 = 0x0216; // 534
pub const CMSG_SPIRIT_HEALER_ACTIVATE: u16 = 0x021C; // 540
pub const SMSG_SPIRIT_HEALER_CONFIRM: u16 = 0x0222; // 546
pub const SMSG_CORPSE_RECLAIM_DELAY: u16 = 0x0269; // 617
/// Self-resurrect — the DEATH popup's soulstone/Reincarnation button (`UseSoulstone()`, decision
/// 1746). **EMPTY body** (VERIFIED vmangos `Opcodes_1_12_1.h:692` = 691, handler
/// `WorldSession::HandleSelfResOpcode(NullClientPacket const&)`, `SpellHandler.cpp:461`): the
/// server casts whatever `PLAYER_SELF_RES_SPELL` holds on us and zeroes the field. There is no
/// answer packet — the resurrection arrives as ordinary descriptor deltas, exactly like a reclaim.
pub const CMSG_SELF_RES: u16 = 0x02B3; // 691
/// Sent with the 10% durability loss a natural (non-PvP) death applies (VERIFIED vmangos
/// `Opcodes_1_12_1.h` 701 + `Unit.cpp:1170-1182` — an EMPTY body; the client's cue for the red
/// "Your equipped items suffer a 10%% durability loss." error line).
pub const SMSG_DURABILITY_DAMAGE_DEATH: u16 = 0x02BD; // 701

// **The ack'd movement-mode family** — four granted mover modes, one wire shape (decision 0866).
// This is not our grouping: the server names the set itself in `IsFlagAckOpcode`
// (`Server/Protocol/Opcodes.h`) and routes all four through one sender
// (`MovementPacketSender::AddMovementFlagChangeToController`, whose `MovementChangeType` is exactly
// `{ROOT, WATER_WALK, SET_HOVER, FEATHER_FALL}`). Opcode numbers VERIFIED vmangos `Opcodes_1_1x.h`
// (unchanged into 1.12.1) and cross-checked against the client's own name table
// ([`super::opcode_names`]).
//
// **The wire shape is uniform.** The SMSG to the mover carries `packed guid + u32 counter`; the ack
// echoes `full u64 guid + u32 counter + MovementInfo`, plus a trailing `u32 apply` for every mode
// **except root** — the root ack lands on vmangos's `HandleMoveRootAck`, the other three on
// `HandleMovementFlagChangeToggleAck`, which reads that extra dword
// (`Server/Packets/Movement.cpp:38-59`). Un-acked changes never reach observers, and wrong/zero
// counters trip the server's cheat log — the counter must be echoed. Modelled as
// [`super::MoveMode`], whose docs carry what each flag does to the mover.
pub const SMSG_MOVE_WATER_WALK: u16 = 0x00DE; // 222
pub const SMSG_MOVE_LAND_WALK: u16 = 0x00DF; // 223
pub const CMSG_MOVE_WATER_WALK_ACK: u16 = 0x02D0; // 720
pub const SMSG_FORCE_MOVE_ROOT: u16 = 0x00E8; // 232
pub const CMSG_FORCE_MOVE_ROOT_ACK: u16 = 0x00E9; // 233
pub const SMSG_FORCE_MOVE_UNROOT: u16 = 0x00EA; // 234
pub const CMSG_FORCE_MOVE_UNROOT_ACK: u16 = 0x00EB; // 235
pub const SMSG_MOVE_FEATHER_FALL: u16 = 0x00F2; // 242
pub const SMSG_MOVE_NORMAL_FALL: u16 = 0x00F3; // 243
pub const SMSG_MOVE_SET_HOVER: u16 = 0x00F4; // 244
pub const SMSG_MOVE_UNSET_HOVER: u16 = 0x00F5; // 245
pub const CMSG_MOVE_HOVER_ACK: u16 = 0x00F6; // 246
pub const CMSG_MOVE_FEATHER_FALL_ACK: u16 = 0x02CF; // 719

// **The knockback handshake** — the server aims a launch at our own mover, the mover flies it, and
// the ack is what the server relays onward (decision 1702). Numbers VERIFIED vmangos
// `Opcodes_1_11_2.h:242-244` (unchanged into 1.12.1) and cross-checked against the client's own name
// table ([`super::opcode_names`]).
//
// **The shape.** `SMSG_MOVE_KNOCK_BACK` carries `packed guid + u32 counter + f32 vcos + f32 vsin +
// f32 speedXY + f32 speedZ` (vmangos `MovementPacketSender::SendKnockBackToController`, the
// `> CLIENT_BUILD_1_9_4` branch that carries the counter). Those last four floats are a
// [`super::JumpInfo`] in a different field order — the same launch quad, and that is not a
// coincidence: the ack must echo them back as the `MovementInfo` **jump tail**, which is why
// `speedZ` rides the jump tail's **down-positive** convention (vmangos negates the caller's
// upward `verticalSpeed` on the way out, with the source comment "!! notice the - sign in front of
// speedZ !!").
//
// **The ack is mandatory and checked value-by-value.** `CMSG_MOVE_KNOCK_BACK_ACK` is
// `full u64 guid + u32 counter + MovementInfo` (`Server/Packets/Movement.cpp:61-68` — the same
// shape as the movement-mode family's ack minus its trailing `apply` dword), and
// `Unit::FindPendingMovementKnockbackChange` refuses it unless the counter matches **and** all four
// jump-tail floats are within `0.01` of what was sent — so `MOVEFLAG_JUMPING` must be set and the
// tail must be the launch quad. A mismatched ack is `OnWrongAckData`; a missing one is
// `OnFailedToAckChange` (a knockback is the one pending change the server will not re-send,
// `Unit.cpp:6887`). The ack is also the observers' packet: the server answers it with
// `MSG_MOVE_KNOCK_BACK` built from the `MovementInfo` we sent.
pub const SMSG_MOVE_KNOCK_BACK: u16 = 0x00EF; // 239
pub const CMSG_MOVE_KNOCK_BACK_ACK: u16 = 0x00F0; // 240
                                                  //
                                                  // **And the observers hear it — `MSG_MOVE_KNOCK_BACK` is NOT dead** (decision 1702, correcting
                                                  // decision 0277's "no observer-side knockback signal exists in 1.12 at all"). The reference
                                                  // registers `0xF1` at its own dispatch entry `0x603bb0`, which reaches `CGUnit_C::OnKnockBack`
                                                  // `0x6026f0` — it reads the four floats *after* a full `MovementInfo` and re-launches the unit
                                                  // locally through the very same `0x6179c0` the controller's own knockback uses, acking nothing.
                                                  // That body (`packed guid + MovementInfo + vcos + vsin + speedXY + speedZ`) is exactly what vmangos
                                                  // sends (`SendKnockBackToObservers`), and its leading `[packed guid][MovementInfo]` is the ordinary
                                                  // relay shape — the launch quad also rides that `MovementInfo`'s own jump tail, so a receiver that
                                                  // replays the relayed arc reproduces the re-launch without reading the trailing four.
pub const MSG_MOVE_KNOCK_BACK: u16 = 0x00F1; // 241

// The social family — the friend list, the ignore list, and `/who` (VERIFIED vmangos
// `Opcodes_1_12_1.h` + `Server/Packets/Social.{h,cpp}`, `Handlers/MiscHandler.cpp`,
// `SocialMgr.{h,cpp}`; client side wow-re's `FriendList.cpp` TU, `system/net/scratch/w2b.md`).
// Bodies in [`super::social`]; decision 0668.
pub const CMSG_WHO: u16 = 0x0062; // 98
pub const SMSG_WHO: u16 = 0x0063; // 99
pub const CMSG_FRIEND_LIST: u16 = 0x0066; // 102
pub const SMSG_FRIEND_LIST: u16 = 0x0067; // 103
pub const SMSG_FRIEND_STATUS: u16 = 0x0068; // 104
pub const CMSG_ADD_FRIEND: u16 = 0x0069; // 105
pub const CMSG_DEL_FRIEND: u16 = 0x006A; // 106
pub const SMSG_IGNORE_LIST: u16 = 0x006B; // 107
pub const CMSG_ADD_IGNORE: u16 = 0x006C; // 108
pub const CMSG_DEL_IGNORE: u16 = 0x006D; // 109

// The guild family — the query/roster caches, invitations, the member verbs, rank administration
// and the event broadcast (VERIFIED vmangos `Opcodes_1_12_1.h:87-88`, `:132-151`, `:562-566`,
// `:765` + `Server/Packets/Guild.{h,cpp}`, `Guild/Guild.{h,cpp}`, `Handlers/GuildHandler.cpp`; the
// client side of `SMSG_GUILD_EVENT` is wow-re's RF-0077,
// `system/object-layer/scratch/rf77-smsg-chat-wire-order.md`, which pins handler `0x5e7180` to
// this 0x92 and reads its guild-init registration/teardown block). Bodies in [`super::guild`].
//
// The numbers are two contiguous runs plus two strays: the query pair sits with the other ask-once
// caches at 0x54/0x55, the core family runs 0x81-0x93 immediately after the group family, rank
// administration was appended at 0x231-0x235, and the guild info text at 0x2FC. The
// charter/petition opcodes that *found* a guild are their own family, below; the tabard
// opcodes that dress one are not built.
pub const CMSG_GUILD_QUERY: u16 = 0x0054; // 84
pub const SMSG_GUILD_QUERY_RESPONSE: u16 = 0x0055; // 85
/// vmangos registers this `STATUS_NEVER` (`Opcodes.cpp:210`): at 1.12 a guild is founded through
/// the charter/petition flow, not by this packet. Modelled for completeness; no reply comes back.
pub const CMSG_GUILD_CREATE: u16 = 0x0081; // 129
pub const CMSG_GUILD_INVITE: u16 = 0x0082; // 130
pub const SMSG_GUILD_INVITE: u16 = 0x0083; // 131
pub const CMSG_GUILD_ACCEPT: u16 = 0x0084; // 132
pub const CMSG_GUILD_DECLINE: u16 = 0x0085; // 133
pub const SMSG_GUILD_DECLINE: u16 = 0x0086; // 134
pub const CMSG_GUILD_INFO: u16 = 0x0087; // 135
pub const SMSG_GUILD_INFO: u16 = 0x0088; // 136
pub const CMSG_GUILD_ROSTER: u16 = 0x0089; // 137
/// The widest packet in the game — the sender truncates its member list to `0x8000 - 4` bytes —
/// and the one conditional-field trap in the family: see [`super::guild::read_guild_roster`].
pub const SMSG_GUILD_ROSTER: u16 = 0x008A; // 138
pub const CMSG_GUILD_PROMOTE: u16 = 0x008B; // 139
pub const CMSG_GUILD_DEMOTE: u16 = 0x008C; // 140
pub const CMSG_GUILD_LEAVE: u16 = 0x008D; // 141
pub const CMSG_GUILD_REMOVE: u16 = 0x008E; // 142
pub const CMSG_GUILD_DISBAND: u16 = 0x008F; // 143
pub const CMSG_GUILD_LEADER: u16 = 0x0090; // 144
pub const CMSG_GUILD_MOTD: u16 = 0x0091; // 145
pub const SMSG_GUILD_EVENT: u16 = 0x0092; // 146
pub const SMSG_GUILD_COMMAND_RESULT: u16 = 0x0093; // 147
pub const CMSG_GUILD_RANK: u16 = 0x0231; // 561
pub const CMSG_GUILD_ADD_RANK: u16 = 0x0232; // 562
pub const CMSG_GUILD_DEL_RANK: u16 = 0x0233; // 563
pub const CMSG_GUILD_SET_PUBLIC_NOTE: u16 = 0x0234; // 564
pub const CMSG_GUILD_SET_OFFICER_NOTE: u16 = 0x0235; // 565
pub const CMSG_GUILD_INFO_TEXT: u16 = 0x02FC; // 764

// The petition family — the guild-charter flow that FOUNDS a guild (VERIFIED vmangos
// `Opcodes_1_12_1.h:444-456`, `:705-706` + `Server/Packets/Petition.{h,cpp}`,
// `Handlers/PetitionsHandler.cpp`; the handler table is `Opcodes.cpp:528-540` and `:800-801`).
// Bodies in [`super::petition`]; decision 1672.
//
// Two shapes to know. The three `MSG_` opcodes are genuinely bidirectional with *different*
// bodies each way — decline sends an item guid and receives a player guid — so each has its own
// reader and its own builder rather than one shared body. And `MSG_DELETE_GUILD_CHARTER` (0x2C0)
// is deliberately **absent**: vmangos registers it `INVALID_PACKET(…, Unhandled)`
// (`Opcodes.cpp:800`) and has no handler at all, because destroying a charter runs through the
// ordinary item-destroy path, which cascades into deleting the petition (`Player.cpp:10811-10817`,
// `Item.cpp:515-516`). Declaring it would model traffic that nothing on either end sends.
pub const CMSG_PETITION_SHOWLIST: u16 = 0x01BB; // 443
pub const SMSG_PETITION_SHOWLIST: u16 = 0x01BC; // 444
pub const CMSG_PETITION_BUY: u16 = 0x01BD; // 445
pub const CMSG_PETITION_SHOW_SIGNATURES: u16 = 0x01BE; // 446
/// The answer to **two** different asks — our own `CMSG_PETITION_SHOW_SIGNATURES`, and someone
/// else's `CMSG_OFFER_PETITION` aimed at us. Only its `owner` field tells the two apart.
pub const SMSG_PETITION_SHOW_SIGNATURES: u16 = 0x01BF; // 447
pub const CMSG_PETITION_SIGN: u16 = 0x01C0; // 448
pub const SMSG_PETITION_SIGN_RESULTS: u16 = 0x01C1; // 449
/// Bidirectional with different bodies: we send the charter **item's** guid, the owner receives
/// the declining **player's** guid.
pub const MSG_PETITION_DECLINE: u16 = 0x01C2; // 450
pub const CMSG_OFFER_PETITION: u16 = 0x01C3; // 451
pub const CMSG_TURN_IN_PETITION: u16 = 0x01C4; // 452
pub const SMSG_TURN_IN_PETITION_RESULTS: u16 = 0x01C5; // 453
pub const CMSG_PETITION_QUERY: u16 = 0x01C6; // 454
pub const SMSG_PETITION_QUERY_RESPONSE: u16 = 0x01C7; // 455
/// Bidirectional, and here the two bodies agree: `u64 item` + the new name, both ways. The echo
/// comes back only on success.
pub const MSG_PETITION_RENAME: u16 = 0x02C1; // 705

// The group/party family — invite/accept/decline/kick/leader/disband, the loot-method setting, the
// roster push (`SMSG_GROUP_LIST`), party command feedback, live member stats for the party/raid
// frame, minimap pings, raid subgroup management, raid-target icons, and ready checks (VERIFIED
// vmangos `Opcodes_1_12_1.h` + `Server/Packets/Group.{h,cpp}`, `Handlers/GroupHandler.cpp`,
// `Group/Group.{h,cpp}`). Bodies in [`super::group`].
pub const CMSG_GROUP_INVITE: u16 = 0x006E; // 110
pub const SMSG_GROUP_INVITE: u16 = 0x006F; // 111
pub const CMSG_GROUP_ACCEPT: u16 = 0x0072; // 114
pub const CMSG_GROUP_DECLINE: u16 = 0x0073; // 115
pub const SMSG_GROUP_DECLINE: u16 = 0x0074; // 116
pub const CMSG_GROUP_UNINVITE: u16 = 0x0075; // 117
pub const CMSG_GROUP_UNINVITE_GUID: u16 = 0x0076; // 118
pub const SMSG_GROUP_UNINVITE: u16 = 0x0077; // 119
pub const CMSG_GROUP_SET_LEADER: u16 = 0x0078; // 120
pub const SMSG_GROUP_SET_LEADER: u16 = 0x0079; // 121
pub const CMSG_LOOT_METHOD: u16 = 0x007A; // 122
pub const CMSG_GROUP_DISBAND: u16 = 0x007B; // 123
pub const SMSG_GROUP_DESTROYED: u16 = 0x007C; // 124
pub const SMSG_GROUP_LIST: u16 = 0x007D; // 125
pub const SMSG_PARTY_MEMBER_STATS: u16 = 0x007E; // 126
pub const SMSG_PARTY_COMMAND_RESULT: u16 = 0x007F; // 127
/// Same opcode both directions (VERIFIED vmangos `Group.cpp:33-37` client read, `Handlers/
/// GroupHandler.cpp:382-391` server rebroadcast): the client's own ping carries no guid, the
/// server stamps ours on before relaying to the rest of the group (see
/// [`super::group::minimap_ping`] / the parse arm).
pub const MSG_MINIMAP_PING: u16 = 0x01D5; // 469
pub const CMSG_GROUP_CHANGE_SUB_GROUP: u16 = 0x027E; // 638
pub const CMSG_REQUEST_PARTY_MEMBER_STATS: u16 = 0x027F; // 639
pub const CMSG_GROUP_SWAP_SUB_GROUP: u16 = 0x0280; // 640
pub const CMSG_GROUP_RAID_CONVERT: u16 = 0x028E; // 654
pub const CMSG_GROUP_ASSISTANT_LEADER: u16 = 0x028F; // 655
pub const SMSG_PARTY_MEMBER_STATS_FULL: u16 = 0x02F2; // 754
/// Same opcode both directions, mode-prefixed shapes on the server's side (VERIFIED vmangos
/// `Server/Packets/Group.cpp:77-82` client read, `:132-147` server write; see
/// [`super::group::RaidTargetUpdate`]).
pub const MSG_RAID_TARGET_UPDATE: u16 = 0x0321; // 801
/// Same opcode both directions — an empty body starts/requests, a non-empty one answers (VERIFIED
/// vmangos `Server/Packets/Group.cpp:84-96`, `:126-130`; see [`super::group::ReadyCheck`]).
pub const MSG_RAID_READY_CHECK: u16 = 0x0322; // 802
/// Ask the server for our saved-instance (raid lockout) list — empty body (VERIFIED vmangos
/// `Server/Protocol/Opcodes.cpp` registers it against `HandleRequestRaidInfoOpcode`, which is a
/// bare `SendRaidInfo()`). The RaidFrame's `RequestRaidInfo()` on every OnShow (decision 1549).
pub const CMSG_REQUEST_RAID_INFO: u16 = 0x02CD; // 717
/// The answer: `u32 count` then `count` × `{u32 mapId, u32 secondsUntilReset, u32 instanceId}`
/// (VERIFIED vmangos `Objects/Player.cpp::Player::SendRaidInfo`, permanent binds only; see
/// [`super::group::read_raid_instance_info`]).
pub const SMSG_RAID_INSTANCE_INFO: u16 = 0x02CC; // 716

// The duel family (decision 0633) — the six inbound handlers WoW.exe registers in
// `Ui/DuelInfo.cpp` at `0x4d4710` plus the two it sends from `AcceptDuel 0x4d4830` /
// `CancelDuel 0x4d48b0`; server side VERIFIED vmangos `Server/Packets/Duel.{h,cpp}` +
// `Handlers/DuelHandler.cpp`. There is no CMSG for *starting* a duel: the challenge is a normal
// `CMSG_CAST_SPELL` of the spellbook spell whose `Effect[0]` is `SPELL_EFFECT_DUEL` (83).
// Bodies in [`super::duel`].
pub const SMSG_DUEL_REQUESTED: u16 = 0x0167; // 359
pub const SMSG_DUEL_OUTOFBOUNDS: u16 = 0x0168; // 360
pub const SMSG_DUEL_INBOUNDS: u16 = 0x0169; // 361
pub const SMSG_DUEL_COMPLETE: u16 = 0x016A; // 362
pub const SMSG_DUEL_WINNER: u16 = 0x016B; // 363
pub const CMSG_DUEL_ACCEPTED: u16 = 0x016C; // 364
pub const CMSG_DUEL_CANCELLED: u16 = 0x016D; // 365
/// Milliseconds, not seconds — the client divides by 1000 (`0x4d4aef`). Live on 5875 despite the
/// opcode's high number: `0x4d474a` registers a handler for it alongside the 0x167 block.
pub const SMSG_DUEL_COUNTDOWN: u16 = 0x02B7; // 695

// The mirror timers — breath / fatigue / feign-death (decision 0874; VERIFIED vmangos
// `Opcodes_1_12_1.h:474-476` + `Server/Packets/Misc.{h,cpp}`). Server-authoritative countdowns the
// client only mirrors and integrates; the family has no CMSG and no separate "update" opcode — a
// running timer that changes anything is re-sent as a whole START. Bodies in
// [`super::mirror_timer`].
pub const SMSG_START_MIRROR_TIMER: u16 = 0x01D9; // 473
/// Never sent by vmangos, on purpose (`Player::SendMirrorTimers` substitutes a full START and says
/// why in the source): the shipped 1.12 `MirrorTimer.lua` handler reads the same `arg1` as both a
/// timer name and a number, so a real pause packet errors out in the reference UI. Decoded anyway.
pub const SMSG_PAUSE_MIRROR_TIMER: u16 = 0x01DA; // 474
pub const SMSG_STOP_MIRROR_TIMER: u16 = 0x01DB; // 475

// The mail arc (decision 0544; VERIFIED vmangos `Opcodes_1_12_1.h` + `Handlers/MailHandler.cpp`).
// There is no `SMSG_SHOW_MAILBOX` on 5875 — the mailbox window opens entirely client-side; every
// CMSG below carries the mailbox guid at its head, and the server independently re-validates the
// 5 yd interact distance (`CheckMailBox`) on each one. Bodies in [`super::mail`].
pub const CMSG_SEND_MAIL: u16 = 0x0238; // 568
pub const SMSG_SEND_MAIL_RESULT: u16 = 0x0239; // 569
pub const CMSG_GET_MAIL_LIST: u16 = 0x023A; // 570
pub const SMSG_MAIL_LIST_RESULT: u16 = 0x023B; // 571
/// Letter bodies never ride the list inline — the client fetches them ask-once by `itemTextId`.
pub const CMSG_ITEM_TEXT_QUERY: u16 = 0x0243; // 579
pub const SMSG_ITEM_TEXT_QUERY_RESPONSE: u16 = 0x0244; // 580
pub const CMSG_MAIL_TAKE_MONEY: u16 = 0x0245; // 581
pub const CMSG_MAIL_TAKE_ITEM: u16 = 0x0246; // 582
/// No response packet — the app flips the local `checked` READ bit and repaints.
pub const CMSG_MAIL_MARK_AS_READ: u16 = 0x0247; // 583
pub const CMSG_MAIL_RETURN_TO_SENDER: u16 = 0x0248; // 584
pub const CMSG_MAIL_DELETE: u16 = 0x0249; // 585
pub const CMSG_MAIL_CREATE_TEXT_ITEM: u16 = 0x024A; // 586
/// Same opcode both directions (VERIFIED vmangos `Opcodes_1_12_1.h`: 644): our request is an EMPTY
/// body; the reply (same opcode) is one `f32` — `0.0` unread mail waiting, `-86400.0` none.
pub const MSG_QUERY_NEXT_MAIL_TIME: u16 = 0x0284; // 644
/// One `u32` (always 0) — sent when a mail *arrives* (instant for text-only, on the delivery
/// timer's expiry otherwise).
pub const SMSG_RECEIVED_MAIL: u16 = 0x0285; // 645

// The auction house arc (decision 1511 P0; VERIFIED vmangos `Opcodes_1_12_1.h`: 597-607, 612-613,
// 653 — every value re-read against that table). Bodies in [`super::auction`]. Two family facts
// belong here: **there is no `*_LIST_PENDING_SALES` on 5875** — the symbol exists in no vmangos
// opcode table at all, so the pending-sales pane is a later client's and has no wire to build —
// and every CMSG below carries the auctioneer guid at its head because the server re-validates
// the 5 yd interact distance on each one independently; the hello is not a session token.
/// Same opcode both directions (597): our request is one auctioneer guid; the reply echoes it and
/// adds `u32 houseId` (`AuctionHouse.dbc`, rows 1..7, which carries the deposit/cut rates). It is
/// the **reply** that opens the window, not our send.
pub const MSG_AUCTION_HELLO: u16 = 0x0255; // 597
pub const CMSG_AUCTION_SELL_ITEM: u16 = 0x0256; // 598
pub const CMSG_AUCTION_REMOVE_ITEM: u16 = 0x0257; // 599
/// The Browse search — ten fields and **no sort bytes** on 5875: filtering is server-side,
/// sorting entirely client-side ([`super::auction::auction_list_items`]).
pub const CMSG_AUCTION_LIST_ITEMS: u16 = 0x0258; // 600
pub const CMSG_AUCTION_LIST_OWNER_ITEMS: u16 = 0x0259; // 601
/// Bid *and* buy out: buyout is inferred from the price (`price >= buyout && buyout != 0`), never
/// flagged, and there is no separate opcode for it.
pub const CMSG_AUCTION_PLACE_BID: u16 = 0x025A; // 602
/// The one verdict opcode for sell/cancel/bid, with a conditional tail keyed on its error
/// ([`super::auction::AuctionCommandTail`]). Several refusals send **nothing at all** instead.
pub const SMSG_AUCTION_COMMAND_RESULT: u16 = 0x025B; // 603
/// This and the two list results below share ONE frame and ONE 64-byte record layout
/// ([`super::auction::read_auction_list_result`]): `u32 count`, the records, then `u32 totalCount`
/// at the very end — the match count *before* the 50-row page cap.
pub const SMSG_AUCTION_LIST_RESULT: u16 = 0x025C; // 604
pub const SMSG_AUCTION_OWNER_LIST_RESULT: u16 = 0x025D; // 605
/// Pushed to the bidder: won, or outbid — `bidOrZero == 0` means **won**, not "no bid".
pub const SMSG_AUCTION_BIDDER_NOTIFICATION: u16 = 0x025E; // 606
/// Pushed to the seller. A **different field order** from the bidder notification, and no
/// `houseId`; the two never share a struct or a reader.
pub const SMSG_AUCTION_OWNER_NOTIFICATION: u16 = 0x025F; // 607
pub const CMSG_AUCTION_LIST_BIDDER_ITEMS: u16 = 0x0264; // 612
pub const SMSG_AUCTION_BIDDER_LIST_RESULT: u16 = 0x0265; // 613
/// Pushed to a bidder whose auction the seller cancelled.
pub const SMSG_AUCTION_REMOVED_NOTIFICATION: u16 = 0x028D; // 653

// The world-state table (VERIFIED both ways: vmangos `Opcodes_1_12_1.h` 706-707, and wow-re's read
// of the reference's own handler `0x48f690`, whose registration at `0x48f515-0x48f52f` selects these
// two arms as the ONLY writers of the table setter `0x4c5870`). Bodies in
// [`super::world_state`]. Backs the NPC-text `$<n>w`/`$<n>e` tokens today; the BG/PvP scoreboard
// later.
pub const SMSG_INIT_WORLD_STATES: u16 = 0x02C2; // 706
pub const SMSG_UPDATE_WORLD_STATE: u16 = 0x02C3; // 707

// ── The instance/raid lockout family (decision 1748) ──────────────────────────────────────────
//
// The five inbound handlers WoW.exe registers together at `0x498680`-`0x4986cf`, plus the
// save-created one registered apart at `0x4e7e48`, plus the one thing the player can ask for.
// Bodies (and the client's own read order) in [`super::instance`]; server side VERIFIED vmangos
// `Server/Packets/Misc.{h,cpp}` + `Maps/Map.cpp`.

/// "You are now saved to this instance" (`u32` flag; handler `0x4e7e60`). vmangos always sends
/// `0`; the client's `== 1` arm wraps the same string in a `"(Debug-Only Lock Notice) %s"`
/// literal, and anything ≥ 2 prints an uninitialized buffer (a real 1.12 bug — we print nothing).
pub const SMSG_INSTANCE_SAVE_CREATED: u16 = 0x02CB; // 715
/// The raid-lockout welcome/countdown line: `u32 type`, `u32 mapId`, `u32 secondsUntilReset`
/// (handler `0x49e1c0`; the four types in [`super::instance::RaidInstanceWarning`]).
pub const SMSG_RAID_INSTANCE_MESSAGE: u16 = 0x02FA; // 762
/// "Reset all instances" — empty body (the `ResetInstances` binding `0x48a6b0`; vmangos
/// `HandleResetInstancesOpcode`).
pub const CMSG_RESET_INSTANCES: u16 = 0x031D; // 797
/// "%s has been reset." — `u32 mapId` (handler `0x49e470`, which also clears the last-instance
/// latch `0x495d00` *before* reading the body).
pub const SMSG_INSTANCE_RESET: u16 = 0x031E; // 798
/// The reset refusal: `u32 reason`, `u32 mapId` (handler `0x49e540`; the three reasons in
/// [`super::instance::InstanceResetFailure`]).
pub const SMSG_INSTANCE_RESET_FAILED: u16 = 0x031F; // 799
/// The dungeon we were last in: `u32 mapId` (handler `0x49e670` → `0x495d10`). Shows no line —
/// it is half of what `CanShowResetInstances()` reads.
pub const SMSG_UPDATE_LAST_INSTANCE: u16 = 0x0320; // 800
/// Whether we hold any permanent bind: `u32` flag (handler `0x49e6c0` → `0x495d50`). The other
/// half of `CanShowResetInstances()`.
pub const SMSG_UPDATE_INSTANCE_OWNERSHIP: u16 = 0x032B; // 811
