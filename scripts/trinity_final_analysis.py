"""Analisis final dedup del trinity v2 con thresholds ajustados."""

import io
import json
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")

OUT = Path(__file__).resolve().parent.parent / "analysis_output"


def percentile(values, p):
    if not values:
        return 0.0
    s = sorted(values)
    idx = max(0, min(len(s) - 1, int(len(s) * p)))
    return s[idx]


def load(p):
    if not p.exists():
        return []
    with p.open() as f:
        return [json.loads(l) for l in f if l.strip()]


def dedup_enters(enters, window_s=60):
    """Dedup: 1 entry per (market_id, direction) per window_s.
    Returns list de unique entries (primera de cada bucket)."""
    seen = {}  # (mkt, dir) -> last_bucket_time
    out = []
    for e in sorted(enters, key=lambda d: d["timestamp"]):
        mkt = e["market_id"]
        dr = e.get("direction") or "?"
        # Bucket key: market + dir + (timestamp // window)
        ts_min = e["timestamp"][:16]  # YYYY-MM-DDTHH:MM
        key = (mkt, dr, ts_min)
        if key in seen:
            continue
        seen[key] = True
        out.append(e)
    return out


def analyze_strategy(name, decisions):
    enters = [d for d in decisions if d["decision"] == "ENTER"]
    skips = [d for d in decisions if d["decision"] == "SKIP"]
    skip_reasons = Counter(d.get("skip_reason") or "?" for d in skips)
    unique_enters = dedup_enters(enters, window_s=60)
    unique_markets = {e["market_id"] for e in unique_enters}

    pnls = [float(e["expected_pnl_usdc"]) for e in unique_enters]
    sizes = [float(e["size_usdc"]) for e in unique_enters]
    edges = [float(e["edge_per_unit"]) for e in unique_enters if float(e["edge_per_unit"]) > 0]
    sum_asks = [float(e["sum_ask"]) for e in unique_enters if float(e["sum_ask"]) > 0]
    ttrs = [e["ttr_seconds"] for e in unique_enters if e.get("ttr_seconds") is not None]
    direction = Counter(e.get("direction") or "?" for e in unique_enters)
    crypto = Counter()
    for e in unique_enters:
        slug = e.get("market_slug", "").lower()
        for c in ("btc", "eth", "sol", "xrp"):
            if c in slug:
                crypto[c] += 1
                break

    return {
        "name": name,
        "raw_decisions": len(decisions),
        "raw_enters": len(enters),
        "raw_skips": len(skips),
        "unique_enters": len(unique_enters),
        "unique_markets": len(unique_markets),
        "skip_reasons": dict(skip_reasons.most_common(8)),
        "pnl_total": sum(pnls),
        "pnl_median": percentile(pnls, 0.5) if pnls else 0,
        "size_total": sum(sizes),
        "size_median": percentile(sizes, 0.5) if sizes else 0,
        "edge_median_pct": percentile(edges, 0.5) * 100 if edges else 0,
        "edge_max_pct": max(edges) * 100 if edges else 0,
        "sum_ask_median": percentile(sum_asks, 0.5) if sum_asks else 0,
        "ttr_median": percentile(ttrs, 0.5) if ttrs else 0,
        "directions": dict(direction),
        "crypto": dict(crypto),
        "samples": unique_enters[:5],
    }


def main():
    sA = analyze_strategy("A — Lag Arb (momentum)", load(OUT / "trinity_A.jsonl"))
    sB = analyze_strategy("B — Bilateral Puro", load(OUT / "trinity_B.jsonl"))
    sC = analyze_strategy("C — Hibrido (B + CEX veto)", load(OUT / "trinity_C.jsonl"))

    print("=" * 70)
    print("TRINITY FINAL ANALYSIS (deduplicado por market+minuto)")
    print("=" * 70)
    print()
    print(f"{'Strategy':40} | Enters únicos | PnL teórico | Edge median")
    print("-" * 80)
    for s in (sA, sB, sC):
        print(
            f"{s['name'][:40]:40} | {s['unique_enters']:13} | "
            f"${s['pnl_total']:11.4f} | {s['edge_median_pct']:8.2f}%"
        )

    for s in (sA, sB, sC):
        print()
        print(f"{'─' * 70}")
        print(f"{s['name']}")
        print(f"{'─' * 70}")
        print(f"  Raw decisions (con spam):   {s['raw_decisions']}")
        print(f"  Raw enters:                 {s['raw_enters']}")
        print(f"  ENTERS unicos (dedup):      {s['unique_enters']}")
        print(f"  Markets unicos con entry:   {s['unique_markets']}")
        print(f"  PnL teorico total:          ${s['pnl_total']:.4f}")
        print(f"  PnL teorico median/entry:   ${s['pnl_median']:.4f}")
        print(f"  Notional total:             ${s['size_total']:.2f}")
        print(f"  Size median:                ${s['size_median']:.2f}")
        print(f"  Edge median:                {s['edge_median_pct']:.2f}%")
        print(f"  Edge max:                   {s['edge_max_pct']:.2f}%")
        print(f"  Sum_ask median:             {s['sum_ask_median']:.4f}")
        print(f"  TTR median:                 {s['ttr_median']}s")
        print(f"  Direcciones:                {s['directions']}")
        print(f"  Cryptos:                    {s['crypto']}")
        print(f"  Top skip reasons:")
        for r, n in list(s["skip_reasons"].items())[:5]:
            print(f"    {r}: {n}")

        if s["samples"]:
            print(f"  Sample ENTERS:")
            for i, e in enumerate(s["samples"][:3]):
                print(
                    f"    [{i+1}] {e['market_slug'][:45]:45} "
                    f"{e.get('direction','?'):4} "
                    f"edge={float(e['edge_per_unit']) * 100:5.2f}% "
                    f"sum={float(e['sum_ask']):.4f} "
                    f"size=${float(e['size_usdc']):.2f} "
                    f"pnl=${float(e['expected_pnl_usdc']):.4f}"
                )

    # Proyeccion 1h
    print()
    print("=" * 70)
    print("PROYECCIONES (a 1h, asumiendo throughput constante)")
    print("=" * 70)
    # 2 min run, escalar 30x
    for s in (sA, sB, sC):
        proj_pnl = s["pnl_total"] * 30
        proj_enters = s["unique_enters"] * 30
        print(f"  {s['name'][:38]:38} → {proj_enters:5} enters/h, ${proj_pnl:.2f}/h teórico")


if __name__ == "__main__":
    main()
