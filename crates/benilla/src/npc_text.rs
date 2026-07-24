//! NPC-text chat macros — the client-side substitution the 1.12 wire leaves **un**-expanded in
//! every server-authored text the player reads: gossip greetings, questgiver panel texts, and the
//! quest log's description/objectives (the director's screenshot showed a literal "$N" in a quest
//! description — the real client substitutes everywhere quest/gossip text renders). One shared
//! mechanism (the 0109 look fix promoted it out of `ui_gossip`); every feed seam that pushes NPC
//! text into the VM runs it.
//!
//! Handled: `$N`/`$n` → the player's name; `$B`/`$b` → a newline (the text renderer splits on
//! `\n`); `$G male:female;` / `$g …` → the gender branch (`gender`: 0 = male, 1 = female). **Left
//! for a follow-up** (and so passed through literally): `$C`/`$c` (class name) and `$R`/`$r` (race
//! name) — the client keeps localized class/race name tables the app doesn't have handy yet. An
//! unterminated `$G…` is passed through literally too (degrade-to-visible, never eat text).

use bevy::prelude::*;

use crate::names::NameCache;
use crate::net::{Guid, NetCommands, ObjectStore, SelfPlayer};

/// Substitute the chat-text macros into `text` (see the module doc for the handled set).
pub(crate) fn substitute(text: &str, name: &str, gender: u8) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() {
            match chars[i + 1] {
                'N' | 'n' => {
                    out.push_str(name);
                    i += 2;
                    continue;
                }
                'B' | 'b' => {
                    out.push('\n');
                    i += 2;
                    continue;
                }
                'G' | 'g' => {
                    if let Some((male, female, end)) = parse_gender_branch(&chars, i + 2) {
                        out.extend(if gender == 1 { female } else { male });
                        i = end;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Parse a `$G`/`$g` branch body `male:female;` starting at `start` (just past the `G`), returning
/// the two arms (as `&str` slices of `chars`) and the index just past the terminating `;`. `None`
/// if there's no `:` before the `;`, or no terminating `;` (malformed → caller keeps it literal).
fn parse_gender_branch(chars: &[char], start: usize) -> Option<(&[char], &[char], usize)> {
    let colon = (start..chars.len()).find(|&j| chars[j] == ':' || chars[j] == ';')?;
    if chars[colon] != ':' {
        return None;
    }
    let semi = (colon + 1..chars.len()).find(|&j| chars[j] == ';')?;
    Some((&chars[start..colon], &chars[colon + 1..semi], semi + 1))
}

/// The self player's `(name, gender)` for substitution: the name from the [`NameCache`] (a miss
/// queries the server once, like the unit frames), gender from the descriptor (`unit_gender`:
/// 0 male / 1 female). Falls back to an empty name / male when the self player or its name isn't
/// known yet (the feeds diff on the name, so text re-substitutes when it lands).
pub(crate) fn player_identity(
    self_q: &Query<(&ObjectStore, &Guid), With<SelfPlayer>>,
    names: &mut NameCache,
    commands: &NetCommands,
) -> (String, u8) {
    match self_q.iter().next() {
        Some((store, guid)) => {
            let name = names.resolve(guid.0, commands).unwrap_or("").to_string();
            (name, store.0.unit_gender().unwrap_or(0))
        }
        None => (String::new(), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::substitute;

    #[test]
    fn macros_substitute() {
        // $N → name, $B → newline.
        assert_eq!(substitute("Greetings $N", "Thrall", 0), "Greetings Thrall");
        assert_eq!(substitute("Hail,$Bfriend", "X", 0), "Hail,\nfriend");
        assert_eq!(substitute("$N,$Bwelcome", "Jaina", 0), "Jaina,\nwelcome");
        // $G male:female; branches on gender (0 male / 1 female).
        assert_eq!(
            substitute("Well met, $Glad:lass;.", "X", 0),
            "Well met, lad."
        );
        assert_eq!(
            substitute("Well met, $Glad:lass;.", "X", 1),
            "Well met, lass."
        );
        // Unknown macros + malformed $G pass through literally (degrade-to-visible).
        assert_eq!(substitute("A $C of $R", "X", 0), "A $C of $R");
        assert_eq!(substitute("Broken $Gbranch", "X", 0), "Broken $Gbranch");
        // A trailing lone $ survives.
        assert_eq!(substitute("Cost: 5$", "X", 0), "Cost: 5$");
    }
}
