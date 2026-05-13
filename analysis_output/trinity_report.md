# Trinity Harness — Reporte Comparativo

Run de 10 minutos en DRY-RUN puro. 3 strategies sobre el mismo feed Polymarket + Coinbase.

## Resumen ejecutivo

| Strategy | ENTERS | PnL teórico | Edge median | Size median |
|---|---|---|---|---|
| A — Lag Arb Direccional (CEX) | 27198 | $148472.8110 | 15.56% | $20.00 |
| B — Bilateral Puro Mejorado | 0 | $0.0000 | 0.00% | $0.00 |
| C — Híbrido (Bilateral + CEX veto) | 0 | $0.0000 | 0.00% | $0.00 |


## STRATEGY A — Lag Arb Direccional (CEX)

| Métrica | Valor |
|---|---|
| Decisions logged | 27198 |
| ENTERS | **27198** |
| SKIPS logged | 0 |
| PnL teórico total | **$148472.8110** |
| PnL median/entry | $3.8784 |
| Notional rotado | $482968.42 |
| Edge median | 15.56% |
| Edge max | 40.70% |
| Size median | $20.00 |
| Size max | $55.61 |
| TTR median | 208s |
| Directions | {'NO': 24075, 'YES': 3123} |
| Crypto split | {'btc': 21862, 'eth': 5336} |
| CEX edge median | -0.1456 |
| P_model median | 0.2383 |
| σ (annual) median | 0.1209 |

Top skip reasons:

## STRATEGY B — Bilateral Puro Mejorado

| Métrica | Valor |
|---|---|
| Decisions logged | 0 |
| ENTERS | **0** |
| SKIPS logged | 0 |
| PnL teórico total | **$0.0000** |
| PnL median/entry | $0.0000 |
| Notional rotado | $0.00 |
| Edge median | 0.00% |
| Edge max | 0.00% |
| Size median | $0.00 |
| Size max | $0.00 |
| TTR median | 0s |

Top skip reasons:

## STRATEGY C — Híbrido (Bilateral + CEX veto)

| Métrica | Valor |
|---|---|
| Decisions logged | 0 |
| ENTERS | **0** |
| SKIPS logged | 0 |
| PnL teórico total | **$0.0000** |
| PnL median/entry | $0.0000 |
| Notional rotado | $0.00 |
| Edge median | 0.00% |
| Edge max | 0.00% |
| Size median | $0.00 |
| Size max | $0.00 |
| TTR median | 0s |

Top skip reasons:

## Crossover analysis

| Métrica | Valor |
|---|---|
| Markets totales con ENTRY | 2 |
| Solo A | 2 |
| Solo B | 0 |
| Solo C | 0 |
| A ∩ B | 0 |
| A ∩ C | 0 |
| B ∩ C | 0 |
| A ∩ B ∩ C | 0 |

## Eficacia del veto CEX (Strategy C)

- Opps que C vetó por desacuerdo CEX: **0**
- Si C es subset estricto de B, B-C = 0 entradas extras de B

## Top 5 entradas por PnL teórico — Strategy B

| Mkt | Edge | Size $ | PnL teo $ |
|---|---|---|---|

## Veredicto

Ranking PnL teórico (10 min):

1. **A — Lag Arb Direccional (CEX)**: $148472.8110 (27198 entradas)
2. **B — Bilateral Puro Mejorado**: $0.0000 (0 entradas)
3. **C — Híbrido (Bilateral + CEX veto)**: $0.0000 (0 entradas)

**Ganador**: A — Lag Arb Direccional (CEX)

⚠️ C no entró ninguna vez. Si B sí entró, el CEX veto descartó todas las opps de B.
