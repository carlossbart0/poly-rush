# TRINITY HARNESS — plan de implementación

## Objetivo

Correr 3 estrategias en paralelo sobre el MISMO feed Polymarket WS (y mismo CEX feed) durante 10 minutos en DRY-RUN puro. Cada estrategia persiste sus "would-have-entered" decisions a JSONL separado. Después: análisis comparativo para decidir cuál replicar con capital real.

## Las 3 estrategias

### A) LAG-ARB DIRECCIONAL (CEX-driven)

Réplica de lo que hace `0xeebde7a0`. Compra direccional el lado con edge según modelo CEX.

```
INPUTS:
  - precio mid YES en Polymarket (book WS)
  - precio mid CEX (Coinbase WS)
  - time-to-resolution del market
  - σ realized rolling 60 ticks

MODEL:
  K = strike (precio open del bucket 5m, derivado del slug timestamp)
  P_model(YES wins) = Φ((log(BTC_now/K) - 0.5σ²dt) / (σ√dt))
  P_market(YES)    = mid_yes
  edge = P_model - P_market

DECISION:
  if |edge| > 0.10 (10%):       # threshold conservador
    direction = sign(edge)       # YES si edge>0, NO si edge<0
    size_usdc = $20 × |edge|     # Kelly-light: más confianza = más size
    LOG_ENTRY(direction, size, P_model, P_market, edge, ttr)
  
  if ttr < 30:                   # NO entrar muy cerca del cierre
    SKIP
  
  if neg_risk:
    SKIP
```

**Riesgos conocidos**:
- σ realized puede ser muy distinta de implied → modelo descalibrado
- Latencia CEX → Polymarket → en live perderías ms críticos
- Si mi K (strike inferido) está mal, todo el modelo está mal

### B) BILATERAL PURO MEJORADO (3 fixes aplicados)

Mi bot Rust actual + las 3 mejoras identificadas:

```
INPUTS:
  - best_ask YES, best_ask NO (book WS)
  - market metadata (neg_risk, end_date)

DECISION:
  if neg_risk:                                    # FIX 1
    SKIP
  if (end_date - now) < 120s:                     # FIX 2 (anti-phantom)
    SKIP
  if min_liquidity(yes,no) × min_price < $50:     # FIX 3
    SKIP
  
  cost = price_yes + price_no + fees + slippage_buf
  edge = 1 - cost
  
  if edge >= 2% AND edge <= 8%:                   # cap upper (phantom protection)
    LOG_ENTRY(YES @ p_yes, NO @ p_no, shares=min(size_y, size_n, max_cap))
```

**Riesgos conocidos**:
- Throughput bajo (arbs reales son escasos)
- Pierde la cola alta de edge real (>8% suele ser phantom pero a veces es real)

### C) HÍBRIDO (bilateral con CEX como veto)

Igual que B, pero usa CEX como **filtro de sanidad**:

```
DECISION:
  Pasa todos los filtros de B PRIMERO.
  Si B aprobaría la entry:
    P_model = (mismo Black-Scholes de A)
    P_market = (price_yes + (1-price_no))/2  # mid implicito
    
    if |P_model - P_market| < 5%:              # CEX confirma
      LOG_ENTRY (igual que B)
    else:
      SKIP_WITH_REASON("cex_disagrees")
```

C es estrictamente menos opps que B. La hipótesis: las opps que B detecta pero CEX no confirma son phantoms o mispricings que se van a corregir antes del fill.

## Arquitectura

```
src/
├── lib.rs                    (mod declarations)
├── main.rs                   (CLI: trinity subcommand)
├── types.rs                  (existente)
├── fees.rs                   (existente)
├── book_state.rs             (existente)
├── poly_ws.rs                (existente)
├── gamma.rs                  (EXTENDIDO: end_date, neg_risk filter)
├── recorder.rs               (existente)
├── config.rs                 (EXTENDIDO: trinity params)
├── runner.rs                 (existente — strategy B sola)
│
├── cex_feed.rs               (NUEVO)
├── pricing_model.rs          (NUEVO)
├── strategies/
│   ├── mod.rs                (NUEVO)
│   ├── lag_arb.rs            (NUEVO — strategy A)
│   ├── bilateral_pure.rs     (NUEVO — strategy B refactored)
│   └── hybrid.rs             (NUEVO — strategy C)
└── trinity_runner.rs         (NUEVO)
```

## Filtros adicionales aplicados (robustez)

Para TODAS las estrategias:
- **neg_risk filter**: ya no entra a multi-outcome (donde puede resolver a "neither" = pérdida 100%)
- **time-to-resolution >= 30s**: evita phantom edges de markets resolviendo
- **min liquidity**: descarta books con <$50 total notional disponible
- **price sanity**: 0.01 < price < 0.99 (descarta lotteries y resolved)
- **edge sanity**: descarta edges >50% (casi seguro phantom o bug)

## Recorders separados

Cada estrategia escribe a su propio JSONL:
- `analysis_output/trinity_A.jsonl`
- `analysis_output/trinity_B.jsonl`
- `analysis_output/trinity_C.jsonl`

Cada line es una "decision" con:
```json
{
  "strategy": "A" | "B" | "C",
  "timestamp": "...",
  "market_id": "...",
  "market_slug": "btc-updown-5m-T",
  "decision": "ENTER" | "SKIP",
  "skip_reason": "neg_risk|ttr|liquidity|cex_disagrees|null",
  "edge_per_unit": "0.034",
  "size_usdc": "20.00",
  "direction": "YES|NO|BOTH",
  "legs": [...],
  "price_market_implied": "0.45",
  "price_model": "0.52",
  "cex_btc_spot": "63420.5",
  "ttr_seconds": 380,
  "expected_pnl_usdc": "0.68"
}
```

También un **events JSONL** común con métricas crudas (latencia, # books, # markets activos por minuto) para post-mortem.

## Análisis post-run

Script `scripts/trinity_analysis.py` que para cada estrategia computa:

1. **# decisions ENTER vs SKIP**
2. **PnL teórico total** (suma de expected_pnl)
3. **Hit rate hipotético** — para los markets que ENTERON y ya resolvieron en ventana, ¿cuántos hubieran ganado? (compara dirección con outcome resuelto via Polymarket data)
4. **Edge distribution** (median, p90, max)
5. **Time-to-resolution distribution** al entrar
6. **Crossover analysis**: opps donde A y C coinciden vs solo A vs solo C
7. **Latencia**: para A, mide delay CEX_tick → decision

## Parámetros default del experimento

```
ARB_FEE_RATE_BPS=180
ARB_SLIPPAGE_BUFFER_BPS=20
ARB_MIN_EDGE_PER_UNIT=0.02      (B: 2%)
ARB_MAX_EDGE_PER_UNIT=0.08      (B: cap anti-phantom)
ARB_MAX_SIZE_USDC=20
ARB_MIN_TTR_SECONDS=30
ARB_MIN_LIQUIDITY_USDC=50

LAGARB_EDGE_THRESHOLD=0.10      (A: 10% modelo vs market)
LAGARB_SIZE_USDC=20

HYBRID_CEX_TOLERANCE=0.05       (C: 5% max disagreement)

CEX_VOL_LOOKBACK_TICKS=60
CEX_VOL_FALLBACK_ANNUAL=0.60    (60% vol crypto típica si pocos ticks)
```

## Investigado lo crítico

| Pregunta | Respuesta usada | Confianza |
|---|---|---|
| Coinbase WS endpoint | `wss://ws-feed.exchange.coinbase.com` (ticker channel) | alta |
| Schema Coinbase ticker | `{type:"ticker", product_id, price, time, ...}` | alta |
| Polymarket fee taker crypto | 1.80% (180 bps) | alta (mi bot Python lo usa) |
| Slug de markets crypto 5m | `{crypto}-updown-5m-{unix_close}` | alta (visto en data) |
| Strike de updown-5m | K = precio CEX a T_open = T_close - 300s | media (asumido, no verificado oficialmente) |
| σ realista BTC 5m | 60% annualized fallback, rolling 60 ticks como base | media |
| Latencia mínima viable VPS Madrid → CLOB | ~46ms TTFB | alta (medido) |

## Output esperado tras 10 min

Reporte markdown con:
- Total opps por strategy
- PnL teórico ranking
- Hit rate estimado (cuál hubiera acertado dirección)
- Recomendación de réplica con capital basada en data, no opinión

## Plazos

| Etapa | Min |
|---|---|
| Plan (este doc) | 5 |
| Implementación cex_feed + pricing | 25 |
| Implementación strategies A, B', C | 25 |
| Trinity runner + recorders | 15 |
| Compilación + tests | 15 |
| **Dry-run 10 min** | **10** |
| Análisis + reporte | 15 |
| **TOTAL** | **~110 min** |
