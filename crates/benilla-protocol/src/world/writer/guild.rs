//! The guild family's `WorldWriter` sends — the two cache asks, the invitation dance, the member
//! verbs, and rank administration.
//!
//! Three shapes of the family are worth knowing before calling any of these:
//!
//! - **Almost nothing is acked individually.** Promote, demote, remove, the leader handover, the
//!   MOTD, both notes and every rank verb answer with a fresh `SMSG_GUILD_ROSTER` (and, for the
//!   rank verbs, an `SMSG_GUILD_QUERY_RESPONSE` too) — the server re-sends the whole snapshot
//!   rather than a delta. A refusal instead comes back as `SMSG_GUILD_COMMAND_RESULT`. So the
//!   caller's model updates when the roster lands, never optimistically at the send.
//! - **Members are addressed by NAME here**, unlike the friend list's remove-by-guid: every verb
//!   below that targets a player takes the character name, and the server normalises its case.
//! - **Rank ids are 0-based with 0 = guild master**, and authority *decreases* as the id rises —
//!   [`WorldWriter::guild_promote`] moves a member to `rank - 1`.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Ask for a guild's public identity by id (`CMSG_GUILD_QUERY`) — name, rank names, tabard.
    /// The ask-once cache fill behind every "which guild is that?": a roster row, a `/who` hit and
    /// a guild chat line all reference a guild by id alone.
    pub fn guild_query(&mut self, guild_id: u32) -> Result<()> {
        self.send(opcode::CMSG_GUILD_QUERY, &messages::guild_query(guild_id))
    }

    /// Found a guild by name (`CMSG_GUILD_CREATE`). vmangos registers this opcode `STATUS_NEVER`:
    /// on a 1.12 realm founding runs through the charter/petition flow instead, so this send is
    /// here for completeness of the family and draws no reply.
    pub fn guild_create(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_GUILD_CREATE, &messages::guild_create(name))
    }

    /// Invite a character into our guild by name (`CMSG_GUILD_INVITE`). They get an
    /// `SMSG_GUILD_INVITE` popup; we get an `SMSG_GUILD_COMMAND_RESULT` only if it was refused
    /// (already guilded, ignoring us, wrong faction, no permission).
    pub fn guild_invite(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_GUILD_INVITE, &messages::guild_invite(name))
    }

    /// Accept the guild invitation we are holding (`CMSG_GUILD_ACCEPT`, empty body). Which
    /// invitation is the server's pending state, not a field — so there is nothing to pass, and
    /// nothing to get wrong except sending it when no invite is outstanding (a silent no-op).
    pub fn guild_accept(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GUILD_ACCEPT, &messages::guild_accept())
    }

    /// Turn down the guild invitation we are holding (`CMSG_GUILD_DECLINE`, empty body). The
    /// inviter is told by `SMSG_GUILD_DECLINE`; we hear nothing back.
    pub fn guild_decline(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GUILD_DECLINE, &messages::guild_decline())
    }

    /// Ask for our guild's founding date and member/account counts (`CMSG_GUILD_INFO`, empty
    /// body), answered by `SMSG_GUILD_INFO`. A different ask from the roster, sharing no fields.
    pub fn guild_info(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GUILD_INFO, &messages::guild_info())
    }

    /// Ask for the whole guild roster (`CMSG_GUILD_ROSTER`, empty body). The server also pushes
    /// `SMSG_GUILD_ROSTER` unasked after every change it makes, so this is a refresh — the guild
    /// pane's opener — and never the only way the roster arrives.
    pub fn guild_roster(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GUILD_ROSTER, &messages::guild_roster())
    }

    /// Promote a member one rank (`CMSG_GUILD_PROMOTE`): the server does `rank - 1`, *towards*
    /// guild master. Answered by a fresh roster, or refused with `RANK_TOO_HIGH_S`/`PERMISSIONS`.
    pub fn guild_promote(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_GUILD_PROMOTE, &messages::guild_promote(name))
    }

    /// Demote a member one rank (`CMSG_GUILD_DEMOTE`): `rank + 1`, away from guild master.
    pub fn guild_demote(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_GUILD_DEMOTE, &messages::guild_demote(name))
    }

    /// Leave our guild (`CMSG_GUILD_LEAVE`, empty body). Refused with
    /// `(QUIT, LEADER_LEAVE)` while we are the guild master and anyone else remains — hand over
    /// with [`Self::guild_leader`] first, or [`Self::guild_disband`].
    pub fn guild_leave(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GUILD_LEAVE, &messages::guild_leave())
    }

    /// Kick a member by name (`CMSG_GUILD_REMOVE`); needs the REMOVE right and a rank above theirs.
    pub fn guild_remove(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_GUILD_REMOVE, &messages::guild_remove(name))
    }

    /// Disband the guild (`CMSG_GUILD_DISBAND`, empty body). Guild master only, and irreversible —
    /// every member gets a `GE_DISBANDED` event.
    pub fn guild_disband(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GUILD_DISBAND, &messages::guild_disband())
    }

    /// Hand the guild to another member (`CMSG_GUILD_LEADER`). Guild master only; both names ride
    /// back to everyone as a `GE_LEADER_CHANGED` event.
    pub fn guild_leader(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_GUILD_LEADER, &messages::guild_leader(name))
    }

    /// Set the message of the day (`CMSG_GUILD_MOTD`). Passing `""` clears it — see
    /// [`messages::guild_motd`] for why we send the one-byte empty cstring rather than an empty
    /// body, which the server also accepts.
    pub fn guild_motd(&mut self, motd: &str) -> Result<()> {
        self.send(opcode::CMSG_GUILD_MOTD, &messages::guild_motd(motd))
    }

    /// Rewrite one rank's name **and** rights in a single packet (`CMSG_GUILD_RANK`).
    ///
    /// There is no partial form: a caller changing only the name must still send the rank's
    /// current rights, and vice versa. Guild master only; `rights` is ignored and replaced with
    /// [`messages::guild_rank_right::ALL`] for rank 0; and a `name` over
    /// [`messages::GUILD_RANK_MAX_LENGTH`] characters gets the session **kicked** by vmangos's
    /// anticheat rather than refused, so the caller caps it.
    pub fn guild_rank(&mut self, rank_id: u32, rights: u32, name: &str) -> Result<()> {
        self.send(
            opcode::CMSG_GUILD_RANK,
            &messages::guild_rank(rank_id, rights, name),
        )
    }

    /// Append a rank at the bottom of the ladder (`CMSG_GUILD_ADD_RANK`). It starts with guild
    /// chat listen + speak and nothing else. Silently ignored once the guild already has
    /// [`messages::GUILD_RANKS_MAX_COUNT`] ranks — the reference UI hides the button there.
    pub fn guild_add_rank(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_GUILD_ADD_RANK, &messages::guild_add_rank(name))
    }

    /// Delete the **lowest** rank (`CMSG_GUILD_DEL_RANK`, empty body). There is no rank id on the
    /// wire: it is always the last one, which is why the reference UI only ever offers to remove
    /// the bottom row. Refused with `RANK_IN_USE` while a member still holds it.
    pub fn guild_del_rank(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GUILD_DEL_RANK, &messages::guild_del_rank())
    }

    /// Set a member's public note (`CMSG_GUILD_SET_PUBLIC_NOTE`); needs
    /// [`messages::guild_rank_right::EDIT_PUBLIC_NOTE`].
    pub fn guild_set_public_note(&mut self, name: &str, note: &str) -> Result<()> {
        self.send(
            opcode::CMSG_GUILD_SET_PUBLIC_NOTE,
            &messages::guild_set_public_note(name, note),
        )
    }

    /// Set a member's officer note (`CMSG_GUILD_SET_OFFICER_NOTE`); needs
    /// [`messages::guild_rank_right::EDIT_OFFICER_NOTE`]. Note that *seeing* officer notes is a
    /// separate right — a roster whose officer notes are all empty may mean we cannot view them.
    pub fn guild_set_officer_note(&mut self, name: &str, note: &str) -> Result<()> {
        self.send(
            opcode::CMSG_GUILD_SET_OFFICER_NOTE,
            &messages::guild_set_officer_note(name, note),
        )
    }

    /// Set the guild information text (`CMSG_GUILD_INFO_TEXT`) — the long free-text pane, not the
    /// MOTD and not `SMSG_GUILD_INFO`'s counts. It rides back on the roster as its `info` field.
    pub fn guild_info_text(&mut self, text: &str) -> Result<()> {
        self.send(
            opcode::CMSG_GUILD_INFO_TEXT,
            &messages::guild_info_text(text),
        )
    }
}
