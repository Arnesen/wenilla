//! `mpqcat` — write one file out of an MPQ to stdout. The instrument for reading the real client's
//! own data (FrameXML, GlobalStrings, a BLP header) when a fidelity question needs the source
//! rather than a memory of it. Several decision records cite an external `mpqx` for this; nothing
//! in the workspace could do it until now, so every lookup was a hunt.
//!
//! ```text
//! cargo run -q -p benilla-mpq --example mpqcat -- <archive.MPQ> 'Interface\FrameXML\MacroFrame.xml'
//! ```
//!
//! Archive names are the client's own backslash paths. A miss is an error, not empty output — the
//! same file often lives in several archives (patch beats interface beats base), and "not here"
//! and "here but empty" are answers you must be able to tell apart.
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(archive), Some(name)) = (args.next(), args.next()) else {
        eprintln!("usage: mpqcat <archive.MPQ> <file-in-archive>");
        std::process::exit(2);
    };
    let bytes = benilla_mpq::Archive::open(&archive)?.read_file(&name)?;
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}
