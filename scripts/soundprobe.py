#!/usr/bin/env python3
"""soundprobe.py — read back a `$WOW_SOUND_PROBE` capture and name the mechanism.

A measuring run (decision 1556) leaves three files: `pre.wav` (the mix as the game asked for it),
`post.wav` (what was heard), and `timeline.jsonl` (levels, voices, deadline misses, kit starts, and
the director's F9 marks — all keyed to the sample offset in `pre.wav`).

This reads them together and answers the question the capture was taken for: **of the mechanisms
that sound like a speaker breaking, which one is actually present?** They are distinguishable, but
only by comparing the two WAVs against each other and against the timeline — no single file
separates them:

    over-scale sum ............ pre past 1.0, post clean, gain well under 1
    limiter not engaging ...... pre past 1.0 AND post past 1.0
    underrun / missed deadline  both clean; `load>=1`, overruns, block-aligned steps in post
    starved stream decoder .... both clean; hard steps to zero in post
    non-finite samples ........ NaN/inf anywhere — invisible to every level meter
    voice refusal ............. `refused` climbing near the mark
    the limiter's own pumping . post clean but gain diving repeatedly (1551 made it worse)

Usage:
    scripts/soundprobe.py                       # the default benilla-config/sound-probe
    scripts/soundprobe.py <dir>                 # a capture directory
    scripts/soundprobe.py <dir> --around 2.0    # widen the window read around each mark
    scripts/soundprobe.py <dir> --json

Pure stdlib on purpose (no numpy on this machine — an instrument that needs an install is an
instrument nobody runs). Windowed reductions go through `max`/`min` on array slices so the hot
loop stays in C; the per-sample passes run only near the marks, where they are affordable.
"""

import argparse
import json
import math
import os
import struct
import sys
from array import array

# A sample this close to full scale was clipped, or was pushed there by a limiter that had to
# choose. Below the renderer's own clamp of 1.0 so a limiter riding its 0.99 ceiling reads as
# "held", not as "clipped".
CLIPPED = 0.999
# Envelope resolution. 10 ms is finer than any transient we care to place and keeps a ten-minute
# capture to ~60 000 windows.
WINDOW = 0.010
# A sample-to-sample jump this large is not music. Real 44.1 kHz content moves slowly between
# adjacent samples; a hard step is a zero-fill, a cut, or a stepped parameter — the waveform
# signature the level meters cannot see.
STEP = 0.30
# kira's internal block. A discontinuity landing on a multiple of this is a *block-level* failure
# (a starved decoder zero-filling a whole chunk), not a one-off click — the distinction between
# decision 1109's mechanism and a de-click bug.
BLOCK = 128


def read_wav(path):
    """(rate, interleaved float32 array). Tolerates the tap's crash-safe truncated tail."""
    with open(path, "rb") as f:
        head = f.read(44)
        if len(head) < 44 or head[0:4] != b"RIFF" or head[8:12] != b"WAVE":
            sys.exit(f"soundprobe: {path} is not a RIFF/WAVE file")
        fmt, channels, rate, _br, _al, bits = struct.unpack("<HHIIHH", head[20:36])
        if (fmt, channels, bits) != (3, 2, 32):
            sys.exit(f"soundprobe: expected stereo float-32, got fmt={fmt} ch={channels} bits={bits}")
        raw = f.read()
    usable = len(raw) - (len(raw) % 8)
    s = array("f")
    s.frombytes(raw[:usable])
    if sys.byteorder == "big":
        s.byteswap()
    return rate, s


def envelope(samples, rate, window=WINDOW):
    """Per-window peak |sample|, plus the count of clipped and non-finite samples in each."""
    step = max(1, int(rate * window)) * 2
    out = []
    for i in range(0, len(samples), step):
        seg = samples[i : i + step]
        if not seg:
            break
        # NaN/inf first, and via a SUM rather than a comparison: `max`/`min` compare with `>`,
        # which is false for every NaN, so a NaN in the window can be silently skipped depending on
        # where it sits. A sum propagates it unconditionally — the same trap this whole capture
        # exists because of (`f32::max` discards NaN; see sound::meter).
        if not math.isfinite(sum(seg)):
            nf = sum(1 for v in seg if not math.isfinite(v))
            finite = [v for v in seg if math.isfinite(v)]
            peak = max(max(finite), -min(finite)) if finite else 0.0
        else:
            nf = 0
            peak = max(max(seg), -min(seg))
        clipped = sum(1 for v in seg if abs(v) >= CLIPPED) if peak >= CLIPPED else 0
        out.append((i / 2 / rate, peak, clipped, nf))
    return out


def brightness(samples, rate, lo, hi):
    """A crude high-frequency energy ratio for [lo,hi): `sum((x[n]-x[n-1])^2) / sum(x[n]^2)`.

    NOT a spectrum — it is a one-tap difference filter, which is all that is affordable in stdlib.
    But it is enough for the question a mark actually poses: **was that moment harsher, or just
    louder?** Distortion of any origin — a clipped waveform, aliasing from a pitch-varied playback
    rate, a stepped parameter — puts energy high in the band, and this number rises with it while
    staying flat for content that merely got loud. Meaningful only against the same capture's own
    baseline, so it is always reported next to one.
    """
    a, b = max(0, int(lo * rate)) * 2, min(len(samples), int(hi * rate) * 2)
    num = den = 0.0
    for ch in (0, 1):
        prev = None
        for i in range(a + ch, b, 2):
            v = samples[i]
            if v != v:
                prev = None
                continue
            if prev is not None:
                num += (v - prev) * (v - prev)
            den += v * v
            prev = v
    return num / den if den > 1e-12 else 0.0


def steps(samples, rate, lo, hi):
    """Sample-to-sample discontinuities in [lo,hi) seconds: (time, jump, block_aligned)."""
    a, b = max(0, int(lo * rate)) * 2, min(len(samples), int(hi * rate) * 2)
    found = []
    for ch in (0, 1):
        prev = None
        for i in range(a + ch, b, 2):
            v = samples[i]
            if prev is not None and v == v and prev == prev:
                d = abs(v - prev)
                if d >= STEP:
                    frame = i // 2
                    found.append((frame / rate, d, frame % BLOCK == 0))
            prev = v
    found.sort(key=lambda t: -t[1])
    return found


def load_timeline(path):
    rows = []
    if not os.path.exists(path):
        return rows
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    return rows


def db(x):
    return -math.inf if x <= 0 else 20 * math.log10(x)


def span(rows, kind, lo, hi, key=None):
    """Rows of `kind` whose audio time falls in [lo,hi)."""
    return [r for r in rows if r.get("ev") == kind and lo <= r.get("_at", -1) < hi]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir", nargs="?", default=None)
    ap.add_argument("--around", type=float, default=1.5, help="seconds either side of a mark")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    d = args.dir
    if d is None:
        for guess in ("benilla-config/sound-probe", "../benilla-config/sound-probe"):
            if os.path.isdir(guess):
                d = guess
                break
    if d is None or not os.path.isdir(d):
        sys.exit("soundprobe: no capture directory — pass one (see WOW_SOUND_PROBE)")

    pre_p, post_p = os.path.join(d, "pre.wav"), os.path.join(d, "post.wav")
    if not os.path.exists(pre_p):
        sys.exit(f"soundprobe: {pre_p} missing — was the run started with WOW_SOUND_PROBE=1?")
    rate, pre = read_wav(pre_p)
    _, post = read_wav(post_p) if os.path.exists(post_p) else (rate, array("f"))
    rows = load_timeline(os.path.join(d, "timeline.jsonl"))
    for r in rows:
        r["_at"] = r.get("a", 0) / rate

    pre_env, post_env = envelope(pre, rate), envelope(post, rate)
    dur = len(pre) / 2 / rate
    pre_peak = max((w[1] for w in pre_env), default=0.0)
    post_peak = max((w[1] for w in post_env), default=0.0)
    pre_clip = sum(w[2] for w in pre_env)
    post_clip = sum(w[2] for w in post_env)
    pre_nf = sum(w[3] for w in pre_env)
    post_nf = sum(w[3] for w in post_env)
    ticks = [r for r in rows if r.get("ev") == "tick"]
    overruns = sum(r.get("overruns", 0) for r in ticks)
    refused = sum(r.get("refused", 0) for r in ticks)
    errors = sum(r.get("errors", 0) for r in ticks)
    max_load = max((r.get("load", 0.0) for r in ticks), default=0.0)
    max_voices = max((r.get("voices", 0) for r in ticks), default=0)
    min_gain = min((r.get("gain", 1.0) for r in ticks), default=1.0)
    marks = [r for r in rows if r.get("ev") == "mark"]

    # A brightness baseline from windows spread across the capture, so "harsher than usual" is a
    # statement about this session rather than about audio in general.
    base = []
    if dur > 1.0:
        for k in range(1, 12):
            t0 = dur * k / 12.0
            b = brightness(post, rate, t0, min(dur, t0 + 0.25))
            if b > 0:
                base.append(b)
    base.sort()
    baseline = base[len(base) // 2] if base else 0.0

    # ---- the verdict -----------------------------------------------------------------------
    verdicts = []
    if pre_nf or post_nf:
        verdicts.append(
            f"NON-FINITE SAMPLES — {pre_nf} pre, {post_nf} post. A NaN/inf is invisible to every "
            "level meter and passes the limiter's own test untouched. This is a defect upstream "
            "of the output and is almost certainly what you heard."
        )
    if post_clip:
        verdicts.append(
            f"THE LIMITER IS NOT HOLDING — {post_clip} clipped sample(s) in what was actually "
            f"heard (post peak {post_peak:.3f}). It is installed but something is getting past it."
        )
    elif pre_clip:
        verdicts.append(
            f"over-scale sum, caught — the mix asked for {pre_peak:.2f}x full scale "
            f"({db(pre_peak):+.1f} dBFS) and the output stayed clean (peak {post_peak:.3f}). "
            "1551's mechanism is present and is being handled."
        )
    if overruns:
        verdicts.append(
            f"UNDERRUNS — {overruns} missed mix deadline(s), peak load {max_load*100:.0f}% of "
            "budget. This is audible as a crack and no limiter can touch it."
        )
    if errors:
        verdicts.append(f"STREAM ERRORS — {errors} reported by the device layer.")
    if refused:
        verdicts.append(f"{refused} sound(s) refused — the voice arena filled.")
    if min_gain < 0.5 and not pre_clip:
        verdicts.append(
            f"LIMITER PUMPING WITHOUT CAUSE — gain dove to {min_gain:.2f} ({db(min_gain):+.1f} dB) "
            "with nothing over full scale. The fix would then be the complaint; try "
            "`SoundOutputLimiter 0`."
        )
    if not verdicts:
        verdicts.append(
            "Nothing anomalous in levels, deadlines or finiteness. If you heard it in this "
            "capture, the mechanism is in the waveform's content — check the marks below for "
            "steps, and listen to post.wav at those offsets."
        )

    if args.json:
        print(json.dumps({
            "dir": d, "rate": rate, "duration": dur,
            "pre": {"peak": pre_peak, "clipped": pre_clip, "nonfinite": pre_nf},
            "post": {"peak": post_peak, "clipped": post_clip, "nonfinite": post_nf},
            "overruns": overruns, "refused": refused, "errors": errors,
            "max_load": max_load, "max_voices": max_voices, "min_gain": min_gain,
            "marks": [r["_at"] for r in marks], "verdicts": verdicts,
        }, indent=2))
        return

    print(f"\n  capture   {d}   {dur:.1f}s @ {rate} Hz")
    print(f"  asked for peak {pre_peak:7.3f} ({db(pre_peak):+6.1f} dBFS)  clipped {pre_clip:>8}  nan {pre_nf}")
    print(f"  heard     peak {post_peak:7.3f} ({db(post_peak):+6.1f} dBFS)  clipped {post_clip:>8}  nan {post_nf}")
    print(f"  limiter   deepest gain {min_gain:.3f} ({db(min_gain):+.1f} dB)")
    print(f"  health    overruns {overruns}   refused {refused}   stream errors {errors}"
          f"   peak load {max_load*100:.0f}%   peak voices {max_voices}")
    print("\n  VERDICT")
    for v in verdicts:
        print(f"    • {v}")

    if not marks:
        print("\n  No F9 marks in this capture. The worst moments by level:")
        for t, pk, cl, _nf in sorted(pre_env, key=lambda w: -w[1])[:5]:
            print(f"    {t:8.2f}s  asked {pk:.2f}x  clipped {cl}")
        print()
        return

    print(f"\n  {len(marks)} MARK(S) — what was happening when you pressed F9\n")
    for m in marks:
        at = m["_at"]
        lo, hi = max(0.0, at - args.around), at + args.around
        pw = [w for w in pre_env if lo <= w[0] < hi]
        qw = [w for w in post_env if lo <= w[0] < hi]
        t = [r for r in ticks if lo <= r["_at"] < hi]
        st = steps(post, rate, lo, hi)[:3]
        plays = span(rows, "play", lo, hi)
        print(f"  ── mark {m.get('n')} at {at:.2f}s " + "─" * 46)
        print(f"     asked  peak {max((w[1] for w in pw), default=0):.3f}   clipped {sum(w[2] for w in pw)}")
        print(f"     heard  peak {max((w[1] for w in qw), default=0):.3f}   clipped {sum(w[2] for w in qw)}")
        print(f"     gain   {min((r.get('gain',1.0) for r in t), default=1.0):.3f}"
              f"   voices {max((r.get('voices',0) for r in t), default=0)}"
              f"   load {max((r.get('load',0.0) for r in t), default=0)*100:.0f}%"
              f"   overruns {sum(r.get('overruns',0) for r in t)}"
              f"   refused {sum(r.get('refused',0) for r in t)}")
        harsh = brightness(post, rate, lo, hi)
        if baseline > 0:
            ratio = harsh / baseline
            verdict = (
                "MUCH harsher than this capture's norm — distortion of some kind"
                if ratio >= 2.5
                else "harsher than usual" if ratio >= 1.5
                else "no harsher than usual — this moment was loud, not dirty"
            )
            print(f"     harsh  {harsh:.3f} vs baseline {baseline:.3f} ({ratio:.1f}x) — {verdict}")
        if st:
            worst = ", ".join(
                f"{d_:.2f} at {ts:.3f}s{' [block-aligned]' if al else ''}" for ts, d_, al in st
            )
            print(f"     STEPS  {worst}")
            if any(al for _, _, al in st):
                print("            block-aligned → a starved decoder or a zero-filled chunk,")
                print("            not a level problem. The limiter is irrelevant to this.")
        else:
            print("     steps  none above threshold — the waveform is continuous here")
        if plays:
            names = {}
            for p in plays:
                names[p.get("name", "?")] = names.get(p.get("name", "?"), 0) + 1
            top = sorted(names.items(), key=lambda kv: -kv[1])[:6]
            print(f"     sounds {len(plays)} started: " + ", ".join(f"{n}×{c}" for n, c in top))
        print()


if __name__ == "__main__":
    main()
