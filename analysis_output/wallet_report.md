# Forensic Analysis — 0xeebde7a0e019a63e6b476eb425505b7b3e6eba30

**Ventana**: ultimas 24h (cap API: 2900 events)
**Período activo**: 21:48–23:07 UTC (1h 19min de los últimos 24h)
**Trades analizados**: 2850 (100% BUY, 0% SELL)
**Markets únicos**: 55

---

## Conclusión ejecutiva

**NO es arbitraje bilateral puro** como inicialmente se clasificó. Es **lag arb direccional con hedge oportunista**, alimentado por feed CEX (Coinbase/Binance).

Evidencia clave:
- `pair_cost (sum_avg_yes + sum_avg_no) median = 1.0021` — si fuera arb garantizado tendría que ser <0.98
- **53% de los markets** terminan con `pair_cost > 1.0` (pérdida si hold to resolve sin signal)
- **Quantities asimétricas**: median ratio min/max = 0.52 (no balanced)
- Delta median entre legs = **14s** (no FOK simultáneo)
- Bot solo activo **21–23 UTC** (3h del día), no 24/7

El edge real proviene de **información asimétrica vía CEX feed**, no de matemática bilateral. Replica requiere monitorear Coinbase/Binance spot + modelo predictivo P(BTC_T > K).

---

## 1. Volumen y frecuencia

| Métrica | Valor |
|---|---|
| Trades totales (24h cap) | 2850 |
| Trades/h (período activo) | **1864** |
| Trades/s (período activo) | 0.52 |
| BUY:SELL ratio | 100% : 0% |
| Notional total | **$63,867** |
| Notional/h | $41,749 |
| PnL teórico/h (edge bilateral) | **$441** |
| PnL teórico/día (extrapolado) | $10,597 |

**Interpretación HFT**: throughput alto pero NO sostenido. Concentración temporal extrema sugiere un trigger basado en eventos macro (probable: ventana de horario operativo del operator) o disponibilidad de signal CEX.

---

## 2. Inter-arrival times (gaps entre trades consecutivos)

| Percentil | Gap |
|---|---|
| min | 0ms |
| p10 | 0ms |
| **median** | **0ms** |
| p90 | 5s |
| p99 | 21s |
| max | 47s |

**56.9% de los gaps son <100ms**. Esto significa **ráfagas simultáneas** — el bot enviá 5-10 órdenes en un mismo tick cuando un signal dispara. Después espera segundos hasta el siguiente trigger.

---

## 3. Bilateral pairing (BUY-A + BUY-B mismo market dentro de 60s)

| Métrica | Valor |
|---|---|
| Pares detectados | 1613 |
| Delta median entre legs | **14s** |
| Delta p90 | 46s |
| Delta p99 | 56s |
| Delta máx | 60s |
| **Sum (price_A + price_B) median** | **0.99** |
| Sum p10 | 0.89 |
| Sum p90 | 1.07 |
| Sum máx | 1.93 |
| Pares con `sum < 1` (arb real) | 52% |
| Edge bruto median en arbs | 4.0% |
| Edge bruto p90 | 19% |
| Sizes balanced (ratio ≥99%) | **13.6%** |

**Interpretación**: Si fuera arb FOK simultáneo, delta sería ms, sum sería <1 en 100% y sizes balanced 100%. **Ninguna se cumple**. Las "pares" son artefacto de proximidad temporal, no de estrategia bilateral coordinada.

---

## 4. Market timing — time-to-resolution al entrar

| Percentil | Segundos al cierre |
|---|---|
| min | 0s |
| p10 | 98s |
| **median** | **680s (11 min)** |
| p90 | 3010s (50 min) |
| max | 7128s (2h) |

| Trades a <60s del cierre | **4.7%** |
| Trades a <120s del cierre | **12.1%** |

**12% entra muy cerca del cierre** — agresivo. Estos son los trades de "snipe": precio Polymarket aún no actualizó, pero el bot ya sabe vía CEX que va a resolver de un lado.

---

## 5. Position sizing — power-law distribution

| Percentil | Notional USDC |
|---|---|
| p10 | $0.30 |
| median | $3.19 |
| p90 | $22.91 |
| p99 | $334.06 |
| **max** | **$5,000.49** |

Distribución power-law: muchos chicos ($1-5), pocos gigantes ($1k+). **No es sizing uniforme**. Hipótesis: scaling por conviction o por arrival rate de signal.

### Shares vs precio (scaling pattern)

| Bucket precio | N trades | median size | max size |
|---|---|---|---|
| 0.00–0.10 | 442 | 10 | **3154** |
| 0.10–0.20 | 333 | 8 | 111 |
| 0.20–0.30 | 283 | 7 | 62 |
| ... | ... | ... | ... |
| 0.90–1.00 | 293 | 23 | **5051** |

**Observación crítica**: los sizes máximos aparecen en los **extremos** (precios <0.10 o >0.90). Esto es consistente con direccional alto-conviction: cuando el bot está 90%+ confiado de la dirección, compra heavy (cantidades 100×-500× la mediana).

---

## 6. Asset / duration selection

| Crypto | % trades |
|---|---|
| BTC | **88.2%** |
| ETH | 11.8% |
| SOL/DOGE/otros | 0% |

| Duración | N trades |
|---|---|
| `updown-5m` | 236 |
| `5m` (otros) | 236 |
| **15m** | (presente en top market: btc-updown-15m-1778626800) |

**88% BTC** confirma: el bot tiene signal especializado para BTC. ETH es secundario. Otros cryptos no entran porque la accuracy del signal es menor (o el operator no implementó modelos para esos).

---

## 7. Drill-down: top markets por # trades

| Trades | Notional | Slug | Asset A qty/avg | Asset B qty/avg | **pair_cost** | PnL si hold |
|---|---|---|---|---|---|---|
| 173 | $5,487 | btc-updown-15m-1778626800 | 550 @ $0.56 | **5653 @ $0.92** | **1.48** | -$4,937 (si resuelve A) o +$166 (si B) |
| 157 | $2,477 | 0x8d0de1695530aa | 2053 @ $0.34 | 2162 @ $0.82 | 1.16 | -$424 / +$382 |
| 146 | $1,572 | 0xed683be234e4bd | 1272 @ $0.88 | 3792 @ $0.12 | 0.998 | -$300 / +$2,220 |
| 131 | $2,505 | 0x9f21a586ad6f3b | **2583 @ $0.93** | 1093 @ $0.085 | 1.02 | -$1,412 / +$170 |
| 108 | $619 | 0x1a3a7851307f57 | **1679 @ $0.13** | 703 @ $0.57 | 0.70 | +$84 (guaranteed via bilateral) |
| 104 | $128 | 0xaced33261ff1a4 | 456 @ $0.19 | 141 @ $0.29 | 0.48 | +$13 (guaranteed) |

### Patrón emergente

**Markets con apuesta direccional clara**:
- btc-updown-15m: compró **5653 NO** @ avg $0.92 (apuesta heavy de que BTC NO subirá), con hedge de 550 YES. Si gana NO → ~$166 profit; si pierde → -$4937.
- 0x9f21a: compró **2583 YES** @ avg $0.93 (high conviction). Hedge mínimo 1093 NO @ $0.085. Si gana YES → small profit; si pierde → -$1412.

**Markets con bilateral genuino**:
- 0x1a3a78 y 0xaced33: pair_cost <0.71. Ambos lados tienen guaranteed profit. Estos son **mientras esperaba el signal direccional**, captura arbs colaterales.

---

## 8. Cluster temporal — actividad por minuto

Período activo: **21:48–23:07 UTC** (1h 19min).

```
21:48 ████████████████ (27)
21:49 ████████████████████ (31)
...
22:08 ███████████████████████████████████ (54)
...
22:19 ██████████████████████████████████████ (60) ← pico inicial
...
22:34 ██████████████████████████████████████████████████ (77) ← pico
...
23:02 ██████████████████████████████████████████████████ (77) ← pico final
23:07 █████████████████████████████ (46)
```

**Patrón**: actividad sostenida con 3 picos a las 22:19, 22:34 y 23:02. Los picos sugieren **disparadores macro** (probable: moves de BTC en CEX que activan señales en cascada en múltiples markets simultáneos).

---

## 9. Exit pattern

- **0 BUY→SELL pairs**. Bot NUNCA vende.
- **50 eventos REDEEM** en ventana → CTF redeem automático al resolver markets.
- **Implicación**: estrategia es 100% **HOLD TO RESOLVE**. Sin stop-loss, sin take-profit intra-trade.

---

## 10. Neg_risk exposure

**0% de trades en markets neg_risk** (sobre 432 trades con metadata). El bot **filtra estrictamente** markets multi-outcome. Consistente con un operator que entiende los riesgos (a diferencia de mi Rust bot que no filtra esto).

---

## Blueprint para réplica

Estructura mínima para replicar (no es trivial — más complejo que arb bilateral):

```
┌─────────────────────────────────────────────────────────┐
│ Layer 1: CEX FEED                                        │
│  - Coinbase WS ticker BTC-USD, ETH-USD (1-tick latency) │
│  - Binance WS ticker (backup + diff feeds)              │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│ Layer 2: PROBABILITY MODEL                               │
│  Para market btc-updown-T (resuelve UP si BTC_T > K):    │
│  - σ_implied desde options o historical realized vol     │
│  - P(BTC_T > K) = N(d2) en log-normal Black-Scholes      │
│  - Tomar mid-spread de Polymarket como P_market          │
│  - edge = |P_model - P_market|                           │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│ Layer 3: ENTRY DECISION                                  │
│  if edge > threshold_entry (~5-10%):                     │
│    direction = sign(P_model - P_market)                  │
│    size = Kelly_fraction(edge, variance) × max_capital   │
│    if T-now < 120s: increase aggressiveness             │
│    if neg_risk: ABORT                                    │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│ Layer 4: EXECUTION                                       │
│  - Sub-orders chicas ($3-20) en lugar de 1 grande       │
│  - Si best_ask del otro lado baja N bps → hedge chico    │
│  - NO FOK (no necesita simultaneidad), GTC con price=ask │
└─────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│ Layer 5: POSITION MANAGEMENT                             │
│  Track per-market: {qty_yes, cost_yes, qty_no, cost_no} │
│  Nada de SELL — HOLD a resolution                        │
│  Watch redeemable flag → auto REDEEM CTF                 │
└─────────────────────────────────────────────────────────┘
```

### Parámetros derivados del análisis

| Parámetro | Valor sugerido | Justificación |
|---|---|---|
| `min_edge_entry` | 5% (P_model - P_market) | Por debajo: signal noise > edge |
| `kelly_fraction` | 0.25 (1/4 Kelly) | Conservador, como gabagool |
| `time_to_resolution_min` | 0s (entra hasta el cierre) | p10 = 98s, 12% trades a <2 min |
| `max_notional_per_trade` | $5,000 | Visto en el data (max single trade) |
| `median_notional_per_trade` | $3 | Mediana del bot |
| `targets` | BTC 5m, BTC 15m, ETH 5m | 100% del data |
| `operating_hours` | 21:00-23:00 UTC | (probable correlación con operator timezone) |

---

## Replicabilidad: viable pero NO trivial

| Capa | Estado mi bot Rust actual | Esfuerzo para replicar |
|---|---|---|
| Layer 1 (CEX feed) | ❌ ausente | ~150 líneas (Coinbase WS) |
| Layer 2 (model) | ❌ ausente | ~200 líneas (Black-Scholes) |
| Layer 3 (entry) | parcial (bilateral threshold) | ~100 líneas (Kelly + dir) |
| Layer 4 (execution) | ❌ no live | ~400 líneas (EIP-712 + GTC) |
| Layer 5 (position) | ❌ ausente | ~150 líneas (state + redeem) |
| **Total** | | **~1000 líneas Rust** |

**Costos adicionales**:
- Datos de IV/realized vol para σ (~$50/mes APIs)
- Capital de operación $500-5000
- Riesgo: si signal accuracy <55% → pérdida neta

---

## Recomendaciones priorizadas

1. **DESCARTAR la clasificación "bilateral arb bot"**. Esta wallet NO es replicable solo con `strategy/arbitrage/` bilateral. Requiere `lag_arb` (que el advisor previo había rechazado como "predictivo, no arb real" — **el advisor tenía razón pero el bot real efectivamente usa eso**).

2. **Si querés replicarlo**: agregar Layer 1+2 a mi Rust bot. Mi bot Python ya tiene un módulo `lag_arb.py` que abandoné por consejo del advisor; puede servir como referencia.

3. **Si querés mantenerte en arb puro** (mi recomendación): mejorar el Rust bot con los 3 fixes ya identificados (neg_risk filter, time-to-resolution, edge >5% descartar phantom). PnL teórico realista: $20-100/h dependiendo de condiciones.

4. **NO copy-trade esta wallet**: delta 14s entre legs + size asimétrico → cualquier copy llegaria tarde al best ask.
