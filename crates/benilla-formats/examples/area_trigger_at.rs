//! Where an area trigger actually **is**, and how to walk into it:
//! `cargo run -p benilla-formats --example area_trigger_at -- <id|map> [id|map…]`
//!
//! The client's half of every portal is pure geometry (`crate::area_trigger`'s law), and that
//! geometry lives only in `AreaTrigger.dbc` — the server's `areatrigger_teleport` row names the
//! *destination* but never the volume you have to cross to reach it. So a probe that wants to
//! reproduce a portal the way a player meets it — **walking in**, which the module's own warning
//! says is not the same as a `.go` that lands inside — has no way to aim without this.
//!
//! Prints each row's volume (sphere radius, or the oriented box) plus a ready-made `.go` for a spot
//! `APPROACH` yards **outside** it on the box's own axis, and the facing that then walks you
//! through. An argument about whether a portal fired is otherwise unfalsifiable: landing inside the
//! volume races the server's re-check and is silently ignored about one time in six (measured, see
//! `crate::area_trigger`).
//!
//! Output is Blizzard-derived; pipe it to the scratchpad, never into the repo.

/// How far outside the volume to park the approach `.go`, in yards. Far enough that the trigger is
/// unambiguously *not* yet entered (so the walk-in is a real crossing), close enough that a few
/// seconds of held-forward covers it.
const APPROACH: f32 = 12.0;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        anyhow::bail!("usage: area_trigger_at <id|map:N>...");
    }
    let data = benilla_formats::wow_data().expect("no WoW install found (set $WOW_DATA)");
    let mut chain = benilla_formats::open_chain(&data)?;
    let cat = benilla_formats::load_area_trigger_catalog(&mut chain)?;
    println!("AreaTrigger.dbc — {} rows\n", cat.len());

    for arg in &args {
        let rows: Vec<_> = if let Some(m) = arg.strip_prefix("map:") {
            cat.on_map(m.parse()?).iter().collect()
        } else {
            cat.get(arg.parse()?).into_iter().collect()
        };
        if rows.is_empty() {
            println!("no rows for {arg:?}\n");
            continue;
        }
        for r in rows {
            let [x, y, z] = r.position;
            if r.radius > 0.0 {
                println!(
                    "id {:5}  map {:3}  SPHERE r={:.1} at ({x:.2}, {y:.2}, {z:.2})",
                    r.id, r.map_id, r.radius
                );
                // A sphere has no authored axis; approach along -Y, the arbitrary but stated choice.
                let sy = y + r.radius + APPROACH;
                println!(
                    "        walk in:  .go xyz {x:.2} {sy:.2} {z:.2} {}   then face -Y (yaw {:.3}) and hold forward",
                    r.map_id,
                    std::f32::consts::FRAC_PI_2 * 3.0,
                );
            } else {
                let [bx, by, bz] = r.box_size;
                println!(
                    "id {:5}  map {:3}  BOX {bx:.1}x{by:.1}x{bz:.1} yaw {:.3} at ({x:.2}, {y:.2}, {z:.2})",
                    r.id, r.map_id, r.box_yaw
                );
                // Approach along the box's local +X axis, backing off half its depth plus the margin.
                let (s, c) = r.box_yaw.sin_cos();
                let back = bx * 0.5 + APPROACH;
                let (sx, sy) = (x - c * back, y - s * back);
                println!(
                    "        walk in:  .go xyz {sx:.2} {sy:.2} {z:.2} {}   then face yaw {:.3} and hold forward",
                    r.map_id, r.box_yaw
                );
            }
        }
        println!();
    }
    Ok(())
}
