//! `benilla-visual` — diff captures from the Phase-5 visual A/B harness (decision 0008).
//!
//! Usage:
//!   benilla-visual diff     <a.png> <b.png>   [--out <diff.png>] [--fail <mae>] [--amplify <n>]
//!   benilla-visual diff-dir <dir_a> <dir_b>   [--out <diff_dir>] [--fail <mae>] [--amplify <n>]
//!
//! `diff` compares two images; `diff-dir` compares every `*.png` present in *both* directories by name.
//! Prints the metrics; writes amplified heatmap(s) when `--out` is given; exits non-zero if any image's
//! MAE exceeds `--fail` (when given). Typical loop: capture baselines, change the renderer, re-capture,
//! `diff-dir baseline candidate --out diff --fail 1.5`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use benilla_visual::{compare, compose_side_by_side, diff_image, Metrics, OVER_THRESHOLD};

/// Gap (px) between the two halves of a side-by-side compose.
const COMPOSE_GAP: u32 = 6;

/// Default amplification for the heatmap output (per-channel abs-diff ×N, clamped).
const DEFAULT_AMPLIFY: u32 = 8;

struct Opts {
    out: Option<PathBuf>,
    fail: Option<f64>,
    amplify: u32,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut opts = Opts {
        out: None,
        fail: None,
        amplify: DEFAULT_AMPLIFY,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => opts.out = Some(PathBuf::from(next(&mut it, "--out")?)),
            "--fail" => {
                opts.fail = Some(
                    next(&mut it, "--fail")?
                        .parse()
                        .context("--fail not a number")?,
                )
            }
            "--amplify" => {
                opts.amplify = next(&mut it, "--amplify")?
                    .parse()
                    .context("--amplify not an integer")?
            }
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            _ => positional.push(a.clone()),
        }
    }

    let Some((cmd, rest)) = positional.split_first() else {
        print_usage();
        bail!("no subcommand given");
    };

    match cmd.as_str() {
        "diff" => {
            let [a, b] = two(rest, "diff")?;
            let m = diff_one(
                Path::new(a),
                Path::new(b),
                opts.out.as_deref(),
                opts.amplify,
            )?;
            println!("{:<28} {}", format!("{a} vs {b}"), fmt_metrics(&m));
            if over_fail(&m, opts.fail) {
                bail!("MAE {:.3} exceeds --fail {:.3}", m.mae, opts.fail.unwrap());
            }
        }
        "diff-dir" => {
            let [da, db] = two(rest, "diff-dir")?;
            let worst = diff_dir(
                Path::new(da),
                Path::new(db),
                opts.out.as_deref(),
                opts.amplify,
            )?;
            if let Some((name, m)) = worst {
                if over_fail(&m, opts.fail) {
                    bail!(
                        "worst image {name:?} MAE {:.3} exceeds --fail {:.3}",
                        m.mae,
                        opts.fail.unwrap()
                    );
                }
            }
        }
        "compose-dir" => {
            let [da, db] = two(rest, "compose-dir")?;
            let out = opts
                .out
                .as_deref()
                .context("compose-dir needs --out <dir>")?;
            compose_dir(Path::new(da), Path::new(db), out)?;
        }
        other => {
            print_usage();
            bail!("unknown subcommand {other:?}");
        }
    }
    Ok(())
}

/// Stitch every `*.png` present in both dirs into `left | right` side-by-side images under `out`.
fn compose_dir(da: &Path, db: &Path, out: &Path) -> Result<()> {
    let names: BTreeSet<String> = pngs(da)?.intersection(&pngs(db)?).cloned().collect();
    if names.is_empty() {
        bail!(
            "no common *.png files between {} and {}",
            da.display(),
            db.display()
        );
    }
    std::fs::create_dir_all(out).ok();
    for name in &names {
        let (l, r) = (load(&da.join(name))?, load(&db.join(name))?);
        let composed = compose_side_by_side(&l, &r, COMPOSE_GAP);
        let dst = out.join(name);
        composed
            .save(&dst)
            .with_context(|| format!("writing {}", dst.display()))?;
        println!("{} -> {}", name, dst.display());
    }
    Ok(())
}

fn next(it: &mut std::slice::Iter<String>, flag: &str) -> Result<String> {
    it.next()
        .cloned()
        .with_context(|| format!("{flag} needs a value"))
}

fn two<'a>(rest: &'a [String], cmd: &str) -> Result<[&'a str; 2]> {
    match rest {
        [a, b] => Ok([a, b]),
        _ => bail!("{cmd} needs exactly two paths, got {}", rest.len()),
    }
}

fn over_fail(m: &Metrics, fail: Option<f64>) -> bool {
    fail.is_some_and(|t| m.mae > t)
}

/// Load an image as RGB (dropping any alpha — capture windows are opaque).
fn load(path: &Path) -> Result<image::RgbImage> {
    Ok(image::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .to_rgb8())
}

fn diff_one(a: &Path, b: &Path, out: Option<&Path>, amplify: u32) -> Result<Metrics> {
    let (ia, ib) = (load(a)?, load(b)?);
    let m = compare(&ia, &ib)?;
    if let Some(out) = out {
        if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).ok();
        }
        diff_image(&ia, &ib, amplify)?
            .save(out)
            .with_context(|| format!("writing diff {}", out.display()))?;
    }
    Ok(m)
}

/// Compare every `*.png` present in both dirs (by file name). Returns the worst (highest-MAE) result.
fn diff_dir(
    da: &Path,
    db: &Path,
    out: Option<&Path>,
    amplify: u32,
) -> Result<Option<(String, Metrics)>> {
    let names: BTreeSet<String> = pngs(da)?.intersection(&pngs(db)?).cloned().collect();
    if names.is_empty() {
        bail!(
            "no common *.png files between {} and {}",
            da.display(),
            db.display()
        );
    }
    if let Some(out) = out {
        std::fs::create_dir_all(out).ok();
    }
    let mut worst: Option<(String, Metrics)> = None;
    for name in &names {
        let diff_out = out.map(|o| o.join(name));
        let m = diff_one(&da.join(name), &db.join(name), diff_out.as_deref(), amplify)?;
        println!("{:<24} {}", name, fmt_metrics(&m));
        if worst.as_ref().is_none_or(|(_, w)| m.mae > w.mae) {
            worst = Some((name.clone(), m));
        }
    }
    if let Some((name, m)) = &worst {
        println!("worst: {name} (MAE {:.3})", m.mae);
    }
    Ok(worst)
}

fn pngs(dir: &Path) -> Result<BTreeSet<String>> {
    let mut set = BTreeSet::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                set.insert(name.to_string());
            }
        }
    }
    Ok(set)
}

fn fmt_metrics(m: &Metrics) -> String {
    format!(
        "MAE {:>6.3}  RMSE {:>6.3}  max {:>3}  >{}: {:>6.2}%",
        m.mae,
        m.rmse,
        m.max_delta,
        OVER_THRESHOLD,
        m.pct_over * 100.0
    )
}

fn print_usage() {
    eprintln!(
        "benilla-visual — diff Phase-5 visual-harness captures\n\
         \n\
         USAGE:\n  \
           benilla-visual diff        <a.png> <b.png> [--out <diff.png>] [--fail <mae>] [--amplify <n>]\n  \
           benilla-visual diff-dir    <dir_a> <dir_b> [--out <diff_dir>] [--fail <mae>] [--amplify <n>]\n  \
           benilla-visual compose-dir <dir_a> <dir_b> --out <dir>   (side-by-side `a | b` per image)\n\
         \n\
         Exits non-zero if any image's MAE exceeds --fail."
    );
}
