#!/usr/bin/env python3
"""Render results.jsonl as small multiples: one panel per operation, one point per step."""
import html, json, os

HERE = os.path.dirname(os.path.abspath(__file__))
METRICS = [
    ("up_again_ms", "stack up", "two services, tools installed"),
    ("down_ms", "stack down", "until every process is gone"),
    ("restart_ms", "stack restart", "both services"),
    ("fleet_up_wall_ms", "6 worktrees up at once", "wall time for the whole fleet"),
    ("exec_ms", "stack exec -- true", "median, services running"),
    ("status_ms", "stack status", "median"),
    ("up_warm_ms", "stack up, already running", "median"),
    ("fleet_exec_wall_ms", "24 execs across 6 worktrees", "wall time, 6 at a time"),
]

def fmt(v):
    return f"{v/1000:.2f} s" if v >= 1000 else f"{v:.0f} ms"

def panel(key, title, sub, rows):
    W, H, L, R, T, B = 340, 150, 12, 64, 14, 22
    vals = [r[key] for r in rows]
    top = max(vals) * 1.12
    n = len(vals)
    xs = [L + (W - L - R) * (i / max(n - 1, 1)) for i in range(n)]
    ys = [T + (H - T - B) * (1 - v / top) for v in vals]
    base = H - B
    pts = " ".join(f"{x:.1f},{y:.1f}" for x, y in zip(xs, ys))
    area = f"{xs[0]:.1f},{base} {pts} {xs[-1]:.1f},{base}"
    dots = "".join(
        f'<g class="pt" tabindex="0"><circle cx="{x:.1f}" cy="{y:.1f}" r="12" fill="transparent"/>'
        f'<circle class="dot" cx="{x:.1f}" cy="{y:.1f}" r="4"/>'
        f'<title>{html.escape(r["label"])}: {fmt(v)}</title></g>'
        for x, y, v, r in zip(xs, ys, vals, rows))
    ticks = "".join(f'<text class="tick" x="{x:.1f}" y="{H-6}" text-anchor="middle">{i}</text>' for i, x in enumerate(xs))
    change = (vals[-1] - vals[0]) / vals[0] * 100
    speed = vals[0] / vals[-1]
    note = f"{speed:.1f}× faster" if speed >= 1.15 else (f"{change:+.0f}%" if abs(change) >= 3 else "unchanged")
    return f'''<figure class="panel"><figcaption><b>{title}</b><span>{sub}</span></figcaption>
<div class="hero">{fmt(vals[-1])}<small>was {fmt(vals[0])} · {note}</small></div>
<svg viewBox="0 0 {W} {H}" role="img" aria-label="{title}: {fmt(vals[0])} to {fmt(vals[-1])}">
<line class="axis" x1="{L}" x2="{W-R+8}" y1="{base}" y2="{base}"/>
<polygon class="area" points="{area}"/><polyline class="line" points="{pts}"/>{dots}
<text class="end" x="{xs[-1]+10:.1f}" y="{ys[-1]+4:.1f}">{fmt(vals[-1])}</text>{ticks}</svg></figure>'''

def main():
    rows = [json.loads(l) for l in open(os.path.join(HERE, "results.jsonl")) if l.strip()]
    panels = "".join(panel(k, t, s, rows) for k, t, s in METRICS)
    steps = "".join(f"<li>{html.escape(r['label'])}</li>" for r in rows)
    table = "".join(
        "<tr><th>" + html.escape(r["label"]) + "</th>" + "".join(f"<td>{fmt(r[k])}</td>" for k, _, _ in METRICS) + "</tr>"
        for r in rows)
    head = "".join(f"<th>{t}</th>" for _, t, _ in METRICS)
    page = f'''<!doctype html><html><head><meta charset="utf-8"><style>
.wrap{{font-family:var(--font-sans);color:var(--foreground)}}
.grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:12px}}
.panel{{margin:0;padding:16px;background:var(--card);border:1px solid var(--border);border-radius:var(--radius)}}
figcaption b{{display:block;font-size:13px;font-weight:600}}
figcaption span{{font-size:12px;color:var(--muted-foreground)}}
.hero{{font-size:26px;font-weight:600;margin:6px 0 2px;font-variant-numeric:tabular-nums}}
.hero small{{font-size:12px;font-weight:400;color:var(--muted-foreground);margin-left:8px}}
svg{{width:100%;height:150px;display:block;overflow:visible}}
.axis{{stroke:var(--border);stroke-width:1}}
.line{{fill:none;stroke:var(--chart-1);stroke-width:2;stroke-linejoin:round;stroke-linecap:round}}
.area{{fill:var(--chart-1);opacity:.12}}
.dot{{fill:var(--chart-1);stroke:var(--card);stroke-width:2}}
.pt:hover .dot,.pt:focus .dot{{r:6}}
.tick,.end{{font-size:11px;fill:var(--muted-foreground);font-family:var(--font-sans)}}
.end{{fill:var(--foreground);font-variant-numeric:tabular-nums}}
ol{{margin:12px 0 0;padding-left:0;list-style:none;counter-reset:s -1;display:flex;flex-wrap:wrap;gap:4px 16px;font-size:12px;color:var(--muted-foreground)}}
ol li{{counter-increment:s}}ol li::before{{content:counter(s) "  ";color:var(--foreground);font-weight:600}}
details{{margin-top:12px;font-size:12px}}summary{{cursor:pointer;color:var(--muted-foreground)}}
.scroll{{overflow-x:auto}}table{{border-collapse:collapse;margin-top:8px;font-variant-numeric:tabular-nums}}
th,td{{padding:4px 10px;text-align:right;border-bottom:1px solid var(--border);white-space:nowrap}}tr th:first-child{{text-align:left}}
</style></head><body><div class="wrap"><div class="grid">{panels}</div>
<ol aria-label="Steps on the horizontal axis">{steps}</ol>
<details><summary>Table of every measurement</summary><div class="scroll"><table><tr><th>Step</th>{head}</tr>{table}</table></div></details></div></body></html>'''
    out = os.path.join(HERE, "hillclimb.html")
    open(out, "w").write(page)
    print(out)

main()
