//! The social family's `WorldWriter` sends — friends, ignores, and `/who` (decision 0668).
//!
//! Note the asymmetry the wire imposes and the UI has to live with: you **add** by name and
//! **remove** by guid (`CMSG_ADD_FRIEND` takes a cstring, `CMSG_DEL_FRIEND` a guid). That is not
//! an inconsistency to paper over — it is why the client keeps both lists keyed by guid and
//! resolves display names through the name cache (see [`crate::messages::social`]'s module doc).
//! A "remove Bob" verb therefore has to find Bob's guid in the list *before* it can send anything.

use anyhow::Result;

use crate::messages::{self, opcode, WhoRequest};

use super::WorldWriter;

impl WorldWriter {
    /// Ask for the friend list again (`CMSG_FRIEND_LIST`, empty body) — the FrameXML's
    /// `ShowFriends()`. The server also pushes it unasked at login, so this is a refresh, never
    /// the only way the list arrives.
    pub fn friend_list(&mut self) -> Result<()> {
        self.send(opcode::CMSG_FRIEND_LIST, &messages::friend_list())
    }

    /// Befriend a character by name (`CMSG_ADD_FRIEND`). The answer is an `SMSG_FRIEND_STATUS`
    /// carrying one of the `FRIEND_ADDED_*` / refusal codes — never a silent success.
    pub fn add_friend(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_ADD_FRIEND, &messages::add_friend(name))
    }

    /// Drop a friend by guid (`CMSG_DEL_FRIEND`); acked with `FRIEND_REMOVED`.
    pub fn del_friend(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_DEL_FRIEND, &messages::del_friend(guid))
    }

    /// Ignore a character by name (`CMSG_ADD_IGNORE`); acked with `FRIEND_IGNORE_ADDED`.
    pub fn add_ignore(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_ADD_IGNORE, &messages::add_ignore(name))
    }

    /// Stop ignoring, by guid (`CMSG_DEL_IGNORE`); acked with `FRIEND_IGNORE_REMOVED`.
    pub fn del_ignore(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_DEL_IGNORE, &messages::del_ignore(guid))
    }

    /// Run a `/who` (`CMSG_WHO`). The server answers at most one query per session at a time
    /// (`ReceivedWhoRequest` gates it) and runs it as an async task, so the `SMSG_WHO` lands a
    /// tick or two later — a second query fired while one is in flight is dropped, not queued.
    pub fn who(&mut self, request: &WhoRequest) -> Result<()> {
        self.send(opcode::CMSG_WHO, &messages::who(request))
    }
}
