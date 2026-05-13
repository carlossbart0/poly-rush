"""Análisis comparativo post-run del trinity harness.

Lee los 3 JSONLs (trinity_A.jsonl, trinity_B.jsonl, trinity_C.jsonl) y
produce un reporte comparativo:

1. Conteos por strategy: ENTER vs SKIP, razones de SKIP
2. PnL teórico acumulado por strategy
3. Distribución de edge, size, time-to-resolution
4. Top markets por strategy
5. Crossover analysis: ¿cuándo A y B coincidieron? ¿cuándo C vetó a B?
6. Latencia: tiempos entre evaluaciones
7. Asset selection: qué crypto por strategy
8. Best/worst entries

Output: stdout + analysis_output/trinity_report.md
"""

from __future__ import annotations

import io
import json
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")

OUT = Path(__file__).resolve().parent.parent / "analysis_output"


def load(path: Path) -> list[dict]:
    if not path.exists():
        return []
    with path.open() as f:
        return [json.loads(line) for line in f if line.strip()]


def percentile(values, p):
    if not values:
        return 0.0
    s = sorted(values)
    idx = max(0, min(len(s) - 1, int(len(s) * p)))
    return s[idx]


def fmt_money(x: float) -> str:
    return f"${x:,.2f}"


def analyze(name: str, decisions: list[dict]) -> dict:
    """Compute summary metrics for a strategy."""
    enters = [d for d in decisions if d["decision"] == "ENTER"]
    skips = [d for d in decisions if d["decision"] == "SKIP"]
    skip_reasons = Counter(d.get("skip_reason") or "unknown" for d in skips)
    edges = [float(d["edge_per_unit"]) for d in enters if float(d["edge_per_unit"]) > 0]
    sizes = [float(d["size_usdc"]) for d in enters if float(d["size_usdc"]) > 0]
    pnls = [float(d["expected_pnl_usdc"]) for d in enters]
    notionals = [float(d["size_usdc"]) for d in enters]
    ttrs = [d["ttr_seconds"] for d in enters if d.get("ttr_seconds") is not None]
    direction_counter = Counter(d.get("direction") or "?" for d in enters)
    markets = Counter(d["market_id"][:18] for d in enters)
    crypto = Counter()
    for d in enters:
        slug = d.get("market_slug", "").lower()
        for c in ("btc", "eth", "sol", "xrp", "doge", "bnb", "hype"):
            if c in slug:
                crypto[c] += 1
                break

    cex_edges = [d.get("cex_edge") for d in enters if d.get("cex_edge") is not None]
    p_models = [d.get("p_model_yes") for d in enters if d.get("p_model_yes") is not None]
    sigmas = [d.get("cex_sigma_annual") for d in enters if d.get("cex_sigma_annual") is not None]

    return {
        "name": name,
        "n_decisions_logged": len(decisions),
        "n_enters": len(enters),
        "n_skips": len(skips),
        "skip_reasons": dict(skip_reasons.most_common()),
        "edge_median": percentile(edges, 0.5) if edges else 0,
        "edge_p90": percentile(edges, 0.9) if edges else 0,
        "edge_max": max(edges) if edges else 0,
        "size_median": percentile(sizes, 0.5) if sizes else 0,
        "size_max": max(sizes) if sizes else 0,
        "pnl_total": sum(pnls),
        "pnl_median_per_entry": percentile(pnls, 0.5) if pnls else 0,
        "notional_total": sum(notionals),
        "ttr_median": percentile(ttrs, 0.5) if ttrs else 0,
        "ttr_p10": percentile(ttrs, 0.1) if ttrs else 0,
        "directions": dict(direction_counter),
        "top_markets": markets.most_common(5),
        "crypto_split": dict(crypto.most_common()),
        "cex_edge_median": percentile(cex_edges, 0.5) if cex_edges else None,
        "p_model_median": percentile(p_models, 0.5) if p_models else None,
        "sigma_median": percentile(sigmas, 0.5) if sigmas else None,
        "enters": enters,  # para crossover analysis después
    }


def crossover(a_enters, b_enters, c_enters):
    """Analiza overlap entre las 3 strategies."""
    a_keys = {(e["market_id"], e["timestamp"][:19]) for e in a_enters}
    b_keys = {(e["market_id"], e["timestamp"][:19]) for e in b_enters}
    c_keys = {(e["market_id"], e["timestamp"][:19]) for e in c_enters}
    # Markets unicos por strategy (no timestamps)
    a_mkts = {e["market_id"] for e in a_enters}
    b_mkts = {e["market_id"] for e in b_enters}
    c_mkts = {e["market_id"] for e in c_enters}
    return {
        "a_only_mkts": len(a_mkts - b_mkts - c_mkts),
        "b_only_mkts": len(b_mkts - a_mkts - c_mkts),
        "c_only_mkts": len(c_mkts - a_mkts - b_mkts),
        "a_and_b_mkts": len(a_mkts & b_mkts),
        "a_and_c_mkts": len(a_mkts & c_mkts),
        "b_and_c_mkts": len(b_mkts & c_mkts),
        "all_three_mkts": len(a_mkts & b_mkts & c_mkts),
        "total_unique_mkts": len(a_mkts | b_mkts | c_mkts),
        "a_only_decisions": len(a_keys - b_keys - c_keys),
        "b_only_decisions": len(b_keys - a_keys - c_keys),
        "c_only_decisions": len(c_keys - a_keys - b_keys),
    }


def print_strategy(s, f):
    p = lambda *a, **k: print(*a, **k) or print(*a, **k, file=f)
    f.write(f"\n## STRATEGY {s['name']}\n\n")
    print(f"\n{'─' * 70}")
    print(f"STRATEGY {s['name']}")
    print(f"{'─' * 70}")
    print(f"  Decisions logged:    {s['n_decisions_logged']}")
    print(f"  ENTERS:              {s['n_enters']}")
    print(f"  SKIPS logged:        {s['n_skips']}")
    if s["skip_reasons"]:
        print(f"  Top skip reasons:")
        for r, n in list(s["skip_reasons"].items())[:5]:
            print(f"    {r}: {n}")
    print(f"  PnL teorico total:   ${s['pnl_total']:.4f}")
    print(f"  PnL median/entry:    ${s['pnl_median_per_entry']:.4f}")
    print(f"  Notional rotado:     ${s['notional_total']:.2f}")
    print(f"  Edge median:         {s['edge_median'] * 100:.2f}%")
    print(f"  Edge p90:            {s['edge_p90'] * 100:.2f}%")
    print(f"  Edge max:            {s['edge_max'] * 100:.2f}%")
    print(f"  Size median:         ${s['size_median']:.2f}")
    print(f"  Size max:            ${s['size_max']:.2f}")
    print(f"  TTR median:          {s['ttr_median']}s")
    print(f"  TTR p10:             {s['ttr_p10']}s")
    print(f"  Directions:          {s['directions']}")
    print(f"  Crypto split:        {s['crypto_split']}")
    if s["cex_edge_median"] is not None:
        print(f"  CEX edge median:     {s['cex_edge_median']:.4f}")
        print(f"  P_model median:      {s['p_model_median']:.4f}")
        print(f"  Sigma median:        {s['sigma_median']:.4f}")
    print(f"  Top 5 markets:")
    for mid, n in s["top_markets"]:
        print(f"    {mid}: {n}")

    # markdown
    f.write(f"| Métrica | Valor |\n|---|---|\n")
    f.write(f"| Decisions logged | {s['n_decisions_logged']} |\n")
    f.write(f"| ENTERS | **{s['n_enters']}** |\n")
    f.write(f"| SKIPS logged | {s['n_skips']} |\n")
    f.write(f"| PnL teórico total | **${s['pnl_total']:.4f}** |\n")
    f.write(f"| PnL median/entry | ${s['pnl_median_per_entry']:.4f} |\n")
    f.write(f"| Notional rotado | ${s['notional_total']:.2f} |\n")
    f.write(f"| Edge median | {s['edge_median'] * 100:.2f}% |\n")
    f.write(f"| Edge max | {s['edge_max'] * 100:.2f}% |\n")
    f.write(f"| Size median | ${s['size_median']:.2f} |\n")
    f.write(f"| Size max | ${s['size_max']:.2f} |\n")
    f.write(f"| TTR median | {s['ttr_median']}s |\n")
    if s["directions"]:
        f.write(f"| Directions | {s['directions']} |\n")
    if s["crypto_split"]:
        f.write(f"| Crypto split | {s['crypto_split']} |\n")
    if s["cex_edge_median"] is not None:
        f.write(f"| CEX edge median | {s['cex_edge_median']:.4f} |\n")
        f.write(f"| P_model median | {s['p_model_median']:.4f} |\n")
        f.write(f"| σ (annual) median | {s['sigma_median']:.4f} |\n")
    f.write(f"\nTop skip reasons:\n")
    for r, n in list(s["skip_reasons"].items())[:5]:
        f.write(f"- `{r}`: {n}\n")


def main():
    a_data = load(OUT / "trinity_A.jsonl")
    b_data = load(OUT / "trinity_B.jsonl")
    c_data = load(OUT / "trinity_C.jsonl")

    report_path = OUT / "trinity_report.md"

    print("=" * 70)
    print("TRINITY ANALYSIS — A vs B vs C")
    print("=" * 70)
    print(f"  A decisions: {len(a_data)}")
    print(f"  B decisions: {len(b_data)}")
    print(f"  C decisions: {len(c_data)}")

    if not (a_data or b_data or c_data):
        print()
        print("Sin decisions en ningún jsonl. ¿Bot corrió?")
        return

    sA = analyze("A — Lag Arb Direccional (CEX)", a_data)
    sB = analyze("B — Bilateral Puro Mejorado", b_data)
    sC = analyze("C — Híbrido (Bilateral + CEX veto)", c_data)

    with report_path.open("w", encoding="utf-8") as f:
        f.write("# Trinity Harness — Reporte Comparativo\n\n")
        f.write("Run de 10 minutos en DRY-RUN puro. 3 strategies sobre el mismo feed Polymarket + Coinbase.\n\n")
        f.write("## Resumen ejecutivo\n\n")
        f.write("| Strategy | ENTERS | PnL teórico | Edge median | Size median |\n")
        f.write("|---|---|---|---|---|\n")
        for s in (sA, sB, sC):
            f.write(
                f"| {s['name']} | {s['n_enters']} | "
                f"${s['pnl_total']:.4f} | {s['edge_median'] * 100:.2f}% | "
                f"${s['size_median']:.2f} |\n"
            )
        f.write("\n")

        for s in (sA, sB, sC):
            print_strategy(s, f)

        # CROSSOVER
        co = crossover(sA["enters"], sB["enters"], sC["enters"])
        print()
        print("─" * 70)
        print("CROSSOVER ANALYSIS")
        print("─" * 70)
        print(f"  Markets totales que tuvieron ENTRY (algún strategy): {co['total_unique_mkts']}")
        print(f"  Solo A entró:        {co['a_only_mkts']}")
        print(f"  Solo B entró:        {co['b_only_mkts']}")
        print(f"  Solo C entró:        {co['c_only_mkts']}")
        print(f"  A & B en mismo mkt:  {co['a_and_b_mkts']}")
        print(f"  A & C en mismo mkt:  {co['a_and_c_mkts']}")
        print(f"  B & C en mismo mkt:  {co['b_and_c_mkts']}")
        print(f"  Las 3 en mismo mkt:  {co['all_three_mkts']}")

        f.write(f"\n## Crossover analysis\n\n")
        f.write(f"| Métrica | Valor |\n|---|---|\n")
        f.write(f"| Markets totales con ENTRY | {co['total_unique_mkts']} |\n")
        f.write(f"| Solo A | {co['a_only_mkts']} |\n")
        f.write(f"| Solo B | {co['b_only_mkts']} |\n")
        f.write(f"| Solo C | {co['c_only_mkts']} |\n")
        f.write(f"| A ∩ B | {co['a_and_b_mkts']} |\n")
        f.write(f"| A ∩ C | {co['a_and_c_mkts']} |\n")
        f.write(f"| B ∩ C | {co['b_and_c_mkts']} |\n")
        f.write(f"| A ∩ B ∩ C | {co['all_three_mkts']} |\n")

        # === ANALISIS C vs B ===
        # C es subset de B. ¿Cuántas opps de B vetó C?
        c_skips_cex = sum(
            1 for d in c_data
            if d["decision"] == "SKIP" and d.get("skip_reason") == "cex_disagrees"
        )
        f.write(f"\n## Eficacia del veto CEX (Strategy C)\n\n")
        f.write(f"- Opps que C vetó por desacuerdo CEX: **{c_skips_cex}**\n")
        f.write(f"- Si C es subset estricto de B, B-C = {sB['n_enters'] - sC['n_enters']} entradas extras de B\n")
        print()
        print(f"  C vetó por desacuerdo CEX: {c_skips_cex} opps")

        # BEST/WORST ENTRIES
        f.write(f"\n## Top 5 entradas por PnL teórico — Strategy B\n\n")
        f.write(f"| Mkt | Edge | Size $ | PnL teo $ |\n|---|---|---|---|\n")
        top_b = sorted(sB["enters"], key=lambda d: -float(d["expected_pnl_usdc"]))[:5]
        for d in top_b:
            f.write(
                f"| {d['market_id'][:14]} | {float(d['edge_per_unit']) * 100:.2f}% | "
                f"{float(d['size_usdc']):.2f} | {float(d['expected_pnl_usdc']):.4f} |\n"
            )

        # RECOMENDACION FINAL
        ranked = sorted([sA, sB, sC], key=lambda s: -s["pnl_total"])
        f.write(f"\n## Veredicto\n\n")
        f.write(f"Ranking PnL teórico (10 min):\n\n")
        for i, s in enumerate(ranked, 1):
            f.write(f"{i}. **{s['name']}**: ${s['pnl_total']:.4f} ({s['n_enters']} entradas)\n")

        f.write(f"\n**Ganador**: {ranked[0]['name']}\n\n")

        # Honest qualifier
        if sA["n_enters"] == 0:
            f.write("⚠️ A no entró ninguna vez. Posibles causas: σ alta, "
                    "strikes no resolvieron (markets recién iniciados), edge threshold "
                    "10% demasiado estricto para condiciones actuales.\n")
        if sC["n_enters"] == 0:
            f.write("⚠️ C no entró ninguna vez. Si B sí entró, "
                    "el CEX veto descartó todas las opps de B.\n")

    print()
    print("=" * 70)
    print(f"Reporte completo: {report_path}")
    print("=" * 70)


if __name__ == "__main__":
    main()
