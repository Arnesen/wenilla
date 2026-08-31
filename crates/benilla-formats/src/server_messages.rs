//! `ServerMessages.dbc` — the five sentences the server can say to everybody at once.
//!
//! `SMSG_SERVER_MESSAGE` carries a **row id and a fill**, never a finished line: the client indexes
//! this table by the packet's `messageType` (`0x49dfbb`, the store at `[0xc0d990]`), takes the
//! row's localized text as a **format string**, and fills its `%s` with the packet's own text
//! (`snprintf(buf, 0x400, rowText, packetText)` at `0x49dff0`). A packet with an empty text copies
//! the row verbatim instead (`0x49dfc8` tests the first byte, `0x64a5a0` copies) — which is what
//! rows 4 and 5 need, having no `%s` at all.
//!
//! Layout — the real 5875 file (326 bytes, 5 records × **10 × u32** cols, 106-byte string block):
//! `ID(0), Text[8](1..8), TextMask(9)`. The whole shipped table, verbatim:
//!
//! | ID | enUS text | vmangos sends it with |
//! |---|---|---|
//! | 1 | `[SERVER] Shutdown in %s` | `secsToTimeString(remaining)` |
//! | 2 | `[SERVER] Restart in %s` | `secsToTimeString(remaining)` |
//! | 3 | `%s` | the operator's own text |
//! | 4 | `[SERVER] Shutdown cancelled` | `""` |
//! | 5 | `[SERVER] Restart cancelled` | `""` |
//!
//! The ids are vmangos's `enum ServerMessageType` (`src/game/World.h:62`) exactly, and its comment
//! on that enum is the single word `ServerMessages.dbc` — the server is reading this file's ids out
//! of the client's own data.
//!
//! **The row-missing arm is part of the mechanism, not an error path.** A type with no row renders
//! through the client's own fallback format `"[%d]: %s"` (`0x844864`, read out of the image), so an
//! unknown broadcast still reaches the player with its type visible rather than being dropped.
//! [`ServerMessagesCatalog::compose`] is that whole decision in one function.

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::chain::Chain;
use crate::dbc::{parse, str_at, u32_at};

const SERVER_MESSAGES: &str = "DBFilesClient\\ServerMessages.dbc";

/// The five rows, keyed by id.
#[derive(Clone, Debug, Default)]
pub struct ServerMessagesCatalog {
    rows: Vec<(u32, String)>,
}

impl ServerMessagesCatalog {
    /// A catalog over rows given directly — for a test that must run without an install.
    pub fn from_rows(rows: Vec<(u32, String)>) -> Self {
        ServerMessagesCatalog { rows }
    }

    /// The localized format string for a `messageType`, if the table has that row.
    pub fn text(&self, message_type: u32) -> Option<&str> {
        self.rows
            .iter()
            .find(|(id, _)| *id == message_type)
            .map(|(_, t)| t.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// The line `SMSG_SERVER_MESSAGE` becomes — the client's `0x49dfab`–`0x49e030` in full.
    ///
    /// Three arms, in the reference's own order: a row plus a non-empty fill formats; a row plus an
    /// empty fill copies the row (so `[SERVER] Restart cancelled` never grows a stray tail); no row
    /// falls back to `"[%d]: %s"` with the raw type.
    ///
    /// The substitution is `replacen(.., 1)` because the reference's is `snprintf` with **one**
    /// vararg: a row carrying two `%s` would fill the second with stack garbage, and filling only
    /// the first is the closest honest thing. No shipped row has two.
    pub fn compose(&self, message_type: u32, fill: &str) -> String {
        match self.text(message_type) {
            Some(row) if !fill.is_empty() => row.replacen("%s", fill, 1),
            Some(row) => row.to_string(),
            None => format!("[{message_type}]: {fill}"),
        }
    }
}

fn schema() -> Schema {
    let mut s = Schema::new("ServerMessages");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new_array("Text", FieldType::String, 8));
    s.add_field(SchemaField::new("TextMask", FieldType::UInt32));
    s.set_key_field("ID");
    s
}

pub fn load_server_messages_catalog(chain: &mut Chain) -> Result<ServerMessagesCatalog> {
    let bytes = chain
        .read_file(SERVER_MESSAGES)
        .context("reading ServerMessages.dbc")?;
    let rs = parse(&bytes, schema(), "ServerMessages")?;
    let mut rows = Vec::with_capacity(rs.records().len());
    for r in rs.records() {
        let (Some(id), Some(text)) = (u32_at(r, 0), str_at(&rs, r, 1)) else {
            continue;
        };
        rows.push((id, text));
    }
    Ok(ServerMessagesCatalog { rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real shipped table, row for row — the module doc's table asserted rather than trusted,
    /// because these five strings are the whole vocabulary of a server shutdown. Skips without
    /// client data.
    #[test]
    fn the_real_table_is_the_five_documented_rows() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_server_messages_catalog(&mut chain).expect("load ServerMessages");
        let got: Vec<(u32, &str)> = cat.rows.iter().map(|(i, t)| (*i, t.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (1, "[SERVER] Shutdown in %s"),
                (2, "[SERVER] Restart in %s"),
                (3, "%s"),
                (4, "[SERVER] Shutdown cancelled"),
                (5, "[SERVER] Restart cancelled"),
            ]
        );
    }

    fn shipped() -> ServerMessagesCatalog {
        ServerMessagesCatalog::from_rows(vec![
            (1, "[SERVER] Shutdown in %s".into()),
            (2, "[SERVER] Restart in %s".into()),
            (3, "%s".into()),
            (4, "[SERVER] Shutdown cancelled".into()),
            (5, "[SERVER] Restart cancelled".into()),
        ])
    }

    /// A countdown: the row is the format, the packet is the fill.
    #[test]
    fn a_countdown_fills_its_row() {
        assert_eq!(
            shipped().compose(1, "15 Minutes"),
            "[SERVER] Shutdown in 15 Minutes"
        );
        assert_eq!(shipped().compose(3, "back in five"), "back in five");
    }

    /// A cancellation carries no fill, and the row is already a whole sentence — the reference
    /// copies rather than formats, so nothing is appended and no `%s` survives.
    #[test]
    fn a_cancellation_copies_its_row_whole() {
        assert_eq!(shipped().compose(5, ""), "[SERVER] Restart cancelled");
    }

    /// A row-less type still reaches the player, through the client's own `"[%d]: %s"`.
    #[test]
    fn an_unknown_type_falls_back_to_the_bracketed_form() {
        assert_eq!(shipped().compose(9, "something new"), "[9]: something new");
    }

    /// The empty-fill test is on the FILL, not on whether the row has a `%s`: a row with a `%s` and
    /// no fill copies verbatim, leaving the token visible — which is what the reference does, and
    /// what no shipped row can actually reach (rows 1/2/3 always come with text).
    #[test]
    fn an_empty_fill_never_formats() {
        assert_eq!(shipped().compose(1, ""), "[SERVER] Shutdown in %s");
    }
}
