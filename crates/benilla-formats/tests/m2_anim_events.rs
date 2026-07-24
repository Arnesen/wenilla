//! M2 anim-event regression test against real vanilla creatures (decision 0070 slice 3).
//!
//! Guards the event-track parse (events @MD20 0x114, stride 44, timestamps on the global
//! sequence timeline — byte-verified on DireWolf.m2): every event key must land inside its
//! sequence's `[0, duration]` window after the rebase, footstep tags must exist on walking
//! creatures, and the identifier bytes must be forward-stored printable `$xxx` tags (a reversed
//! read would produce `xxx$`). Skips when the client isn't present.

use std::path::PathBuf;

use benilla_formats::{open_chain, parse_m2_animations};

fn vanilla_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
}

#[test]
fn creature_anim_events_parse_within_sequences() {
    let data = vanilla_data_dir();
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let chain = open_chain(&data).expect("open chain");

    for model in [
        "Creature\\DireWolf\\DireWolf.m2",
        "Creature\\Kobold\\Kobold.m2",
        "Creature\\Murloc\\Murloc.m2",
    ] {
        let bytes = chain.read(model).expect("model bytes");
        let anims = parse_m2_animations(&bytes);
        assert!(!anims.is_empty(), "{model}: sequences parse");

        let mut total_events = 0usize;
        let mut footstep_tags = 0usize;
        for a in &anims {
            for e in &a.events {
                total_events += 1;
                assert!(
                    e.time >= 0.0 && e.time <= a.duration + 1e-3,
                    "{model}: event {:?} at {}s escapes its {}s sequence (anim {})",
                    std::str::from_utf8(&e.ident),
                    e.time,
                    a.duration,
                    a.anim_id
                );
                assert_eq!(e.ident[0], b'$', "{model}: forward-stored $ tag");
                if matches!(&e.ident[..3], b"$FL" | b"$FR" | b"$RL" | b"$RR") {
                    footstep_tags += 1;
                }
            }
        }
        assert!(total_events > 0, "{model}: has event keys");
        assert!(
            footstep_tags > 0,
            "{model}: walking creature has footstep tags"
        );
    }
}
