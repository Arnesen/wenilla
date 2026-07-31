//! How far does a welded billboard bone's geometry actually MOVE when the bone is re-oriented?
//!
//! The question decision 0841's regression turns on. A spherical billboard replaces the bone's
//! world rotation outright, so a vertex weighted to it swings by up to `2·|v − pivot|` — while a
//! **seam** vertex (partially weighted) swings by its weight times that, *and* linear-blend skinning
//! shrinks it toward the pivot on the way (the candy-wrapper). If the seam ring sits ON the pivot,
//! both effects vanish and the flap hinges cleanly; if it sits far out, the seam sweeps through
//! whatever it is welded to and no implementation of the law can avoid it.
//!
//! Usage: `cargo run -p benilla-formats --example seamswing -- <WoW/Data> <internal\path.m2>`

use benilla_formats::open_chain;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let data = args.next().unwrap_or_else(|| "WoW/Data".into());
    let path = args.next().expect("usage: seamswing <data> <path.m2>");
    let mut chain = open_chain(std::path::Path::new(&data))?;
    let bytes = chain.read_file(&path)?;
    let denied = benilla_formats::non_separable_billboard_bones(&bytes);
    println!("welded billboard bones: {denied:?}");

    let model = benilla_m2::parse_m2(&mut std::io::Cursor::new(&bytes))?;
    let model = model.model();
    for &b in &denied {
        let bone = &model.bones[b as usize];
        let piv = [bone.pivot.x, bone.pivot.y, bone.pivot.z];
        // Every vertex with any weight on this bone: its weight, and its distance from the pivot —
        // which is the radius the re-orientation sweeps it around.
        let (mut pure, mut seam) = (Vec::new(), Vec::new());
        for v in &model.vertices {
            for i in 0..4 {
                if v.bone_indices[i] as u16 != b || v.bone_weights[i] == 0 {
                    continue;
                }
                let pos = [v.position.x, v.position.y, v.position.z];
                let d = ((pos[0] - piv[0]).powi(2)
                    + (pos[1] - piv[1]).powi(2)
                    + (pos[2] - piv[2]).powi(2))
                .sqrt();
                let w = f32::from(v.bone_weights[i]) / 255.0;
                if w > 0.999 {
                    pure.push(d)
                } else {
                    seam.push((w, d))
                }
            }
        }
        let stat = |v: &[f32]| {
            if v.is_empty() {
                return "—".to_string();
            }
            let (mn, mx) = v
                .iter()
                .fold((f32::MAX, 0.0f32), |(a, b), &d| (a.min(d), b.max(d)));
            format!(
                "n={} min={mn:.4} max={mx:.4} mean={:.4}",
                v.len(),
                v.iter().sum::<f32>() / v.len() as f32
            )
        };
        let seam_d: Vec<f32> = seam.iter().map(|&(_, d)| d).collect();
        println!(
            "bone {b} pivot ({:.3}, {:.3}, {:.3})",
            piv[0], piv[1], piv[2]
        );
        println!("  fully-weighted verts, |v−pivot|: {}", stat(&pure));
        println!("  SEAM verts,          |v−pivot|: {}", stat(&seam_d));
        if let Some(&(w, _)) = seam.first() {
            println!("  seam weight on this bone: {w:.2}");
        }
    }
    Ok(())
}
