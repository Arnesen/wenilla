//! Emote audio data (decision 0070 slice 4): **EmotesText** (the `/wave` command table) joined
//! to **EmotesTextSound** (the race/sex voice kits) and **Emotes** (anim emotes + their
//! `EventSoundID`).
//!
//! Layouts — VERIFIED against build 5875 (headers + row decodes, 2026-07-02):
//! - `EmotesText` **169 × 19 × 76 B**: `ID, Name(str, e.g. "WAVE"), EmoteID (→Emotes),
//!   EmoteText[16]`. Spot-check: WAVE = id 101, emote 3.
//! - `EmotesTextSound` **418 × 5 × 20 B**: `ID, EmotesTextID, RaceID, SexID (0 male / 1 female),
//!   SoundID (→SoundEntries)`.
//! - `Emotes` **78 × 7 × 28 B**: `ID, SlashCommand(str), AnimID, EmoteFlags, EmoteSpecProc,
//!   EmoteSpecProcParam, EventSoundID (→SoundEntries)`. Spot-check row 2: ONESHOT_BOW, anim 66
//!   (VERIFIED against vmangos `SharedDefines.h`'s `Emote` enum: `EMOTE_ONESHOT_BOW = 2`).
//!
//! `AnimID` (column 2) feeds both the one-shot anim-emote path (`SMSG_EMOTE`'s `Emotes.dbc` id) and
//! the looping state-emote idle (`UNIT_NPC_EMOTESTATE`, the same id space) — [`EmoteSoundCatalog::anim`].
//!
//! `EmoteFlags` (column 3) feeds the **send-side posture-eligibility gate** — the only `EmoteFlags`
//! consumer, byte-verified at the real client's `CheckEmoteEligible` (`0x47db40`, called from
//! `DoEmote` `0x5ef560`): before `CMSG_TEXT_EMOTE` goes out, bit `0x0001` combined with a non-zero
//! stand-state aborts the *entire* emote (no packet, no anim — a seated `/bow` does nothing).
//! `wow-5875-re`'s `system/object-layer/scratch/emote-posture-gate.md` (commit `f9584b45`) is the
//! authority; [`EmoteSoundCatalog::emote_flags`] promotes the raw bits for `crate::chat`'s gate.

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};

/// The joined emote-audio tables.
pub struct EmoteSoundCatalog {
    /// Uppercased `EmotesText.Name` ("WAVE") → text-emote id — the `/command` send key.
    by_name: HashMap<String, u32>,
    /// text-emote id → `Emotes.dbc` id (the anim the emote plays; 0 = chat-only).
    text_emote: HashMap<u32, u32>,
    /// `(text-emote id, race, sex)` → voice kit.
    voice: HashMap<(u32, u32, u32), u32>,
    /// `Emotes.dbc` id → its `EventSoundID` kit (0 = none).
    event_sound: HashMap<u32, u32>,
    /// `Emotes.dbc` id → its `AnimID` (`AnimationData.dbc` id; 0 = none).
    anim: HashMap<u32, u32>,
    /// `Emotes.dbc` id → its `EmoteFlags` (the send-side posture-eligibility gate bits).
    emote_flags: HashMap<u32, u32>,
    /// `Emotes.dbc` id → its `EmoteSpecProc` (column 4). `2` marks a looping STATE emote whose
    /// `EventSoundID` the `$ESD` anim event rings (the client's `row[+0x10] == 2` gate at the
    /// `$ESD` handler `0x6239f0` — wow-re `sound/scratch/gather-sound-anim-events.md`).
    spec_proc: HashMap<u32, u32>,
}

impl EmoteSoundCatalog {
    /// Resolve a `/command` name (case-insensitive) to its text-emote id.
    pub fn text_id(&self, name: &str) -> Option<u32> {
        self.by_name.get(&name.to_ascii_uppercase()).copied()
    }

    /// The voice kit for a performer of the given race/sex (`sex`: 0 male, 1 female).
    pub fn voice(&self, text_id: u32, race: u32, sex: u32) -> Option<u32> {
        self.voice.get(&(text_id, race, sex)).copied()
    }

    /// The anim emote a text emote plays (0/none for chat-only emotes).
    pub fn text_emote(&self, text_id: u32) -> Option<u32> {
        self.text_emote.get(&text_id).copied().filter(|&e| e != 0)
    }

    /// An anim emote's event-sound kit.
    pub fn event_sound(&self, emote_id: u32) -> Option<u32> {
        self.event_sound.get(&emote_id).copied().filter(|&k| k != 0)
    }

    /// An `Emotes.dbc` id's `AnimID` (the `AnimationData.dbc` id it plays; `0`/absent = none).
    /// Shared by the one-shot anim-emote path (`SMSG_EMOTE`) and the looping state-emote idle
    /// (`UNIT_NPC_EMOTESTATE`) — both carry an `Emotes.dbc` id in the same id space.
    pub fn anim(&self, emote_id: u32) -> Option<u32> {
        self.anim.get(&emote_id).copied().filter(|&a| a != 0)
    }

    /// An `Emotes.dbc` id's raw `EmoteFlags` bits (`None` when the id isn't in the catalog; `0` is a
    /// real, meaningful value — "no gate bits set" — so unlike [`Self::anim`]/[`Self::event_sound`]
    /// it is *not* filtered out). Feeds `benilla::chat`'s send-side posture-eligibility gate — see
    /// the module doc.
    pub fn emote_flags(&self, emote_id: u32) -> Option<u32> {
        self.emote_flags.get(&emote_id).copied()
    }

    /// An `Emotes.dbc` id's `EmoteSpecProc` (`None` when the id isn't in the catalog; `0` is a real
    /// value, so not filtered). `2` = a looping state emote — the `$ESD` event-sound gate (see the
    /// field doc).
    pub fn spec_proc(&self, emote_id: u32) -> Option<u32> {
        self.spec_proc.get(&emote_id).copied()
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

fn schema(name: &str, n: usize, strings: &[usize]) -> Schema {
    let mut s = Schema::new(name);
    for i in 0..n {
        let ty = if strings.contains(&i) {
            FieldType::String
        } else {
            FieldType::UInt32
        };
        s.add_field(SchemaField::new(format!("f{i}"), ty));
    }
    s
}

/// Read the three tables off the patch chain.
pub fn load_emote_sound_catalog(chain: &mut Chain) -> Result<EmoteSoundCatalog> {
    let bytes = chain
        .read_file("DBFilesClient\\EmotesText.dbc")
        .context("reading EmotesText.dbc")?;
    let rs = parse(&bytes, schema("EmotesText", 19, &[1]), "EmotesText")?;
    let mut by_name = HashMap::with_capacity(rs.records().len());
    let mut text_emote = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        if let Some(name) = str_at(&rs, r, 1).filter(|n| !n.is_empty()) {
            by_name.insert(name.to_ascii_uppercase(), id);
        }
        text_emote.insert(id, u32_at(r, 2).unwrap_or(0));
    }

    let bytes = chain
        .read_file("DBFilesClient\\EmotesTextSound.dbc")
        .context("reading EmotesTextSound.dbc")?;
    let rs = parse(&bytes, schema("EmotesTextSound", 5, &[]), "EmotesTextSound")?;
    let mut voice = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let (Some(text), Some(race), Some(sex), Some(kit)) =
            (u32_at(r, 1), u32_at(r, 2), u32_at(r, 3), u32_at(r, 4))
        else {
            continue;
        };
        voice.insert((text, race, sex), kit);
    }

    let bytes = chain
        .read_file("DBFilesClient\\Emotes.dbc")
        .context("reading Emotes.dbc")?;
    let rs = parse(&bytes, schema("Emotes", 7, &[1]), "Emotes")?;
    let mut event_sound = HashMap::with_capacity(rs.records().len());
    let mut anim = HashMap::with_capacity(rs.records().len());
    let mut emote_flags = HashMap::with_capacity(rs.records().len());
    let mut spec_proc = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        if let Some(kit) = u32_at(r, 6) {
            event_sound.insert(id, kit);
        }
        if let Some(anim_id) = u32_at(r, 2) {
            anim.insert(id, anim_id);
        }
        if let Some(flags) = u32_at(r, 3) {
            emote_flags.insert(id, flags);
        }
        if let Some(proc) = u32_at(r, 4) {
            spec_proc.insert(id, proc);
        }
    }

    Ok(EmoteSoundCatalog {
        by_name,
        text_emote,
        voice,
        event_sound,
        anim,
        emote_flags,
        spec_proc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// The joins hold on real 5875 data: WAVE resolves by name to the byte-decoded id 101 with
    /// anim emote 3; some voice row exists for a human (race 1) male; the voice map carries the
    /// male/female split (the survey's sample rows pair sexes per race).
    #[test]
    fn real_emote_chain_resolves() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_emote_sound_catalog(&mut chain).expect("load emote catalog");
        assert_eq!(cat.text_id("wave"), Some(101), "case-insensitive by name");
        assert_eq!(cat.text_emote(101), Some(3), "WAVE plays anim emote 3");
        assert_eq!(cat.anim(2), Some(66), "ONESHOT_BOW (id 2) plays AnimID 66");
        // EmoteFlags for the director-verified posture-gate rows (emote-posture-gate.md §3), ids
        // from vmangos `SharedDefines.h`'s `Emote` enum: BOW=2, CHEER=4, LAUGH=11, RUDE=14,
        // APPLAUD=21, SALUTE=66.
        assert_eq!(cat.emote_flags(2), Some(0x4801), "ONESHOT_BOW EmoteFlags");
        assert_eq!(cat.emote_flags(4), Some(0x0800), "ONESHOT_CHEER EmoteFlags");
        assert_eq!(
            cat.emote_flags(11),
            Some(0x0980),
            "ONESHOT_LAUGH EmoteFlags"
        );
        assert_eq!(cat.emote_flags(14), Some(0x0001), "ONESHOT_RUDE EmoteFlags");
        assert_eq!(
            cat.emote_flags(21),
            Some(0x0000),
            "ONESHOT_APPLAUD EmoteFlags"
        );
        assert_eq!(
            cat.emote_flags(66),
            Some(0x0800),
            "ONESHOT_SALUTE EmoteFlags"
        );
        // The $ESD gathering chain (decision 0562): STATE_WORK_NOSHEATHE_MINING (233) is a
        // spec-proc-2 state emote carrying the MiningHit kit; its one-shot cousins carry proc 0.
        assert_eq!(cat.spec_proc(233), Some(2), "mining state EmoteSpecProc");
        assert_eq!(
            cat.event_sound(233),
            Some(3782),
            "mining state EventSoundID (SoundEntries \"Mining\")"
        );
        assert_eq!(cat.anim(233), Some(136), "mining state plays anim 136");
        assert_eq!(cat.spec_proc(2), Some(0), "ONESHOT_BOW EmoteSpecProc");
        assert!(
            cat.voice
                .keys()
                .any(|&(_, race, sex)| race == 1 && sex == 0),
            "human male voice rows exist"
        );
        assert!(
            cat.voice.keys().any(|&(_, _, sex)| sex == 1),
            "female voice rows exist"
        );
    }
}
