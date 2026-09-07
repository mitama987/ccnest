"""Analyse a measure2.ps1 run: per phase, child_render / present / total per keystroke.

usage: python analyze2.py <Label> [<Label> ...]
reads  input-<Label>.log, ptydump-<Label>.log, latency-<Label>.log, phases-<Label>.json
"""
import io, json, os, re, statistics, sys

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
HERE = os.path.dirname(os.path.abspath(__file__))


def hms(s):
    m = re.match(r"(?:\S+T)?(\d\d):(\d\d):(\d\d)\.(\d\d\d)", s)
    h, mi, sec, ms = map(int, m.groups())
    return h * 3600 + mi * 60 + sec + ms / 1000.0


def pct(v, q):
    if not v:
        return float("nan")
    s = sorted(v)
    return s[min(len(s) - 1, max(0, int(round(q * len(s) + 0.5)) - 1))]


def stats(v):
    if not v:
        return "n=0"
    return f"n={len(v):2d} p50={statistics.median(v):6.1f} p90={pct(v, 0.9):6.1f} max={max(v):6.1f}"


def analyse(label):
    p = lambda kind: os.path.join(HERE, f"{kind}-{label}.log")
    meta = json.load(open(os.path.join(HERE, f"phases-{label}.json"), encoding="utf-8-sig"))
    symbols = meta["symbols"][: meta["keys"]]
    inputs = [l for l in open(p("input"), encoding="utf-8", errors="replace") if "pending_arrow=" in l]
    dump = [l for l in open(p("ptydump"), encoding="utf-8", errors="replace") if " out " in l]
    draws = [hms(l) for l in open(p("latency"), encoding="utf-8", errors="replace") if " draw " in l]
    outputs = [hms(l) for l in open(p("latency"), encoding="utf-8", errors="replace") if " output pid" in l]
    dump_t = [(hms(l), l) for l in dump]
    moved = sum(1 for l in inputs if "Mouse(Moved" in l)

    print(f"=== {label}  mode={meta['mode']} alt={meta['altScreen']} keys={meta['keys']}  "
          f"draws={len(draws)} outputs={len(outputs)} input-batches={len(inputs)} Mouse(Moved)={moved}")
    for name in ("idle_before", "busy", "idle_after"):
        if name not in meta["phases"]:
            continue
        t0, t1 = (hms(x) for x in meta["phases"][name])
        child, present, total, misses = [], [], [], 0
        for sym in symbols:
            # t_key: first Press of this symbol inside the phase window
            pat = f"Key(Char('{sym}'),Press"
            t_key = next((hms(l) for l in inputs if pat in l and t0 - 0.5 <= hms(l) <= t1 + 0.5), None)
            if t_key is None:
                misses += 1
                continue
            # every byte >= 0x80 is rendered as <xx> in the dump, so a non-ASCII symbol has an
            # unambiguous signature that no VT escape sequence can contain
            needle = "".join(f"<{b:02x}>" for b in sym.encode("utf-8")) if ord(sym) > 127 else sym
            t_echo = next((t for t, l in dump_t if t >= t_key and needle in l), None)
            if t_echo is None or t_echo > t_key + 5:
                misses += 1
                continue
            t_draw = next((t for t in draws if t >= t_echo), None)
            if t_draw is None:
                misses += 1
                continue
            child.append((t_echo - t_key) * 1000)
            present.append((t_draw - t_echo) * 1000)
            total.append((t_draw - t_key) * 1000)
        print(f"  {name:11}  child_render(Claude) {stats(child)}")
        print(f"  {'':11}  present(ccnest)      {stats(present)}")
        print(f"  {'':11}  total                {stats(total)}   unmatched={misses}")
    # mouse-mode timeline
    modes = [(hms(l), m.group(0)) for l in dump for m in [re.search(r"<ESC>\[\?100[0-6][hl]", l)] if m]
    if modes:
        first = modes[0][0]
        print("  mouse-mode seq:", " ".join(f"{t - first:+.1f}s:{s.replace('<ESC>', 'E')}" for t, s in modes[:12]))
    else:
        print("  mouse-mode seq: none (child never enabled mouse tracking)")
    # draw rate during busy phase
    if "busy" in meta["phases"]:
        b0, b1 = (hms(x) for x in meta["phases"]["busy"])
        n = sum(1 for t in draws if b0 <= t <= b1)
        print(f"  draws during busy typing: {n} in {b1 - b0:.1f}s = {n / max(b1 - b0, 0.001):.0f}/s")
    if meta.get("cpu"):
        print("  cpu:", meta["cpu"], " ptyBytesPerPhase:", meta.get("bytes"))


for lab in sys.argv[1:]:
    analyse(lab)
