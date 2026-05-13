"""Drill-down sobre wallet_events.jsonl. Analisis por-market: secuencia
de trades, scaling de sizes vs precio, position lifecycle, accumulator
hypothesis test.
"""

import io
import json
import statistics
import sys
from collections import defaultdict
from pathlib import Path

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")

OUT = Path(__file__).resolve().parent.parent / "analysis_output"


def main() -> None:
    events_path = OUT / "wallet_events.jsonl"
    markets_path = OUT / "wallet_markets.json"
    with events_path.open() as f:
        events = [json.loads(line) for line in f if line.strip()]
    with markets_path.open() as f:
        meta = json.load(f)

    trades = [e for e in events if e["type"] == "TRADE"]
    print(f"Total trades en cache: {len(trades)}")

    # Per-market analysis
    by_market: dict[str, list[dict]] = defaultdict(list)
    for t in trades:
        if t.get("market_id"):
            by_market[t["market_id"]].append(t)

    # Orden ascendente temporal
    for mid in by_market:
        by_market[mid].sort(key=lambda x: x["timestamp"])

    print(f"Markets unicos: {len(by_market)}")
    print()
    print("=" * 70)
    print("DISTRIBUCION DE TRADES POR MARKET")
    print("=" * 70)
    counts = [len(v) for v in by_market.values()]
    counts.sort(reverse=True)
    print(f"  Total trades:      {sum(counts)}")
    print(f"  Mediana per mkt:   {statistics.median(counts):.0f}")
    print(f"  Max per mkt:       {max(counts)}")
    print(f"  Min per mkt:       {min(counts)}")
    print()
    # Top 10
    top10 = sorted(by_market.items(), key=lambda x: -len(x[1]))[:10]
    print("Top 10 markets por # trades:")
    for mid, ts in top10:
        m = meta.get(mid, {})
        notional = sum(float(t["price"] or 0) * float(t["size"]) for t in ts)
        print(f"  {len(ts):4} trades  ${notional:8.0f}  {m.get('slug', mid[:16])[:50]}")

    print()
    print("=" * 70)
    print("ACCUMULATOR HYPOTHESIS TEST")
    print("=" * 70)
    # Para cada market con >=5 trades, ver:
    # 1. Cuantos YES vs NO
    # 2. Si el avg_cost YES + avg_cost NO < 1
    # 3. min(qty_yes, qty_no) = guaranteed pairs
    test_markets = [(mid, ts) for mid, ts in by_market.items() if len(ts) >= 5]
    print(f"Markets con >=5 trades: {len(test_markets)}")
    print()

    sample_count = 0
    pair_costs_at_close = []
    asymmetric_count = 0
    for mid, ts in sorted(test_markets, key=lambda x: -len(x[1]))[:30]:
        m = meta.get(mid, {})
        slug = (m.get("slug") or mid[:16])[:40]
        # Separar por asset
        by_asset = defaultdict(list)
        for t in ts:
            by_asset[t["asset_id"]].append(t)
        if len(by_asset) < 2:
            asymmetric_count += 1
            if sample_count < 3:
                a = next(iter(by_asset.values()))
                notional = sum(float(t["price"] or 0) * float(t["size"]) for t in a)
                print(f"  [ASYM] {slug}: solo 1 asset, {len(a)} trades, ${notional:.0f}")
                sample_count += 1
            continue
        # Compute avg cost per asset
        asset_stats = {}
        for aid, atrades in by_asset.items():
            total_qty = sum(float(t["size"]) for t in atrades)
            total_cost = sum(float(t["size"]) * float(t["price"] or 0) for t in atrades)
            avg = total_cost / total_qty if total_qty else 0
            asset_stats[aid] = {
                "qty": total_qty,
                "cost": total_cost,
                "avg": avg,
                "n": len(atrades),
            }
        assets = list(asset_stats.values())
        if len(assets) == 2:
            pair_cost = assets[0]["avg"] + assets[1]["avg"]
            guaranteed_pairs = min(assets[0]["qty"], assets[1]["qty"])
            total_invested = assets[0]["cost"] + assets[1]["cost"]
            guaranteed_payout = guaranteed_pairs * 1.0  # $1 por par
            pnl_locked = guaranteed_payout - total_invested + (
                # cobertura de la pierna excedente: el extra qty vale 0 o 1 al resolver
                # asumo conservador: vale 0 (peor caso)
                0
            )
            pair_costs_at_close.append(pair_cost)
            if sample_count < 10:
                print(f"  {slug[:38]}")
                print(
                    f"    Asset A: {assets[0]['n']:3} trades, "
                    f"qty={assets[0]['qty']:7.0f}, avg=${assets[0]['avg']:.4f}, "
                    f"cost=${assets[0]['cost']:.2f}"
                )
                print(
                    f"    Asset B: {assets[1]['n']:3} trades, "
                    f"qty={assets[1]['qty']:7.0f}, avg=${assets[1]['avg']:.4f}, "
                    f"cost=${assets[1]['cost']:.2f}"
                )
                print(
                    f"    pair_cost (sum avg): {pair_cost:.4f}  "
                    f"guaranteed_pairs: {guaranteed_pairs:.0f}  "
                    f"PnL_min: ${pnl_locked:.2f}"
                )
                sample_count += 1

    if pair_costs_at_close:
        print()
        print(f"  Pair costs cerrados (sum avg YES + avg NO):")
        print(f"    N: {len(pair_costs_at_close)}")
        print(f"    median: {statistics.median(pair_costs_at_close):.4f}")
        print(
            f"    min: {min(pair_costs_at_close):.4f}, max: {max(pair_costs_at_close):.4f}"
        )
        below_1 = sum(1 for p in pair_costs_at_close if p < 1.0)
        below_099 = sum(1 for p in pair_costs_at_close if p < 0.99)
        below_098 = sum(1 for p in pair_costs_at_close if p < 0.98)
        below_097 = sum(1 for p in pair_costs_at_close if p < 0.97)
        print(f"    pair_cost < 1.00:  {below_1}/{len(pair_costs_at_close)} ({below_1 / len(pair_costs_at_close):.1%})")
        print(f"    pair_cost < 0.99:  {below_099}/{len(pair_costs_at_close)} ({below_099 / len(pair_costs_at_close):.1%})")
        print(f"    pair_cost < 0.98:  {below_098}/{len(pair_costs_at_close)} ({below_098 / len(pair_costs_at_close):.1%})")
        print(f"    pair_cost < 0.97:  {below_097}/{len(pair_costs_at_close)} ({below_097 / len(pair_costs_at_close):.1%})")

    if asymmetric_count:
        print(f"  Markets con SOLO 1 lado (NO bilateral): {asymmetric_count}")

    # Sizing vs price: scatter check
    print()
    print("=" * 70)
    print("SIZING vs PRECIO (scaling pattern)")
    print("=" * 70)
    # Buckets por precio
    buckets: dict[str, list[float]] = defaultdict(list)
    for t in trades:
        p = float(t["price"] or 0)
        s = float(t["size"])
        if p <= 0:
            continue
        if p < 0.10:
            b = "0.00-0.10"
        elif p < 0.20:
            b = "0.10-0.20"
        elif p < 0.30:
            b = "0.20-0.30"
        elif p < 0.40:
            b = "0.30-0.40"
        elif p < 0.50:
            b = "0.40-0.50"
        elif p < 0.60:
            b = "0.50-0.60"
        elif p < 0.70:
            b = "0.60-0.70"
        elif p < 0.80:
            b = "0.70-0.80"
        elif p < 0.90:
            b = "0.80-0.90"
        else:
            b = "0.90-1.00"
        buckets[b].append(s)
    print(f"  Bucket          N trades   median size   p90 size   max size")
    for b in sorted(buckets):
        s = buckets[b]
        if not s:
            continue
        print(
            f"  {b}    {len(s):6}     "
            f"{statistics.median(s):8.1f}   "
            f"{s[int(len(s) * 0.9) if len(s) > 1 else 0]:8.1f}   "
            f"{max(s):.0f}"
        )

    # Velocidad por minuto del run
    print()
    print("=" * 70)
    print("CLUSTER FINO — TRADES POR MINUTO")
    print("=" * 70)
    by_minute: dict[str, int] = defaultdict(int)
    for t in trades:
        key = t["timestamp"][:16]  # "2026-05-12T21:35"
        by_minute[key] += 1
    sorted_minutes = sorted(by_minute.items())
    if sorted_minutes:
        max_n = max(by_minute.values())
        print(f"  N minutos con actividad: {len(sorted_minutes)}")
        print(f"  Max trades/min:          {max_n}")
        # render histogram solo de los con actividad
        print()
        print("  Activity histogram (cada linea = 1 min):")
        for k, n in sorted_minutes[:80]:
            bar = "█" * int(50 * n / max_n) if n else ""
            print(f"  {k}  {n:3}  {bar}")


if __name__ == "__main__":
    main()
