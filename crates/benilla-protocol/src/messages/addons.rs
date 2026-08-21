//! The **addon-info block** at the tail of `CMSG_AUTH_SESSION` — what the client tells the world
//! server about its *secure* addons (decision 1497).
//!
//! Every 1.12.1 logon carries one of these. It is not optional decoration: it is the last field of
//! the auth packet, and a server reads it before the session exists. benilla used to send a
//! `decompressed_size = 0` + zlib-of-nothing stub, which **no real client can produce** — and
//! cmangos-classic kicks the session for it (B277). See [`addon_block`] for the shape and
//! [`STOCK_SECURE_ADDONS`] for what a stock install actually sends.
//!
//! ## Where the shape comes from
//!
//! VERIFIED in `WoW.exe` (5875) — wow-5875-re `system/net/scratch/cmsg-auth-session-addon-block.md`.
//! The writer is `0x51d910`, called once, from `HandleAuthChallenge` (`0x5b4143`) straight after the
//! 20-byte SHA-1 proof. It appends:
//!
//! ```text
//! u32   uncompressed_size    ; byte length of the buffer below
//! u8[]  zlib stream          ; RFC1950 framing (deflateInit_ windowBits +15), runs to end of packet
//! ```
//!
//! and the uncompressed buffer is a **bare concatenation** — no leading count, no trailer —
//! of one record per secure addon:
//!
//! ```text
//! CString name          ; NUL-terminated
//! u8      flags         ; byte 0 of the addon's own .pub signature file
//! u32     modulus_crc   ; CRC-32 of the 256 modulus bytes (.pub bytes 1..=256)
//! u32     url_crc       ; CRC-32 of the addon's .url string; 0 when it has none
//! ```
//!
//! **Which** addons: exactly those whose `.toc` declares `## Secure:` non-zero — enable/disable
//! state is not consulted. On a stock install that is precisely the twelve `Blizzard_*` built-ins
//! and nothing else, because no third-party 1.12 addon declares `## Secure`. A client with *zero*
//! secure addons appends **nothing at all** (not a zero dword) — the writer returns before the
//! local store exists.
//!
//! ## Why the emulators disagree about it, and why it matters
//!
//! Both vmangos and cmangos reject `size == 0` with the same comment ("empty addon packet … can't
//! be received from real client") — that judgement is *correct*, and matched the binary all along.
//! What differs is the consequence:
//!
//! - vmangos `WorldSocket.cpp:447` — `if (BuildAddonPacket(...)) SendPacket(addonPacket);`. A
//!   rejection just means no `SMSG_ADDON_INFO` goes back; the session lives. That is the only
//!   reason benilla's stub ever worked.
//! - cmangos-classic `WorldSocket.cpp:562` — `if (!ReadAddonInfo(...)) { … "sent bad addon info.
//!   Kicking."; return false; }`. Same rejection, session killed. Not config-gated: the check is in
//!   the anticheat module *and* in `NullSessionAnticheat`, so it runs with the anticheat off.
//!
//! cmangos's two readers also disagree with each other on field order — its anticheat module reads
//! `name, u8, u32, u32` (right, matching the binary) and its `NullSessionAnticheat` reads
//! `name, u32, u32, u8` (wrong, but the same nine bytes, so it parses without throwing).

/// CRC-32 of the stock Blizzard public-key modulus — what a stock, unmodified addon's `.pub`
/// hashes to, and the value every emulator calls the "standard addon CRC". A server that sees it
/// knows it need not push the 256-byte modulus back in `SMSG_ADDON_INFO`.
///
/// VERIFIED: `zlib.crc32` of the 256 modulus bytes in this install's `Blizzard_*.pub` files, which
/// are byte-identical to the `modulus`/`tdata` array carried in vmangos and cmangos alike.
pub const STANDARD_MODULUS_CRC: u32 = 0x4C1C_776D;

/// One record in the addon block — a secure addon as the client describes it to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecureAddon<'a> {
    /// The addon's folder name (`Blizzard_AuctionUI`), sent as a `CString`.
    pub name: &'a str,
    /// Byte 0 of the addon's `.pub` file — `1` for a stock signature, `0` when the client found no
    /// usable `.pub`. A server may write a different byte back (`SMSG_ADDON_INFO`), which the real
    /// client persists into the `.pub` and echoes next logon; cmangos's anticheat uses exactly that
    /// channel to stamp a per-install fingerprint across four of the twelve.
    pub flags: u8,
    /// CRC-32 of the `.pub`'s 256 modulus bytes — [`STANDARD_MODULUS_CRC`] for a stock signature,
    /// `0` when `flags` is `0`.
    pub modulus_crc: u32,
    /// CRC-32 of the addon's `.url` string. `0` for every stock addon (none ship one).
    pub url_crc: u32,
}

impl SecureAddon<'_> {
    /// A stock, signed Blizzard built-in: enabled signature, standard modulus, no URL. The shape
    /// all twelve take on an unmodified install.
    const fn stock(name: &str) -> SecureAddon<'_> {
        SecureAddon {
            name,
            flags: 1,
            modulus_crc: STANDARD_MODULUS_CRC,
            url_crc: 0,
        }
    }
}

/// The twelve `Blizzard_*` built-ins a stock 1.12.1 install reports, in the order the client sends
/// them (ascending ASCII of the folder name).
///
/// VERIFIED against a real 1.12.1.5875 client's `CMSG_AUTH_SESSION` captured against a live
/// Blizzard server in 2006 (wow-5875-re `system/net/scratch/cmsg-auth-session-addon-block.md`):
/// 342 uncompressed bytes, twelve records, every one `flags = 1`, `modulus_crc =`
/// [`STANDARD_MODULUS_CRC`], `url_crc = 0`, and nothing left over — the arithmetic proof that the
/// buffer carries no count and no trailer.
///
/// benilla sends this list rather than one derived from the install's own `Interface/AddOns`
/// folder, and the two agree on every stock install: the client's rule is "`## Secure:` non-zero",
/// which no third-party 1.12 addon sets, and benilla's own addons (loaded from
/// `benilla-config/AddOns`) are third-party by construction. Reading each `.pub` off the player's
/// chain would only differ on an install whose signature files have been altered — see 1497.
pub const STOCK_SECURE_ADDONS: [SecureAddon<'static>; 12] = [
    SecureAddon::stock("Blizzard_AuctionUI"),
    SecureAddon::stock("Blizzard_BattlefieldMinimap"),
    SecureAddon::stock("Blizzard_BindingUI"),
    SecureAddon::stock("Blizzard_CombatText"),
    SecureAddon::stock("Blizzard_CraftUI"),
    SecureAddon::stock("Blizzard_GMSurveyUI"),
    SecureAddon::stock("Blizzard_InspectUI"),
    SecureAddon::stock("Blizzard_MacroUI"),
    SecureAddon::stock("Blizzard_RaidUI"),
    SecureAddon::stock("Blizzard_TalentUI"),
    SecureAddon::stock("Blizzard_TradeSkillUI"),
    SecureAddon::stock("Blizzard_TrainerUI"),
];

/// The **uncompressed** addon buffer: one record per addon, concatenated, nothing else.
pub fn addon_block(addons: &[SecureAddon]) -> Vec<u8> {
    let mut out = Vec::with_capacity(addons.iter().map(|a| a.name.len() + 10).sum());
    for addon in addons {
        out.extend_from_slice(addon.name.as_bytes());
        out.push(0);
        out.push(addon.flags);
        out.extend_from_slice(&addon.modulus_crc.to_le_bytes());
        out.extend_from_slice(&addon.url_crc.to_le_bytes());
    }
    out
}

/// The block as it rides the wire: `u32` uncompressed size + the zlib stream, or **nothing at all**
/// when there are no secure addons (what the real client does — it never emits a zero size).
pub fn addon_tail(addons: &[SecureAddon]) -> Vec<u8> {
    if addons.is_empty() {
        return Vec::new();
    }
    let plain = addon_block(addons);
    let mut out = Vec::with_capacity(plain.len() / 2 + 8);
    out.extend_from_slice(&(plain.len() as u32).to_le_bytes());
    let mut encoder =
        flate2::write::ZlibEncoder::new(out, flate2::Compression::default() /* level 6 */);
    std::io::Write::write_all(&mut encoder, &plain).expect("zlib write to a Vec cannot fail");
    encoder.finish().expect("zlib finish on a Vec cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The retail ground truth: 342 uncompressed bytes, consumed exactly by twelve records.
    #[test]
    fn stock_block_matches_the_retail_capture() {
        let plain = addon_block(&STOCK_SECURE_ADDONS);
        assert_eq!(plain.len(), 342, "retail sent 342 uncompressed bytes");

        // Walk it back the way a server does — name, flags, modulus crc, url crc — and require it
        // to land exactly on the end. That is what proves there is no count and no trailer.
        let mut rest = &plain[..];
        let mut seen = Vec::new();
        while !rest.is_empty() {
            let nul = rest
                .iter()
                .position(|&b| b == 0)
                .expect("record has a name");
            let name = std::str::from_utf8(&rest[..nul]).expect("name is utf-8");
            rest = &rest[nul + 1..];
            let (flags, tail) = rest.split_first().expect("record has flags");
            let modulus_crc = u32::from_le_bytes(tail[0..4].try_into().unwrap());
            let url_crc = u32::from_le_bytes(tail[4..8].try_into().unwrap());
            assert_eq!(*flags, 1, "{name} is a stock signed addon");
            assert_eq!(modulus_crc, STANDARD_MODULUS_CRC, "{name} modulus crc");
            assert_eq!(url_crc, 0, "{name} ships no .url");
            seen.push(name.to_string());
            rest = &tail[8..];
        }
        assert_eq!(seen.len(), 12);
        assert!(seen.iter().all(|n| n.starts_with("Blizzard_")));
        let mut sorted = seen.clone();
        sorted.sort();
        assert_eq!(
            seen, sorted,
            "the client sends them in ascending ASCII order"
        );
    }

    /// The wire tail: size prefix, zlib framing, and the retail compressed length. `flate2` at its
    /// default level reproduces the 2006 client's 130 bytes byte-for-byte — the real client used
    /// `Z_DEFAULT_COMPRESSION` through the same zlib deflate.
    #[test]
    fn stock_tail_is_size_plus_zlib() {
        let tail = addon_tail(&STOCK_SECURE_ADDONS);
        assert_eq!(
            u32::from_le_bytes(tail[0..4].try_into().unwrap()),
            342,
            "size prefix is the uncompressed length"
        );
        assert_eq!(tail[4], 0x78, "RFC1950 zlib framing, not raw deflate");
        assert_eq!(tail.len(), 4 + 130, "retail compressed to 130 bytes");
    }

    /// Zero secure addons appends *nothing* — never a zero size, which no real client can emit and
    /// which cmangos-classic kicks for.
    #[test]
    fn no_secure_addons_appends_nothing() {
        assert!(addon_tail(&[]).is_empty());
    }
}
