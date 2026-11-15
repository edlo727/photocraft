"""Measure Nuke's ColorLookup against PhotoCraft's Curves, so the .nk export's curves can be
checked against Nuke itself.

For a few test curves (an S-curve, an HDR range-4 curve, held ends, an overshooting curve), each
is written into a ColorLookup in several ways and sampled over inputs below 0 and past the range:

    baked         what the export writes today: 129 linear keys over the range, plus a key far
                  below and far above, Nuke's default knobs
    baked_range   the same keys with the node's range set to the curve's and the precomputed
                  table off
    points        the curve's own points with the natural spline's exact slopes (editable in
                  Nuke), range set, precomputed table off
    points_auto   the curve's own points with Nuke's automatic tangents

"expected" is PhotoCraft's Curves (a natural cubic spline, continued along its end slopes or
held, as compose::adjust::tone::lookup_tables applies it), computed here in Python.

Run it in Nuke's Script Editor (an empty script is fine; nothing is left behind):
    exec(open(r"F:/Photocraft/photocraft/scripts/nuke_curves_truth.py").read())
or `Nuke<x.y>.exe -t scripts/nuke_curves_truth.py [output.csv]`.

Writes one row per sample: case,style,input,expected,nuke; the first lines are comments with
the Nuke version and ColorLookup's knob names, and a summary of the largest error per style.
"""
import sys

import nuke

DEFAULT_OUT = "F:/Photocraft/photocraft/target/nuke_curves_truth.csv"
WIDTH = 241

# name: (points, range, hold ends)
CASES = {
    "s_curve": ([(0.0, 0.0), (0.25, 0.15), (0.75, 0.85), (1.0, 1.0)], 1.0, False),
    "hdr_range4": ([(0.0, 0.0), (1.0, 1.4), (4.0, 3.2)], 4.0, False),
    "held_ends": ([(0.1, 0.05), (0.5, 0.6), (0.9, 0.95)], 1.0, True),
    "overshoot": ([(0.0, 0.0), (0.2, 0.6), (0.4, 0.3), (1.0, 1.0)], 1.0, False),
}

LUT_SIZE = 4096  # compose::adjust::tone::LUT_SIZE
SLOPE_SPAN = 16  # compose::adjust::tone::TONE_SLOPE_SPAN


def natural_spline(pts):
    """Second derivatives and end slopes of the natural spline (tone::spline_samples)."""
    x = [p[0] for p in pts]
    y = [p[1] for p in pts]
    n = len(pts)
    m2 = [0.0] * n
    if n > 2:
        c = [0.0] * n
        d = [0.0] * n
        for i in range(1, n - 1):
            h0, h1 = x[i] - x[i - 1], x[i + 1] - x[i]
            a, b, cc = h0 / 6.0, (h0 + h1) / 3.0, h1 / 6.0
            r = (y[i + 1] - y[i]) / h1 - (y[i] - y[i - 1]) / h0
            den = b - a * c[i - 1]
            c[i] = cc / den
            d[i] = (r - a * d[i - 1]) / den
        for i in range(n - 2, 0, -1):
            m2[i] = d[i] - c[i] * m2[i + 1]
    return x, y, m2


def spline_at(spl, t, extend):
    x, y, m2 = spl
    n = len(x)
    if t <= x[0]:
        s = (y[1] - y[0]) / (x[1] - x[0]) - (x[1] - x[0]) * m2[1] / 6.0
        return y[0] + (t - x[0]) * s if extend else y[0]
    if t >= x[-1]:
        h = x[-1] - x[-2]
        s = (y[-1] - y[-2]) / h + h * m2[-2] / 6.0
        return y[-1] + (t - x[-1]) * s if extend else y[-1]
    i = 0
    while t > x[i + 1]:
        i += 1
    h = x[i + 1] - x[i]
    a = (x[i + 1] - t) / h
    b = (t - x[i]) / h
    return a * y[i] + b * y[i + 1] + ((a ** 3 - a) * m2[i] + (b ** 3 - b) * m2[i + 1]) * h * h / 6.0


def slope_at(spl, i):
    """The spline's exact slope at point i."""
    x, y, m2 = spl
    n = len(x)
    if i < n - 1:
        h = x[i + 1] - x[i]
        return (y[i + 1] - y[i]) / h - h * (2.0 * m2[i] + m2[i + 1]) / 6.0
    h = x[i] - x[i - 1]
    return (y[i] - y[i - 1]) / h + h * (m2[i - 1] + 2.0 * m2[i]) / 6.0


def photocraft(pts, rng, hold, v):
    """PhotoCraft's lookup: the spline tabulated over 0..range, read with lookup_sample."""
    spl = natural_spline(pts)
    table = [spline_at(spl, k / (LUT_SIZE - 1.0) * rng, not hold) for k in range(LUT_SIZE)]
    t = v / rng
    n = LUT_SIZE
    if hold or 0.0 <= t <= 1.0:
        f = min(max(t, 0.0), 1.0) * (n - 1)
        i = min(int(f), n - 2)
        return table[i] + (table[i + 1] - table[i]) * (f - i)
    k = SLOPE_SPAN
    per_unit = (n - 1) / (k * rng)
    if t < 0.0:
        return table[0] + v * (table[k] - table[0]) * per_unit
    return table[-1] + (v - rng) * (table[-1] - table[-1 - k]) * per_unit


def keys_baked(pts, rng, hold):
    xs = [-100.0 * rng] + [k / 128.0 * rng for k in range(129)] + [100.0 * rng]
    return "{curve L " + " ".join("x%r %r" % (x, photocraft(pts, rng, hold, x)) for x in xs) + "}"


def keys_points(pts, slopes):
    spl = natural_spline(pts)
    parts = []
    for i, (x, y) in enumerate(pts):
        parts.append("x%r %r" % (x, y) + (" s%r" % slope_at(spl, i) if slopes else ""))
    return "{curve " + " ".join(parts) + "}"


IDENTITY = "{curve L x-100000 -100000 x100000 100000}"


def lookup(src, curve, rng=None):
    n = nuke.nodes.ColorLookup(inputs=[src])
    n["lut"].fromScript("master %s red %s green %s blue %s alpha %s" % (IDENTITY, curve, curve, curve, IDENTITY))
    if rng is not None:
        if n.knob("range") is not None:
            n["range"].setValue(rng)
        if n.knob("use_precomputed") is not None:
            n["use_precomputed"].setValue(False)
    return n


def main():
    args = [a for a in sys.argv[1:] if a.lower().endswith(".csv")]
    out_path = args[0] if args else DEFAULT_OUT
    made = []
    try:
        rows, summary = run(made)
    finally:
        for n in made:
            nuke.delete(n)
    with open(out_path, "w") as f:
        f.write("\n".join(rows) + "\n")
    print("\n".join(summary))
    print("wrote %d samples to %s" % (len(rows), out_path))


def run(made):
    try:
        nuke.addFormat("%d 1 1 photocraft_curves" % WIDTH)
    except RuntimeError:
        pass
    probe = nuke.nodes.ColorLookup()
    made.append(probe)
    rows = ["# nuke " + nuke.NUKE_VERSION_STRING,
            "# ColorLookup knobs: " + " ".join(sorted(probe.knobs().keys())),
            "case,style,input,expected,nuke"]
    worst = {}
    for name, (pts, rng, hold) in CASES.items():
        lo, hi = -0.5, rng * 1.6
        base = nuke.nodes.Constant(format="photocraft_curves")
        made.append(base)
        ramp = nuke.nodes.Expression(inputs=[base])
        made.append(ramp)
        expr = "%r + (x + 0.5) / %d * %r" % (lo, WIDTH, hi - lo)
        for k in ("expr0", "expr1", "expr2"):
            ramp[k].setValue(expr)
        ramp["expr3"].setValue("1")
        styles = {
            "baked": lookup(ramp, keys_baked(pts, rng, hold)),
            "baked_range": lookup(ramp, keys_baked(pts, rng, hold), rng),
            "points": lookup(ramp, keys_points(pts, True), rng),
            "points_auto": lookup(ramp, keys_points(pts, False), rng),
        }
        made.extend(styles.values())
        for px in range(WIDTH):
            v = nuke.sample(ramp, "rgba.red", px + 0.5, 0.5)
            want = photocraft(pts, rng, hold, v)
            for style, node in styles.items():
                got = nuke.sample(node, "rgba.red", px + 0.5, 0.5)
                rows.append("%s,%s,%.7g,%.7g,%.7g" % (name, style, v, want, got))
                inside = 0.0 <= v <= rng
                key = (name, style, "inside" if inside else "outside")
                worst[key] = max(worst.get(key, 0.0), abs(got - want))
    summary = ["largest |nuke - photocraft|:"] + ["  %-12s %-12s %-8s %.6f" % (k + (e,)) for k, e in sorted(worst.items())]
    rows[2:2] = ["# " + s for s in summary]
    return rows, summary


main()
