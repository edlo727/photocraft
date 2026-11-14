"""Measure Nuke's Merge2 operations, so PhotoCraft's Nuke blend modes can be checked against Nuke
itself rather than its documentation.

Each case is built the way PhotoCraft's .nk export builds a layer: a straight-alpha colour, then
Premult, then Merge2 (B = the comp, A = the layer). Every operation is measured with alpha masking
(`screen_alpha`) off, and the non-Porter-Duff ones with it on as well, since the export turns it on
for some of them. Colours include light above 1 and an ACEScg-style negative.

Run it either way:
    nuke -t scripts/nuke_merge_truth.py [output.csv]
or in Nuke's Script Editor: File > Source File... (writes to the default path below).

Output, one row per measurement:
    op,screen_alpha,A,a,B,b,out_r,out_g,out_b,out_a
A and B are straight colours, a and b their alphas, out_* Nuke's premultiplied result.
"""
import sys

import nuke

DEFAULT_OUT = "F:/Photocraft/photocraft/target/nuke_truth.csv"

OPS = [
    "atop", "average", "color-burn", "color-dodge", "conjoint-over", "copy", "difference",
    "disjoint-over", "divide", "exclusion", "from", "geometric", "hard-light", "hypot", "in",
    "mask", "matte", "max", "min", "minus", "multiply", "out", "over", "overlay", "plus",
    "screen", "soft-light", "stencil", "under", "xor",
]
# Operations that combine coverage themselves; the export never sets alpha masking on them.
PORTER_DUFF = {"over", "under", "in", "out", "atop", "xor", "mask", "stencil", "matte",
               "conjoint-over", "disjoint-over", "copy"}
COLOURS = [-0.25, 0.2, 0.8, 2.0]
ALPHAS = [0.0, 0.3, 1.0]


def build():
    try:
        nuke.addFormat("4 4 1 photocraft_truth")
    except RuntimeError:
        pass  # already defined in this session
    a_src = nuke.nodes.Constant(name="TruthA")
    b_src = nuke.nodes.Constant(name="TruthB")
    for node in (a_src, b_src):
        node["format"].setValue("photocraft_truth")
    a_pm = nuke.nodes.Premult(inputs=[a_src])
    b_pm = nuke.nodes.Premult(inputs=[b_src])
    merge = nuke.nodes.Merge2(inputs=[b_pm, a_pm])  # input 0 is B, input 1 is A
    return a_src, b_src, merge


def sample(node):
    return [nuke.sample(node, "rgba." + c, 2.0, 2.0) for c in ("red", "green", "blue", "alpha")]


def main():
    # Only a .csv argument is an output path: in the GUI, argv holds the .nk Nuke opened,
    # and writing there would overwrite the comp.
    args = [a for a in sys.argv[1:] if a.lower().endswith(".csv")]
    out_path = args[0] if args else DEFAULT_OUT
    a_src, b_src, merge = build()
    try:
        run(a_src, b_src, merge, out_path)
    finally:
        for node in (merge, a_src, b_src):
            for up in node.dependencies():
                if up.Class() == "Premult":
                    nuke.delete(up)
            nuke.delete(node)  # leave the open comp as it was


def run(a_src, b_src, merge, out_path):
    rows = ["# nuke " + nuke.NUKE_VERSION_STRING,
            "op,screen_alpha,A,a,B,b,out_r,out_g,out_b,out_a"]
    for op in OPS:
        merge["operation"].setValue(op)
        for masked in ([False] if op in PORTER_DUFF else [False, True]):
            merge["screen_alpha"].setValue(masked)
            for ca in COLOURS:
                for a in ALPHAS:
                    a_src["color"].setValue([ca, ca, ca, a])
                    for cb in COLOURS:
                        for b in ALPHAS:
                            b_src["color"].setValue([cb, cb, cb, b])
                            out = sample(merge)
                            rows.append(",".join([op, "1" if masked else "0"]
                                                 + ["%r" % v for v in (ca, a, cb, b)]
                                                 + ["%.9g" % v for v in out]))
        print("measured", op)
    with open(out_path, "w") as f:
        f.write("\n".join(rows) + "\n")
    print("wrote %d measurements to %s" % (len(rows) - 2, out_path))


main()
