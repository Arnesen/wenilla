//! The slash-command **table** (decision 0881): every `/command` the client answers, built at boot
//! from the reference's OWN alias strings rather than hand-typed here.
//!
//! ## Why a table
//!
//! `ChatEdit_ParseText` (ChatFrame.lua l.2163-2199) resolves a typed command in three passes, and
//! **none of them holds a literal**: the chat-type switches walk `SLASH_<ChatTypeInfo index><n>`,
//! the actions walk `SLASH_<SlashCmdList index><n>`, and the emotes walk `EMOTE<i>_CMD<j>` →
//! `EMOTE<i>_TOKEN` → `DoEmote(token)`. The aliases are DATA (`GlobalStrings.lua`), the handlers
//! are code, and the two are joined by an index name.
//!
//! Benilla used to re-type each command's aliases into a Rust `match` as its system was built.
//! Every alias nobody happened to copy was a command that silently did not exist: the audit behind
//! 0881 found **61 live reference aliases dead** (`/lol`, `/hi`, `/ty`, `/congrats`, `/sorry`,
//! `/yes`, `/bravo`, …) because the emote arm matched the *`EmotesText.dbc` name* instead of the
//! alias table — and four names the reference has NO command for (`/joke`, `/puzzle`,
//! `/attackmytarget`, and `/follow`, which fired a text emote where the real client follows your
//! target) answering as emotes. A table keyed on the shipped strings cannot have that class of bug:
//! registering a handler is naming its index, and the aliases come from the data.
//!
//! ## Where each half comes from
//!
//! - **`SLASH_<INDEX><n>`** — the real `GlobalStrings.lua`, already executed into the UI VM at boot
//!   (`crate::ui_script`). One [`SlashIndex`] per command benilla implements; the reference's own
//!   index name is the key.
//! - **`EMOTE<i>_CMD<j>` / `EMOTE<i>_TOKEN`** — the alias→token join, also read off the VM globals
//!   (`crate::ui_script::load_emote_tokens` runs the token half's data lines out of the shipped
//!   `ChatFrame.lua`). The token resolves to an `EmotesText` id through the emote catalog.
//! - **benilla's own instruments** ([`DevCmd`]) — `/castvis`, `/chattest`, `/shot`, … have no
//!   reference strings because they are not reference commands; their aliases are literals here,
//!   the one place in this module where that is honest.
//!
//! Precedence is the reference's resolution ORDER, made static at insert time: a slash index wins
//! over an emote alias of the same name (the reference reaches `SlashCmdList` first), and benilla's
//! instruments are inserted last so they can never shadow a real command.

use std::collections::HashMap;

use bevy::prelude::*;

/// A command benilla implements, identified by the reference's own `SlashCmdList` index — the
/// `SLASH_<INDEX><n>` alias-string prefix ([`SlashIndex::key`]). Adding a command is adding a
/// variant + its handler arm; its aliases arrive with the shipped strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlashIndex {
    Reply,
    Join,
    Leave,
    ListChannel,
    ChatAfk,
    ChatDnd,
    Random,
    Played,
    Help,
    Logout,
    Quit,
    Invite,
    Uninvite,
    Promote,
    Duel,
    DuelCancel,
    Pvp,
    Who,
    Friends,
    RemoveFriend,
    Ignore,
    Unignore,
    Trade,
    Inspect,
    Script,
    LootFfa,
    LootRoundRobin,
    LootMaster,
}

impl SlashIndex {
    /// The reference's index name — the `SLASH_<KEY><n>` prefix its aliases live under.
    fn key(self) -> &'static str {
        match self {
            Self::Reply => "REPLY",
            Self::Join => "JOIN",
            Self::Leave => "LEAVE",
            Self::ListChannel => "LIST_CHANNEL",
            Self::ChatAfk => "CHAT_AFK",
            Self::ChatDnd => "CHAT_DND",
            Self::Random => "RANDOM",
            Self::Played => "PLAYED",
            Self::Help => "HELP",
            Self::Logout => "LOGOUT",
            Self::Quit => "QUIT",
            Self::Invite => "INVITE",
            Self::Uninvite => "UNINVITE",
            Self::Promote => "PROMOTE",
            Self::Duel => "DUEL",
            Self::DuelCancel => "DUEL_CANCEL",
            Self::Pvp => "PVP",
            Self::Who => "WHO",
            Self::Friends => "FRIENDS",
            Self::RemoveFriend => "REMOVEFRIEND",
            Self::Ignore => "IGNORE",
            Self::Unignore => "UNIGNORE",
            Self::Trade => "TRADE",
            Self::Inspect => "INSPECT",
            Self::Script => "SCRIPT",
            Self::LootFfa => "LOOT_FFA",
            Self::LootRoundRobin => "LOOT_ROUNDROBIN",
            Self::LootMaster => "LOOT_MASTER",
        }
    }

    /// Every registered index — the registry proper. A reference command NOT in this list resolves
    /// nowhere and answers `HELP_TEXT_SIMPLE`, exactly as an unknown command does in the reference
    /// (better than a registered handler that silently does nothing).
    const ALL: [Self; 28] = [
        Self::Reply,
        Self::Join,
        Self::Leave,
        Self::ListChannel,
        Self::ChatAfk,
        Self::ChatDnd,
        Self::Random,
        Self::Played,
        Self::Help,
        Self::Logout,
        Self::Quit,
        Self::Invite,
        Self::Uninvite,
        Self::Promote,
        Self::Duel,
        Self::DuelCancel,
        Self::Pvp,
        Self::Who,
        Self::Friends,
        Self::RemoveFriend,
        Self::Ignore,
        Self::Unignore,
        Self::Trade,
        Self::Inspect,
        Self::Script,
        Self::LootFfa,
        Self::LootRoundRobin,
        Self::LootMaster,
    ];
}

/// Benilla's own diagnostic commands — instruments we built, with no reference strings behind them
/// (decisions 0099 `/castvis`, 0288 `/chattest`, 0434 `/partytest`, 0600 `/shot`, 0634 `/liquid`,
/// 0637 `/reaction`). Their aliases are literals because there is nothing shipped to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DevCmd {
    CastVis,
    ChatTest,
    PartyTest,
    Shot,
    Liquid,
    Reaction,
}

impl DevCmd {
    fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::CastVis => &["castvis"],
            Self::ChatTest => &["chattest"],
            Self::PartyTest => &["partytest"],
            Self::Shot => &["shot"],
            Self::Liquid => &["liquid"],
            Self::Reaction => &["reaction", "react"],
        }
    }

    const ALL: [Self; 6] = [
        Self::CastVis,
        Self::ChatTest,
        Self::PartyTest,
        Self::Shot,
        Self::Liquid,
        Self::Reaction,
    ];
}

/// What a typed `/command` resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Command {
    /// A reference `SlashCmdList` command benilla implements.
    Slash(SlashIndex),
    /// An emote alias — carrying the `EmotesText` id its token resolved to (`/lol` → LAUGH → 45).
    Emote { text_id: u32 },
    /// One of benilla's own instruments.
    Dev(DevCmd),
}

/// The built table: alias (lowercase, no leading `/`) → command.
#[derive(Resource, Default)]
pub(crate) struct SlashCommands {
    by_alias: HashMap<String, Command>,
    /// How many aliases each source contributed — the boot line that makes a broken table (an
    /// unreadable `GlobalStrings.lua`, a missing token table) visible immediately instead of at
    /// the first command someone types.
    counts: (usize, usize),
}

impl SlashCommands {
    /// Resolve a typed command (no leading `/`, any case).
    pub(super) fn lookup(&self, cmd: &str) -> Option<Command> {
        self.by_alias.get(&cmd.to_ascii_lowercase()).copied()
    }

    /// Build from the reference's alias strings. `get` reads a global string (the UI VM's globals in
    /// production, a stub map in tests); `text_id` resolves an `EmotesText` NAME to its id (the
    /// emote catalog). Pure over those two, so the whole table is testable without a VM.
    pub(super) fn build(
        get: impl Fn(&str) -> Option<String>,
        text_id: impl Fn(&str) -> Option<u32>,
    ) -> Self {
        let mut by_alias: HashMap<String, Command> = HashMap::new();

        // 1 · the action commands: SLASH_<INDEX><n>, walked from 1 until the first gap (the
        // reference's own `while cmdString` walk).
        for index in SlashIndex::ALL {
            for n in 1.. {
                let Some(alias) = get(&format!("SLASH_{}{n}", index.key())) else {
                    break;
                };
                insert(&mut by_alias, &alias, Command::Slash(index));
            }
        }
        let slash_aliases = by_alias.len();

        // 2 · the emotes: EMOTE<i>_CMD<j> → EMOTE<i>_TOKEN → EmotesText id. The outer walk stops at
        // the first index with no CMD1, exactly like `ChatEdit_ParseText`'s.
        for i in 1.. {
            let Some(first) = get(&format!("EMOTE{i}_CMD1")) else {
                break;
            };
            // A token with no `EmotesText` row (EMOTE27 is "UNUSED") has nothing to send: the
            // reference's `DoEmote` would find no record either.
            let resolved = get(&format!("EMOTE{i}_TOKEN")).and_then(|t| text_id(&t));
            if let Some(text_id) = resolved {
                insert(&mut by_alias, &first, Command::Emote { text_id });
                for j in 2.. {
                    let Some(alias) = get(&format!("EMOTE{i}_CMD{j}")) else {
                        break;
                    };
                    insert(&mut by_alias, &alias, Command::Emote { text_id });
                }
            }
        }
        let emote_aliases = by_alias.len() - slash_aliases;

        // 3 · benilla's instruments, last so they can never shadow a shipped command.
        for dev in DevCmd::ALL {
            for alias in dev.aliases() {
                insert(&mut by_alias, alias, Command::Dev(dev));
            }
        }

        Self {
            by_alias,
            counts: (slash_aliases, emote_aliases),
        }
    }

    /// `(slash aliases, emote aliases)` — how many DISTINCT commands each source made reachable
    /// (the shipped strings repeat: `EMOTE87_CMD1` and `_CMD2` are both `"/sit"`). The boot report.
    pub(super) fn counts(&self) -> (usize, usize) {
        self.counts
    }
}

/// Insert-if-vacant: the FIRST source to claim an alias keeps it, which is how the reference's pass
/// order (`SlashCmdList` before emotes) becomes a static table.
fn insert(map: &mut HashMap<String, Command>, alias: &str, cmd: Command) {
    let key = alias.trim().trim_start_matches('/').to_ascii_lowercase();
    if !key.is_empty() {
        map.entry(key).or_insert(cmd);
    }
}

/// Build the table once the VM (its globals) and the emote catalog both exist. `PostStartup` rather
/// than an ordering constraint: both producers are `Startup` systems whose `insert_resource` lands
/// at the schedule boundary, so this needs no fragile `.after()` chain into two other modules.
pub(crate) fn build_slash_commands(
    mut commands: Commands,
    script: Option<NonSend<benilla_ui::script::UiScript>>,
    emotes: Option<Res<crate::sound::EmoteSounds>>,
) {
    let (Some(script), Some(emotes)) = (script, emotes) else {
        warn!("chat: no UI VM or emote catalog — the slash-command table is EMPTY");
        commands.insert_resource(SlashCommands::default());
        return;
    };
    let globals = script.lua().globals();
    let table = SlashCommands::build(
        |name| globals.get::<String>(name).ok().filter(|s| !s.is_empty()),
        |token| emotes.text_id(token),
    );
    let (slash, emote) = table.counts();
    // The "it loaded" signal: the shipped 1.12 data yields 55 distinct aliases across the
    // registered indices and 225 emote commands (pinned by `real_alias_table_resolves_the_shipped_
    // commands`).
    info!("chat: slash table — {slash} command aliases, {emote} emote aliases");
    if emote == 0 {
        error!("chat: NO emote aliases — every /wave-style command is dead (GlobalStrings?)");
    }
    commands.insert_resource(table);
}
