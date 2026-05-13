"""Forensic analysis HFT-grade de la wallet 0xeebde7a0e019a63e6b476eb425505b7b3e6eba30.

Objetivo: extraer un blueprint replicable de la estrategia del bot.

Mediciones:
1. Volumen y frecuencia (trades/h, /min, /s, distribución por hora)
2. Inter-arrival times (gaps temporales entre trades)
3. Bilateral pairing (YES+NO mismo market, delta_t, ratio sizes)
4. Spread analysis (sum YES + NO al entrar, edge bruto)
5. Market timing (time-to-resolution al entrar)
6. Position sizing (notional, shares, escalado)
7. Asset selection (crypto/duración)
8. Cluster temporal (ráfagas vs sostenido)
9. Exit behavior (BUY/SELL ratio, hold-to-resolve)
10. Latency vs eventos macro (best effort)

Output:
- analysis_output/wallet_events.jsonl (raw events)
- analysis_output/wallet_markets.json (metadata)
- analysis_output/wallet_report.md (sintesis)

NO toca state/bot.db ni la estrategia del Python bot. Read-only.
"""

from __future__ import annotations

import asyncio
import io
import json
import statistics
import sys
from collections import Counter, defaultdict

# Forzar UTF-8 en stdout para que los box chars no rompan en Windows cp1252.
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
from datetime import UTC, datetime, timedelta
from decimal import Decimal
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "bot-polymarket" / "src"))

from polymarket_bot.data import DataApiClient, GammaClient  # noqa: E402

WALLET = "0xeebde7a0e019a63e6b476eb425505b7b3e6eba30"
HOURS_LOOKBACK = 24
OUT_DIR = Path(__file__).resolve().parent.parent / "analysis_output"


async def fetch_all_activity(api: DataApiClient, wallet: str, hours: int) -> list[Any]:
    """Paginar Data API hasta cubrir ventana N horas o tope 3000 (API cap)."""
    cutoff = datetime.now(UTC) - timedelta(hours=hours)
    all_events: list[Any] = []
    offset = 0
    page_size = 100
    while True:
        page = await api.get_activity(wallet, limit=page_size, offset=offset)
        if not page:
            break
        all_events.extend(page)
        last_ts = page[-1].timestamp
        if last_ts < cutoff:
            break
        offset += page_size
        if offset >= 2900:
            break
    return [e for e in all_events if e.timestamp >= cutoff]


async def fetch_market_metadata(
    gamma: GammaClient, condition_ids: list[str]
) -> dict[str, dict[str, Any]]:
    out: dict[str, dict[str, Any]] = {}
    for cid in condition_ids:
        try:
            m = await gamma.get_market(cid)
            out[cid] = {
                "slug": m.slug,
                "question": m.question,
                "end_date": m.end_date.isoformat() if m.end_date else None,
                "closed": m.closed,
                "neg_risk": getattr(m, "neg_risk", False),
                "yes_token_id": m.tokens[0].token_id if len(m.tokens) >= 1 else None,
                "no_token_id": m.tokens[1].token_id if len(m.tokens) >= 2 else None,
            }
        except Exception:
            continue
    return out


def percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    s = sorted(values)
    idx = max(0, min(len(s) - 1, int(len(s) * p)))
    return s[idx]


def serialize_event(e: Any) -> dict[str, Any]:
    return {
        "timestamp": e.timestamp.isoformat(),
        "type": e.type,
        "side": e.side,
        "size": str(e.size) if e.size is not None else None,
        "price": str(e.price) if e.price is not None else None,
        "asset_id": e.asset_id,
        "market_id": e.market_id,
        "tx_hash": e.tx_hash,
    }


async def main() -> None:
    OUT_DIR.mkdir(exist_ok=True)
    print(f"=== FORENSIC ANALYSIS — {WALLET} ===")
    print(f"Ventana: ultimas {HOURS_LOOKBACK}h")
    print()

    async with DataApiClient() as api, GammaClient() as gamma:
        events = await fetch_all_activity(api, WALLET, HOURS_LOOKBACK)
        print(f"Eventos descargados: {len(events)}")
        if not events:
            print("Sin actividad. ABORT.")
            return

        # Persistir raw events
        events_path = OUT_DIR / "wallet_events.jsonl"
        with events_path.open("w") as f:
            for e in events:
                f.write(json.dumps(serialize_event(e)) + "\n")
        print(f"Escrito: {events_path}")

        # Metadata de markets (limit 200 para no abusar Gamma)
        market_ids = sorted({e.market_id for e in events if e.market_id})
        print(f"Markets unicos: {len(market_ids)} (resolviendo metadata para top 200)")
        meta = await fetch_market_metadata(gamma, market_ids[:200])

        meta_path = OUT_DIR / "wallet_markets.json"
        with meta_path.open("w") as f:
            json.dump(meta, f, indent=2, default=str)

        # =====================================================
        # ANALISIS
        # =====================================================
        trades = [e for e in events if e.type == "TRADE"]
        non_trades = [e for e in events if e.type != "TRADE"]
        n_trades = len(trades)

        print()
        print("─" * 70)
        print("1) VOLUMEN Y FRECUENCIA")
        print("─" * 70)
        if not trades:
            print("Sin trades. ABORT.")
            return
        span_s = (trades[0].timestamp - trades[-1].timestamp).total_seconds()
        span_h = span_s / 3600.0
        buys = [t for t in trades if t.side == "BUY"]
        sells = [t for t in trades if t.side == "SELL"]
        print(f"  Trades TOTAL:       {n_trades}")
        print(f"  BUY:                {len(buys)} ({len(buys) / n_trades:.1%})")
        print(f"  SELL:               {len(sells)} ({len(sells) / n_trades:.1%})")
        print(f"  Ventana real:       {span_h:.2f}h ({span_s:.0f}s)")
        print(f"  Throughput:         {n_trades / max(span_h, 0.01):.1f} trades/h")
        print(f"  Throughput:         {n_trades / max(span_s, 1):.2f} trades/s")
        print(f"  Eventos no-trade:   {len(non_trades)} (splits/merges/redeems)")
        non_trade_types = Counter(e.type for e in non_trades)
        for t, n in non_trade_types.most_common():
            print(f"    {t}: {n}")

        # =====================================================
        print()
        print("─" * 70)
        print("2) INTER-ARRIVAL TIMES (gaps entre trades consecutivos)")
        print("─" * 70)
        # Order por timestamp ascendente
        trades_asc = sorted(trades, key=lambda t: t.timestamp)
        deltas_s = [
            (trades_asc[i].timestamp - trades_asc[i - 1].timestamp).total_seconds()
            for i in range(1, len(trades_asc))
        ]
        if deltas_s:
            print(f"  N gaps:             {len(deltas_s)}")
            print(f"  min:                {min(deltas_s):.3f}s")
            print(f"  p10:                {percentile(deltas_s, 0.10):.3f}s")
            print(f"  median:             {percentile(deltas_s, 0.50):.3f}s")
            print(f"  p90:                {percentile(deltas_s, 0.90):.3f}s")
            print(f"  p99:                {percentile(deltas_s, 0.99):.3f}s")
            print(f"  max:                {max(deltas_s):.3f}s")
            print(f"  mean:               {statistics.mean(deltas_s):.3f}s")
            # Burst detection: gaps < 100ms = ráfaga
            burst_count = sum(1 for d in deltas_s if d < 0.1)
            print(
                f"  Gaps <100ms (rafaga simultanea): "
                f"{burst_count} ({burst_count / len(deltas_s):.1%})"
            )

        # =====================================================
        print()
        print("─" * 70)
        print("3) BILATERAL PAIRING")
        print("─" * 70)
        # Para cada trade BUY, buscar otro BUY del MISMO market en distinto asset
        # dentro de N segundos.
        by_market: dict[str, list[Any]] = defaultdict(list)
        for t in trades_asc:
            if t.market_id and t.asset_id and t.side == "BUY":
                by_market[t.market_id].append(t)

        bilateral_pairs: list[tuple[Any, Any, float]] = []
        for _mid, ts in by_market.items():
            ts_sorted = sorted(ts, key=lambda x: x.timestamp)
            for i, t1 in enumerate(ts_sorted):
                for t2 in ts_sorted[i + 1 :]:
                    dt = (t2.timestamp - t1.timestamp).total_seconds()
                    if dt > 60:
                        break
                    if t1.asset_id != t2.asset_id:
                        bilateral_pairs.append((t1, t2, dt))
                        break  # Solo el primer match por t1 (closest)

        n_pairs = len(bilateral_pairs)
        print(f"  Pares bilaterales (BUY-A + BUY-B mismo market):  {n_pairs}")
        if n_pairs:
            deltas = [p[2] for p in bilateral_pairs]
            print(f"  Delta entre legs:")
            print(f"    min:    {min(deltas) * 1000:.1f}ms")
            print(f"    p10:    {percentile(deltas, 0.10) * 1000:.1f}ms")
            print(f"    median: {percentile(deltas, 0.50) * 1000:.1f}ms")
            print(f"    p90:    {percentile(deltas, 0.90) * 1000:.1f}ms")
            print(f"    p99:    {percentile(deltas, 0.99) * 1000:.1f}ms")
            print(f"    max:    {max(deltas):.2f}s")

            # Ratio shares y suma de precios
            sums_price: list[float] = []
            ratios_size: list[float] = []
            for t1, t2, _ in bilateral_pairs:
                p1 = float(t1.price or 0)
                p2 = float(t2.price or 0)
                s1 = float(t1.size)
                s2 = float(t2.size)
                if p1 > 0 and p2 > 0:
                    sums_price.append(p1 + p2)
                if s1 > 0 and s2 > 0:
                    ratios_size.append(min(s1, s2) / max(s1, s2))

            if sums_price:
                print()
                print(f"  SUMA (price_A + price_B) en pares bilaterales:")
                print(f"    min:    {min(sums_price):.4f}")
                print(f"    p10:    {percentile(sums_price, 0.10):.4f}")
                print(f"    median: {percentile(sums_price, 0.50):.4f}")
                print(f"    p90:    {percentile(sums_price, 0.90):.4f}")
                print(f"    max:    {max(sums_price):.4f}")
                # Edge bruto (1 - sum)
                edges = [1.0 - s for s in sums_price]
                # Solo edges positivos (arb real)
                pos_edges = [e for e in edges if e > 0]
                if pos_edges:
                    print(f"  EDGE BRUTO (1 - sum) en pares con arb (>0):")
                    print(
                        f"    N pares con edge>0: {len(pos_edges)} / "
                        f"{len(sums_price)} ({len(pos_edges) / len(sums_price):.1%})"
                    )
                    print(f"    median: {percentile(pos_edges, 0.50):.4f} (={percentile(pos_edges, 0.50) * 100:.2f}%)")
                    print(f"    p90:    {percentile(pos_edges, 0.90):.4f}")
                    print(f"    max:    {max(pos_edges):.4f}")

            if ratios_size:
                print()
                print(f"  Ratio sizes (min/max de las 2 piernas):")
                print(f"    median: {percentile(ratios_size, 0.50):.3f}")
                print(f"    p10:    {percentile(ratios_size, 0.10):.3f}")
                ratio_100pct = sum(1 for r in ratios_size if r >= 0.99)
                print(
                    f"    N pares con ratio >=99% (sizes balanced): "
                    f"{ratio_100pct} ({ratio_100pct / len(ratios_size):.1%})"
                )

        # =====================================================
        print()
        print("─" * 70)
        print("4) MARKET TIMING — time-to-resolution al entrar")
        print("─" * 70)
        ttr_data: list[float] = []
        for t in trades_asc:
            m = meta.get(t.market_id) if t.market_id else None
            if not m or not m.get("end_date"):
                continue
            try:
                end = datetime.fromisoformat(m["end_date"].replace("Z", "+00:00"))
                ttr = (end - t.timestamp).total_seconds()
                if ttr >= 0:
                    ttr_data.append(ttr)
            except Exception:
                continue
        if ttr_data:
            print(f"  N trades con metadata: {len(ttr_data)}")
            print(f"  Tiempo al cierre (s):")
            print(f"    min:    {min(ttr_data):.0f}s")
            print(f"    p10:    {percentile(ttr_data, 0.10):.0f}s")
            print(f"    median: {percentile(ttr_data, 0.50):.0f}s")
            print(f"    p90:    {percentile(ttr_data, 0.90):.0f}s")
            print(f"    max:    {max(ttr_data):.0f}s")
            # Cuantos entran con <60s al cierre (peligroso)
            very_close = sum(1 for x in ttr_data if x < 60)
            close = sum(1 for x in ttr_data if x < 120)
            print(
                f"    Trades a <60s del cierre:  "
                f"{very_close} ({very_close / len(ttr_data):.1%})"
            )
            print(
                f"    Trades a <120s del cierre: "
                f"{close} ({close / len(ttr_data):.1%})"
            )

        # =====================================================
        print()
        print("─" * 70)
        print("5) POSITION SIZING — notional y shares por trade")
        print("─" * 70)
        notionals = [
            float((t.price or Decimal(0)) * t.size) for t in trades_asc if t.size and t.price
        ]
        shares = [float(t.size) for t in trades_asc if t.size]
        if notionals:
            print(f"  Notional (USDC) por trade:")
            print(f"    min:    ${min(notionals):.2f}")
            print(f"    p10:    ${percentile(notionals, 0.10):.2f}")
            print(f"    median: ${percentile(notionals, 0.50):.2f}")
            print(f"    p90:    ${percentile(notionals, 0.90):.2f}")
            print(f"    p99:    ${percentile(notionals, 0.99):.2f}")
            print(f"    max:    ${max(notionals):.2f}")
            print(f"    sum total: ${sum(notionals):.2f}")
        if shares:
            print(f"  Shares por trade:")
            print(f"    median: {percentile(shares, 0.50):.1f}")
            print(f"    p90:    {percentile(shares, 0.90):.1f}")
            print(f"    max:    {max(shares):.0f}")

        # =====================================================
        print()
        print("─" * 70)
        print("6) ASSET / DURATION SELECTION")
        print("─" * 70)
        slug_counter: Counter[str] = Counter()
        crypto_buckets: Counter[str] = Counter()
        duration_buckets: Counter[str] = Counter()
        for t in trades_asc:
            m = meta.get(t.market_id) if t.market_id else None
            if not m:
                continue
            slug = (m.get("slug") or "").lower()
            slug_counter[slug[:50]] += 1
            # Detectar cripto
            for c in ("btc", "eth", "sol", "xrp", "doge", "bnb", "hype"):
                if c in slug:
                    crypto_buckets[c] += 1
                    break
            # Detectar duración
            for d in ("updown-5m", "5m", "15m", "1h", "hourly"):
                if d in slug:
                    duration_buckets[d] += 1
                    break

        print(f"  Trades por crypto:")
        for c, n in crypto_buckets.most_common():
            print(f"    {c}: {n} ({n / sum(crypto_buckets.values()):.1%})")
        print(f"  Trades por duracion:")
        for d, n in duration_buckets.most_common():
            print(f"    {d}: {n}")

        # =====================================================
        print()
        print("─" * 70)
        print("7) CLUSTER TEMPORAL — distribución por hora del día")
        print("─" * 70)
        by_hour: Counter[int] = Counter()
        for t in trades_asc:
            by_hour[t.timestamp.hour] += 1
        # Render histogram simple
        max_count = max(by_hour.values()) if by_hour else 1
        for h in range(24):
            c = by_hour.get(h, 0)
            bar = "█" * int(40 * c / max_count) if c else ""
            print(f"  {h:02d}:00  {c:5d}  {bar}")

        # =====================================================
        print()
        print("─" * 70)
        print("8) HOLD vs SELL — exit pattern")
        print("─" * 70)
        # Para cada SELL, buscar el BUY previo del mismo asset
        opens: dict[str, list[Any]] = defaultdict(list)
        holds: list[float] = []
        for t in sorted(trades, key=lambda x: x.timestamp):
            if t.side == "BUY" and t.asset_id:
                opens[t.asset_id].append(t)
            elif t.side == "SELL" and t.asset_id and opens.get(t.asset_id):
                buy = opens[t.asset_id].pop(0)
                holds.append((t.timestamp - buy.timestamp).total_seconds())
        if holds:
            print(f"  N pares BUY→SELL del mismo asset: {len(holds)}")
            print(f"  Hold times (s):")
            print(f"    median: {percentile(holds, 0.50):.0f}s")
            print(f"    p90:    {percentile(holds, 0.90):.0f}s")
            print(f"    max:    {max(holds):.0f}s")
        else:
            print("  CERO pares BUY→SELL. Estrategia: HOLD TO RESOLVE.")
            print("  Implicacion: posiciones se cierran via REDEEM del CTF al resolver.")
            # Eventos REDEEM
            redeems = [e for e in events if "REDEEM" in (e.type or "").upper()]
            print(f"  Eventos REDEEM en ventana: {len(redeems)}")

        # =====================================================
        print()
        print("─" * 70)
        print("9) NEG_RISK EXPOSURE")
        print("─" * 70)
        neg_risk_trades = 0
        for t in trades_asc:
            m = meta.get(t.market_id) if t.market_id else None
            if m and m.get("neg_risk"):
                neg_risk_trades += 1
        n_with_meta = sum(1 for t in trades_asc if meta.get(t.market_id or ""))
        if n_with_meta:
            print(
                f"  Trades en markets neg_risk: "
                f"{neg_risk_trades} / {n_with_meta} ({neg_risk_trades / n_with_meta:.1%})"
            )

        # =====================================================
        print()
        print("─" * 70)
        print("10) PROYECCION PNL — bilateral edge captured")
        print("─" * 70)
        if bilateral_pairs:
            total_edge_usd = 0.0
            paired_notional = 0.0
            for t1, t2, _ in bilateral_pairs:
                p1 = float(t1.price or 0)
                p2 = float(t2.price or 0)
                s1 = float(t1.size)
                s2 = float(t2.size)
                if p1 <= 0 or p2 <= 0:
                    continue
                shares = min(s1, s2)
                edge = 1.0 - (p1 + p2)
                # Fee: 1.8% * p * (1-p) cada pierna, taker
                fee_a = 0.018 * p1 * (1 - p1) * p1
                fee_b = 0.018 * p2 * (1 - p2) * p2
                edge_net = edge - (fee_a + fee_b) / max(shares, 1)
                if edge_net > 0:
                    total_edge_usd += edge_net * shares
                paired_notional += (p1 + p2) * shares
            print(f"  Pares con edge>0:        {sum(1 for p in bilateral_pairs if 1 - (float(p[0].price or 0) + float(p[1].price or 0)) > 0)}")
            print(f"  PnL teorico (edge_net):  ${total_edge_usd:.2f}")
            print(f"  Notional rotado:         ${paired_notional:.2f}")
            if span_h:
                print(f"  Proyeccion PnL/h:        ${total_edge_usd / span_h:.2f}")
                print(f"  Proyeccion PnL/dia:      ${total_edge_usd / span_h * 24:.2f}")
                print(f"  Notional/h:              ${paired_notional / span_h:.2f}")

        print()
        print("=" * 70)
        print(f"Output: {OUT_DIR}/")
        print("=" * 70)


if __name__ == "__main__":
    asyncio.run(main())
