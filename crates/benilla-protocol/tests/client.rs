//! Client-body + query wire tests (mirrors `src/messages/client.rs`): the golden client-encoded
//! bodies (auth/char-create/chat/emote/stand-state/movement/teleport-ack) and the creature/name
//! query request+response roundtrip. Split out of the former `tests/messages.rs` (decision-adjacent
//! mechanical split — see `tests/common` for the shared fixtures and methodology note).

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{self, MovementInfo};
use benilla_protocol::wire::Vector3d;
use benilla_protocol::ServerPacket;
use common::hx;

/// The faction tongue every chat send must speak ([`messages::faction_language`]): race → tongue
/// VERIFIED against the live world DB (`playercreateinfo_spell` — Alliance races 1/3/4/7 learn
/// spell 668 Language Common, Horde races 2/5/6/8 learn 669 Language Orcish); wire ids VERIFIED
/// vmangos `SharedDefines.h:256-261` (`LANG_ORCISH = 1`, `LANG_COMMON = 7`).
#[test]
fn faction_language_per_race() {
    for race in [1u8, 3, 4, 7] {
        assert_eq!(
            messages::faction_language(race),
            messages::LANGUAGE_COMMON,
            "alliance race {race}"
        );
    }
    for race in [2u8, 5, 6, 8] {
        assert_eq!(
            messages::faction_language(race),
            messages::LANGUAGE_ORCISH,
            "horde race {race}"
        );
    }
}

#[test]
fn client_bodies_golden() {
    let proof: [u8; 20] = std::array::from_fn(|i| (i as u8).wrapping_mul(3).wrapping_add(7));
    assert_eq!(
        messages::auth_session(5875, "TESTUSER", 0x1122_3344, &proof),
        hx("f31600000000000054455354555345520044332211070a0d101316191c1f2225282b2e3134373a3d4000000000789c030000000001"),
        "CMSG_AUTH_SESSION body"
    );
    // Zero-appearance body (the create-if-empty starter): name + [race,class,gender] + 5 zeros + 0.
    assert_eq!(
        messages::char_create(&messages::CharCreateReq {
            name: "Benilla".into(),
            race: 1,
            class: 1,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        }),
        hx("42656e696c6c6100010100000000000000"),
        "CMSG_CHAR_CREATE body (zero appearance)"
    );
    // Distinct appearance dials (3/4/5/6/7) — a misplaced byte can't pass. Body is name + nul +
    // race(01) class(01) gender(00) skin(03) face(04) hairStyle(05) hairColor(06) facialHair(07)
    // outfit(00), matching vmangos's read order (`Packets/Character.cpp:4-19`).
    assert_eq!(
        messages::char_create(&messages::CharCreateReq {
            name: "Benilla".into(),
            race: 1,
            class: 1,
            gender: 0,
            skin: 3,
            face: 4,
            hair_style: 5,
            hair_color: 6,
            facial_hair: 7,
        }),
        hx("42656e696c6c6100010100030405060700"),
        "CMSG_CHAR_CREATE body (distinct appearance)"
    );
    assert_eq!(
        messages::messagechat(0, 7, ".tele Westfall"),
        hx("00000000070000002e74656c65205765737466616c6c00"),
        "CMSG_MESSAGECHAT body"
    );
    // The three-field vmangos shape (`Misc.cpp:60-65`): textEmote, emoteNum(0), full target
    // guid. The original two-field body was silently discarded server-side (the guid read ran
    // off the packet) — /wave did nothing on any client until the director caught it on the ref.
    assert_eq!(
        messages::text_emote(101, 0x2A),
        hx("65000000000000002a00000000000000"),
        "CMSG_TEXT_EMOTE body"
    );
    assert_eq!(
        messages::stand_state_change(1),
        hx("01000000"),
        "CMSG_STANDSTATECHANGE body: one u32 animState (Misc.cpp:35-38)"
    );
    // The chat-type consts feeding CMSG_MESSAGECHAT's type field (VERIFIED vmangos
    // `SharedDefines.h:1194..1202`, the 5875 band).
    assert_eq!(messages::CHAT_TYPE_SAY, 0x0, "ChatMsg::CHAT_MSG_SAY");
    assert_eq!(messages::CHAT_TYPE_YELL, 0x5, "ChatMsg::CHAT_MSG_YELL");
    assert_eq!(
        messages::CHAT_TYPE_WHISPER,
        0x6,
        "ChatMsg::CHAT_MSG_WHISPER"
    );
    assert_eq!(messages::CHAT_TYPE_EMOTE, 0x8, "ChatMsg::CHAT_MSG_EMOTE");
    // /yell and /emote share `messagechat`'s body shape, only the type field differs.
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_YELL, 7, "for the horde"),
        hx("0500000007000000666f722074686520686f72646500"),
        "CMSG_MESSAGECHAT (yell) body"
    );
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_EMOTE, 7, "dances"),
        hx("080000000700000064616e63657300"),
        "CMSG_MESSAGECHAT (emote) body"
    );
    // A Horde say speaks Orcish (0x1) in the language field — hardcoded Common here once made
    // vmangos drop every Horde character's sends at the `KnowsLanguage` gate, dot-commands
    // included (only an SMSG_NOTIFICATION came back).
    assert_eq!(
        messages::messagechat(
            messages::CHAT_TYPE_SAY,
            messages::LANGUAGE_ORCISH,
            "for the horde"
        ),
        hx("0000000001000000666f722074686520686f72646500"),
        "CMSG_MESSAGECHAT (say, Orcish) body"
    );
    // CMSG_MESSAGECHAT (whisper): type + language, then the target name, then the message — both
    // NUL-terminated C-strings, target *before* message (VERIFIED vmangos
    // `Server/Packets/Chat.cpp:3-12`, `ChatMessage::ReadFromWorldPacket`).
    assert_eq!(
        messages::messagechat_whisper(7, "Bob", "hi there"),
        hx("0600000007000000426f6200686920746865726500"),
        "CMSG_MESSAGECHAT (whisper) body"
    );
    // The full 8-byte LE guid body, shared by CMSG_PLAYER_LOGIN / CMSG_SET_ACTIVE_MOVER /
    // CMSG_SET_SELECTION (all read `recv_data >> guid` server-side — a raw uint64, not a packed guid).
    assert_eq!(
        messages::full_guid(0x1234_5678_9abc_def0),
        hx("f0debc9a78563412"),
        "full guid body"
    );
    // CMSG_SET_SELECTION opcode value (317 / 0x013D) — verified vmangos `Opcodes_1_12_1.h`.
    assert_eq!(
        messages::opcode::CMSG_SET_SELECTION,
        0x013D,
        "CMSG_SET_SELECTION opcode"
    );
    // CMSG_INSPECT (276 / 0x0114) — verified vmangos `Opcodes_1_12_1.h`; body is the same raw
    // 8-byte guid (`WorldPackets::Misc::Inspect`), NOT a packed guid. Decision 0631.
    assert_eq!(
        messages::opcode::CMSG_INSPECT,
        0x0114,
        "CMSG_INSPECT opcode"
    );
    assert_eq!(
        messages::full_guid(0x1234_5678_9abc_def0),
        hx("f0debc9a78563412"),
        "CMSG_INSPECT body (a raw guid, same shape as full_guid)"
    );
    let mi = MovementInfo {
        flags: 0x1,
        timestamp: 0x0102_0304,
        position: Vector3d {
            x: -8949.95,
            y: -132.493,
            z: 83.5312,
        },
        orientation: 1.25,
        transport: None,
        pitch: 0.0,
        fall_time: 0,
        jump: None,
    };
    assert_eq!(
        messages::movement(&mi),
        hx("0100000004030201cdd70bc6357e04c3f90fa7420000a03f00000000"),
        "MSG_MOVE_* body"
    );
    assert_eq!(
        messages::teleport_ack(0x1234_5678_9abc_def0, 7, 0x0102_0304),
        hx("f0debc9a785634120700000004030201"),
        "MSG_MOVE_TELEPORT_ACK body"
    );
}

/// The full sendable `CMSG_MESSAGECHAT` type set beyond the original four (decision 0288 phase 1) —
/// every one VERIFIED against vmangos `Handlers/ChatHandler.cpp`'s `HandleChatMessageOpcode` switch
/// (253-655): PARTY/RAID/GUILD/OFFICER/RAID_LEADER/RAID_WARNING/BATTLEGROUND(+LEADER)/AFK/DND all
/// share [`messages::messagechat`]'s plain shape (no target field); CHANNEL carries the channel name
/// the same way WHISPER carries its target (`messages::messagechat_channel`). Bytes hand-computed
/// from the vmangos layout (`Server/Packets/Chat.cpp:3-12`), independent of the Rust builder.
#[test]
fn messagechat_sendable_types_golden() {
    assert_eq!(messages::CHAT_TYPE_PARTY, 0x1, "ChatMsg::CHAT_MSG_PARTY");
    assert_eq!(messages::CHAT_TYPE_RAID, 0x2, "ChatMsg::CHAT_MSG_RAID");
    assert_eq!(messages::CHAT_TYPE_GUILD, 0x3, "ChatMsg::CHAT_MSG_GUILD");
    assert_eq!(
        messages::CHAT_TYPE_OFFICER,
        0x4,
        "ChatMsg::CHAT_MSG_OFFICER"
    );
    assert_eq!(
        messages::CHAT_TYPE_CHANNEL,
        0xE,
        "ChatMsg::CHAT_MSG_CHANNEL"
    );
    assert_eq!(messages::CHAT_TYPE_AFK, 0x14, "ChatMsg::CHAT_MSG_AFK");
    assert_eq!(messages::CHAT_TYPE_DND, 0x15, "ChatMsg::CHAT_MSG_DND");
    assert_eq!(
        messages::CHAT_TYPE_RAID_LEADER,
        0x57,
        "ChatMsg::CHAT_MSG_RAID_LEADER"
    );
    assert_eq!(
        messages::CHAT_TYPE_RAID_WARNING,
        0x58,
        "ChatMsg::CHAT_MSG_RAID_WARNING"
    );
    assert_eq!(
        messages::CHAT_TYPE_BATTLEGROUND,
        0x5C,
        "ChatMsg::CHAT_MSG_BATTLEGROUND"
    );
    assert_eq!(
        messages::CHAT_TYPE_BATTLEGROUND_LEADER,
        0x5D,
        "ChatMsg::CHAT_MSG_BATTLEGROUND_LEADER"
    );

    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_PARTY, 7, "MT is icon 8"),
        hx("01000000070000004d542069732069636f6e203800"),
        "CMSG_MESSAGECHAT (party) body"
    );
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_RAID, 7, "form up"),
        hx("0200000007000000666f726d20757000"),
        "CMSG_MESSAGECHAT (raid) body"
    );
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_GUILD, 7, "hi guild"),
        hx("03000000070000006869206775696c6400"),
        "CMSG_MESSAGECHAT (guild) body"
    );
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_OFFICER, 7, "officers only"),
        hx("04000000070000006f66666963657273206f6e6c7900"),
        "CMSG_MESSAGECHAT (officer) body"
    );
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_RAID_LEADER, 7, "pull in 5"),
        hx("570000000700000070756c6c20696e203500"),
        "CMSG_MESSAGECHAT (raid leader) body"
    );
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_RAID_WARNING, 7, "BLOODLUST NOW"),
        hx("5800000007000000424c4f4f444c555354204e4f5700"),
        "CMSG_MESSAGECHAT (raid warning) body"
    );
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_BATTLEGROUND, 7, "defend flag"),
        hx("5c00000007000000646566656e6420666c616700"),
        "CMSG_MESSAGECHAT (battleground) body"
    );
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_BATTLEGROUND_LEADER, 7, "push mid"),
        hx("5d0000000700000070757368206d696400"),
        "CMSG_MESSAGECHAT (battleground leader) body"
    );
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_AFK, 7, "be back soon"),
        hx("14000000070000006265206261636b20736f6f6e00"),
        "CMSG_MESSAGECHAT (afk) body"
    );
    // A bare `/afk` (no message) still needs the NUL-terminated empty string, not a truncated body.
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_AFK, 7, ""),
        hx("140000000700000000"),
        "CMSG_MESSAGECHAT (afk, empty message) body"
    );
    assert_eq!(
        messages::messagechat(messages::CHAT_TYPE_DND, 7, "do not disturb"),
        hx("1500000007000000646f206e6f74206469737475726200"),
        "CMSG_MESSAGECHAT (dnd) body"
    );
    // CHANNEL: type + language, then the channel name, then the message — the same
    // target-before-message shape as whisper (vmangos's `whisperTargetOrChannel` union).
    assert_eq!(
        messages::messagechat_channel(0, "General", "wtb boar livers"),
        hx("0e0000000000000047656e6572616c0077746220626f6172206c697665727300"),
        "CMSG_MESSAGECHAT (channel) body"
    );
    // The generic builder both named wrappers are thin shells over: `target: None` matches
    // `messagechat`, `target: Some(_)` matches `messagechat_whisper`/`messagechat_channel`.
    assert_eq!(
        messages::messagechat_kind(messages::CHAT_TYPE_SAY, 7, None, "hi"),
        messages::messagechat(messages::CHAT_TYPE_SAY, 7, "hi"),
        "messagechat_kind(None) == messagechat"
    );
    assert_eq!(
        messages::messagechat_kind(messages::CHAT_TYPE_WHISPER, 7, Some("Bob"), "hi"),
        messages::messagechat_whisper(7, "Bob", "hi"),
        "messagechat_kind(Some) == messagechat_whisper"
    );
}

/// **The addon broadcast body** (`SendAddonMessage`, decision 1235) — `CMSG_MESSAGECHAT` on one of
/// four ordinary lanes with the `LANG_ADDON` sentinel in the language field. Bytes hand-computed
/// from the layout, independent of the Rust builder.
///
/// VERIFIED in `WoW.exe` (5875) — wow-re `system/ui/scratch/addon-chat-law.md` §5, the binding
/// `0x49f920`: opcode `0x95` (`0x49facf`), u32 chat type (`0x49fad8`), u32 language from
/// `or ebx,-0x1` (`0x49fab9`) written at `0x49fae1`, then the message CString (`0x49faf0`). There
/// is **no prefix field and no target field on the wire** — the prefix is glued to the message
/// with a literal TAB (`_snprintf(dst, 0x800, "%s\t%s", …)` at `0x49f9b3`, format `0x844b5c`, raw
/// `25 73 09 25 73 00`) and the pair rides as one C-string. The far client splits on the FIRST tab
/// (`0x49a8d0`).
///
/// Corroborated end-to-end against a live vmangos by `examples/addon_chat_probe` (decision 1029):
/// the sentinel comes back intact and the `0x09` survives (addon chat skips `SanitizeChatMessage`,
/// `Handlers/ChatHandler.cpp:49`).
#[test]
fn addon_message_bodies_golden() {
    // `LANG_ADDON` — the whole discriminator. Not a tongue, and never rewritten by the server.
    assert_eq!(
        messages::LANGUAGE_ADDON,
        0xFFFF_FFFF,
        "LANG_ADDON (vmangos SharedDefines.h:270)"
    );

    // oRA2 `Core.lua:563`, verbatim: SendAddonMessage("CTRA", msg, "RAID") — the corpus's one
    // live caller of this verb.
    //   02 00 00 00                          CHAT_MSG_RAID
    //   ff ff ff ff                          LANG_ADDON
    //   43 54 52 41                          "CTRA"
    //   09                                   TAB — the prefix/message separator
    //   73 74 61 74 75 73                    "status"
    //   00                                   the message C-string's NUL
    assert_eq!(
        messages::messagechat(
            messages::CHAT_TYPE_RAID,
            messages::LANGUAGE_ADDON,
            "CTRA\tstatus"
        ),
        hx("02000000ffffffff435452410973746174757300"),
        "CMSG_MESSAGECHAT (addon, RAID) body"
    );

    // The other three lanes of the client's four-value whitelist (`0x49fa3f`-`0x49fa4e`). Only the
    // type byte moves; the sentinel and the tab-composed payload are identical.
    assert_eq!(
        messages::messagechat(
            messages::CHAT_TYPE_PARTY,
            messages::LANGUAGE_ADDON,
            "oRA\thello"
        ),
        hx("01000000ffffffff6f52410968656c6c6f00"),
        "CMSG_MESSAGECHAT (addon, PARTY) body"
    );
    assert_eq!(
        messages::messagechat(
            messages::CHAT_TYPE_GUILD,
            messages::LANGUAGE_ADDON,
            "oRA\thello"
        ),
        hx("03000000ffffffff6f52410968656c6c6f00"),
        "CMSG_MESSAGECHAT (addon, GUILD) body"
    );
    assert_eq!(
        messages::messagechat(
            messages::CHAT_TYPE_BATTLEGROUND,
            messages::LANGUAGE_ADDON,
            "oRA\thello"
        ),
        hx("5c000000ffffffff6f52410968656c6c6f00"),
        "CMSG_MESSAGECHAT (addon, BATTLEGROUND) body"
    );

    // AceEvent-2.0.lua's own line — `SendAddonMessage("LOOT_OPENED", "", "RAID")`, the call
    // replicated in 24 of the 218 corpus addons. **The empty message still carries its tab**: the
    // composition is unconditional, so the payload is `"LOOT_OPENED\t"` and the body ends TAB, NUL.
    // A builder that "helpfully" dropped a trailing separator would make the far client read
    // prefix `"LOOT_OPENED"` with no message at all — which is the same thing, but only by
    // accident of the receiver's no-tab rule; the bytes must still match the reference's.
    assert_eq!(
        messages::messagechat(
            messages::CHAT_TYPE_RAID,
            messages::LANGUAGE_ADDON,
            "LOOT_OPENED\t"
        ),
        hx("02000000ffffffff4c4f4f545f4f50454e45440900"),
        "CMSG_MESSAGECHAT (addon, empty message keeps its TAB) body"
    );

    // An addon line is byte-identical to ordinary speech on the same lane EXCEPT the language
    // field — the property the receive-side gate rests on, pinned so a future refactor cannot
    // quietly reintroduce a tongue here.
    let addon = messages::messagechat(messages::CHAT_TYPE_PARTY, messages::LANGUAGE_ADDON, "x\ty");
    let speech =
        messages::messagechat(messages::CHAT_TYPE_PARTY, messages::LANGUAGE_COMMON, "x\ty");
    assert_eq!(addon.len(), speech.len(), "same shape");
    assert_eq!(addon[0..4], speech[0..4], "same chat type");
    assert_eq!(addon[8..], speech[8..], "same payload");
    assert_eq!(&addon[4..8], &[0xFF, 0xFF, 0xFF, 0xFF], "LANG_ADDON");
    assert_eq!(&speech[4..8], &[0x07, 0x00, 0x00, 0x00], "LANG_COMMON");
}

/// The channel wire family's CMSG bodies (decision 0288 phase 1) — join/leave/list + the full
/// moderation set, ALL verified vmangos `Server/Packets/Channel.cpp` (every `ReadFromWorldPacket`
/// there is a channel-name cstring, optionally followed by a second cstring: a password or a target
/// player name). Bytes hand-computed from that layout, independent of the Rust builder.
#[test]
fn channel_client_bodies_golden() {
    assert_eq!(
        messages::join_channel("General", ""),
        hx("47656e6572616c0000"),
        "CMSG_JOIN_CHANNEL body (no password)"
    );
    assert_eq!(
        messages::join_channel("Secret", "hunter2"),
        hx("5365637265740068756e7465723200"),
        "CMSG_JOIN_CHANNEL body (with password)"
    );
    assert_eq!(
        messages::leave_channel("General"),
        hx("47656e6572616c00"),
        "CMSG_LEAVE_CHANNEL body"
    );
    assert_eq!(
        messages::channel_list("Trade"),
        hx("547261646500"),
        "CMSG_CHANNEL_LIST body"
    );
    assert_eq!(
        messages::channel_password("General", "hunter2"),
        hx("47656e6572616c0068756e7465723200"),
        "CMSG_CHANNEL_PASSWORD body"
    );
    assert_eq!(
        messages::channel_set_owner("General", "Bob"),
        hx("47656e6572616c00426f6200"),
        "CMSG_CHANNEL_SET_OWNER body"
    );
    assert_eq!(
        messages::channel_owner("General"),
        hx("47656e6572616c00"),
        "CMSG_CHANNEL_OWNER body"
    );
    assert_eq!(
        messages::channel_moderator("General", "Bob"),
        hx("47656e6572616c00426f6200"),
        "CMSG_CHANNEL_MODERATOR body"
    );
    assert_eq!(
        messages::channel_unmoderator("General", "Bob"),
        hx("47656e6572616c00426f6200"),
        "CMSG_CHANNEL_UNMODERATOR body"
    );
    assert_eq!(
        messages::channel_mute("General", "Bob"),
        hx("47656e6572616c00426f6200"),
        "CMSG_CHANNEL_MUTE body"
    );
    assert_eq!(
        messages::channel_unmute("General", "Bob"),
        hx("47656e6572616c00426f6200"),
        "CMSG_CHANNEL_UNMUTE body"
    );
    assert_eq!(
        messages::channel_invite("General", "Bob"),
        hx("47656e6572616c00426f6200"),
        "CMSG_CHANNEL_INVITE body"
    );
    assert_eq!(
        messages::channel_kick("General", "Bob"),
        hx("47656e6572616c00426f6200"),
        "CMSG_CHANNEL_KICK body"
    );
    assert_eq!(
        messages::channel_ban("General", "Bob"),
        hx("47656e6572616c00426f6200"),
        "CMSG_CHANNEL_BAN body"
    );
    assert_eq!(
        messages::channel_unban("General", "Bob"),
        hx("47656e6572616c00426f6200"),
        "CMSG_CHANNEL_UNBAN body"
    );
    assert_eq!(
        messages::channel_announcements("General"),
        hx("47656e6572616c00"),
        "CMSG_CHANNEL_ANNOUNCEMENTS body"
    );
    assert_eq!(
        messages::channel_moderate("General"),
        hx("47656e6572616c00"),
        "CMSG_CHANNEL_MODERATE body"
    );

    // Every opcode in the family (VERIFIED vmangos `Server/Protocol/Opcodes_1_12_1.h:154-171`,
    // decimal 151-168).
    assert_eq!(messages::opcode::CMSG_JOIN_CHANNEL, 0x0097);
    assert_eq!(messages::opcode::CMSG_LEAVE_CHANNEL, 0x0098);
    assert_eq!(messages::opcode::SMSG_CHANNEL_NOTIFY, 0x0099);
    assert_eq!(messages::opcode::CMSG_CHANNEL_LIST, 0x009A);
    assert_eq!(messages::opcode::SMSG_CHANNEL_LIST, 0x009B);
    assert_eq!(messages::opcode::CMSG_CHANNEL_PASSWORD, 0x009C);
    assert_eq!(messages::opcode::CMSG_CHANNEL_SET_OWNER, 0x009D);
    assert_eq!(messages::opcode::CMSG_CHANNEL_OWNER, 0x009E);
    assert_eq!(messages::opcode::CMSG_CHANNEL_MODERATOR, 0x009F);
    assert_eq!(messages::opcode::CMSG_CHANNEL_UNMODERATOR, 0x00A0);
    assert_eq!(messages::opcode::CMSG_CHANNEL_MUTE, 0x00A1);
    assert_eq!(messages::opcode::CMSG_CHANNEL_UNMUTE, 0x00A2);
    assert_eq!(messages::opcode::CMSG_CHANNEL_INVITE, 0x00A3);
    assert_eq!(messages::opcode::CMSG_CHANNEL_KICK, 0x00A4);
    assert_eq!(messages::opcode::CMSG_CHANNEL_BAN, 0x00A5);
    assert_eq!(messages::opcode::CMSG_CHANNEL_UNBAN, 0x00A6);
    assert_eq!(messages::opcode::CMSG_CHANNEL_ANNOUNCEMENTS, 0x00A7);
    assert_eq!(messages::opcode::CMSG_CHANNEL_MODERATE, 0x00A8);
}

/// The remaining decision-0288 phase-1 small bodies: `CMSG_CHAT_IGNORED` (reuses
/// [`messages::full_guid`] — VERIFIED vmangos `WorldPackets::Misc::ChatIgnored::
/// ReadFromWorldPacket`, `Server/Packets/Misc.cpp:127-130`, a raw un-packed guid), `CMSG_PLAYED_TIME`
/// (empty), and `MSG_RANDOM_ROLL`'s client→server request shape (VERIFIED vmangos
/// `WorldPackets::Group::RandomRoll::ReadFromWorldPacket`, `Server/Packets/Group.cpp:39-43`).
#[test]
fn chat_ignored_played_time_random_roll_golden() {
    assert_eq!(
        messages::full_guid(0x1234_5678_9abc_def0),
        hx("f0debc9a78563412"),
        "CMSG_CHAT_IGNORED body (a raw guid, same shape as full_guid)"
    );
    assert_eq!(messages::opcode::CMSG_CHAT_IGNORED, 0x0225);

    assert_eq!(
        messages::played_time(),
        Vec::<u8>::new(),
        "CMSG_PLAYED_TIME body: empty"
    );
    assert_eq!(messages::opcode::CMSG_PLAYED_TIME, 0x01CC);
    assert_eq!(messages::opcode::SMSG_PLAYED_TIME, 0x01CD);

    assert_eq!(
        messages::random_roll(1, 100),
        hx("0100000064000000"),
        "MSG_RANDOM_ROLL (request) body"
    );
    assert_eq!(messages::opcode::MSG_RANDOM_ROLL, 0x01FB);
}

#[test]
fn name_and_creature_query_roundtrip() {
    // CMSG_CREATURE_QUERY body: entry u32 + full 8-byte guid (vmangos QueryCreature::ReadFromWorldPacket).
    assert_eq!(
        messages::creature_query(69, 0x1234_5678_9abc_def0),
        hx("45000000f0debc9a78563412"),
        "CMSG_CREATURE_QUERY body"
    );

    // SMSG_NAME_QUERY_RESPONSE: guid, name, realm (empty), race/gender/class u32s (vmangos
    // NameQueryResponse::AppendBodyTo, 1.12.1 includes the realm string).
    let body = hx("070000000000000042656e696c6c610000010000000000000001000000");
    match messages::parse_server(messages::opcode::SMSG_NAME_QUERY_RESPONSE, &body).unwrap() {
        ServerPacket::NameQueryResponse {
            guid,
            name,
            race,
            gender,
            class,
        } => {
            assert_eq!(guid, 7);
            assert_eq!(name, "Benilla");
            assert_eq!((race, gender, class), (1, 0, 1));
        }
        _ => panic!("name query response"),
    }

    // SMSG_CREATURE_QUERY_RESPONSE hit: entry, name, 3 empty names, subname, the 7-u32 tail
    // (type_flags, TYPE — the TAB critter filter's input — FAMILY, RANK, unk, pet_spell_id,
    // display_id), 2 u8 tail. Family and rank are given DIFFERENT non-zero values on purpose:
    // they are adjacent dwords, so a one-column slip between them reads as plausible data and
    // shows up only as a pet whose level line names the wrong beast (decision 1062).
    let body = hx(concat!(
        "45000000",
        "596f756e6720576f6c6600",   // "Young Wolf"
        "000000",                   // name2..4 empty
        "5465737400",               // subname "Test"
        "10000000",                 // type_flags = 0x10 (hide-faction-tooltip)
        "01000000",                 // type = 1 (Beast)
        "01000000",                 // pet_family = 1 (Wolf)
        "02000000",                 // rank = 2 (rare elite)
        "000000000000000000000000", // unk, pet_spell_list_id, display_id
        "0101"                      // civilian, racial_leader
    ));
    match messages::parse_server(messages::opcode::SMSG_CREATURE_QUERY_RESPONSE, &body).unwrap() {
        ServerPacket::CreatureQueryResponse { entry, info } => {
            assert_eq!(entry, 69);
            assert_eq!(
                info,
                Some(benilla_protocol::messages::CreatureQueryInfo {
                    name: "Young Wolf".into(),
                    subname: "Test".into(),
                    creature_type: 1,
                    pet_family: 1,
                    rank: 2,
                    type_flags: 0x10,
                    civilian: true,
                    racial_leader: true,
                })
            );
        }
        _ => panic!("creature query response"),
    }

    // Miss: the lone entry echoed with the top bit set (vmangos "NO CREATURE INFO" branch).
    let body = hx("d2040080");
    match messages::parse_server(messages::opcode::SMSG_CREATURE_QUERY_RESPONSE, &body).unwrap() {
        ServerPacket::CreatureQueryResponse { entry, info } => {
            assert_eq!(entry, 1234);
            assert_eq!(info, None);
        }
        _ => panic!("creature query miss"),
    }
}

/// The event layer's subname law: an EMPTY wire subname is NO subname (vmangos sends "" for
/// creatures without one; the real client renders no line for it — the nameplate builder's
/// verified shape, `0x608f50`). `Some("")` reaching a consumer painted an empty tooltip line
/// whose zero-extent chain slot spilled every later line below the plate.
#[test]
fn creature_query_empty_subname_decodes_to_none() {
    let body = |subname_hex: &str| {
        hx(&format!(
            "45000000{}000000{}{}",
            "4465707574792057696c6c656d00", // "Deputy Willem" (+3 empty names after)
            subname_hex,
            // type_flags, type=7 Humanoid, family..display_id (5 × u32), civilian, racial_leader
            "000000000700000000000000000000000000000000000000000000000000"
        ))
    };
    let parse = |b: &[u8]| {
        messages::parse_server(messages::opcode::SMSG_CREATURE_QUERY_RESPONSE, b).unwrap()
    };
    match decode(parse(&body("00"))).as_slice() {
        [SessionEvent::CreatureName {
            entry,
            name,
            subname,
            ..
        }] => {
            assert_eq!(*entry, 69);
            assert_eq!(name.as_deref(), Some("Deputy Willem"));
            assert_eq!(*subname, None, "an empty wire subname is NO subname");
        }
        other => panic!("expected one CreatureName event, got {other:?}"),
    }
    // Control: a real subname survives verbatim.
    match decode(parse(&body("5465737400"))).as_slice() {
        [SessionEvent::CreatureName { subname, .. }] => {
            assert_eq!(subname.as_deref(), Some("Test"));
        }
        other => panic!("expected one CreatureName event, got {other:?}"),
    }
}

/// The keepalive pair, byte-exact both ways: the `CMSG_PING` body is `{u32 sequence, u32 lastRtt}`
/// LE (VERIFIED wow-re net W1 `SendPing 0x537e10` + vmangos `_HandlePing`'s read order), and the
/// `SMSG_PONG` echo (`{u32 sequence}`) parses and decodes to the event the io layer times against
/// its ping clock. A new wire body never lands without a golden (method).
#[test]
fn ping_body_golden_and_pong_roundtrip() {
    assert_eq!(
        messages::ping(1, 0),
        hx("0100000000000000"),
        "first ping of a connection: seq 1, no RTT yet"
    );
    assert_eq!(
        messages::ping(0x0A0B_0C0D, 23),
        hx("0d0c0b0a17000000"),
        "CMSG_PING body: sequence then lastRtt, both u32 LE"
    );
    let packet = messages::parse_server(messages::opcode::SMSG_PONG, &hx("0d0c0b0a")).unwrap();
    assert!(matches!(
        packet,
        ServerPacket::Pong {
            sequence: 0x0A0B_0C0D
        }
    ));
    match decode(packet).as_slice() {
        [SessionEvent::Pong {
            sequence: 0x0A0B_0C0D,
        }] => {}
        other => panic!("expected one Pong event, got {other:?}"),
    }
}
