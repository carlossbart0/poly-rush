# Research: threshold ideal para lag-arb en Polymarket 5-min crypto markets

Investigacion realizada en mayo 2026 sobre el valor optimo de `momentum_threshold` (basis points) para Strategy A (lag arbitrage Coinbase to Polymarket) en markets `*-updown-5m-*` y `*-updown-15m-*`.

## Resumen ejecutivo

- **3-5 bps** es el rango defendible para `momentum_threshold` en ventana de 30s
- **1 bps es ruido browniano confirmado** matematicamente
- **Polymarket cambio las fees en enero 2026** — fee dinamico de hasta 3.15% mata gran parte del edge historico
- Tu config `ARB_FEE_RATE_BPS=180` (1.80%) puede estar subestimando el costo real
- Bots ganadores actuales operan con escala ($4-5K por trade), no con threshold

## Threshold values segun la literatura

### Paper v3 mas relevante (marzo 2026)

[AI-Augmented Arbitrage in Short-Duration Prediction Markets: Live Trading Analysis of Polymarket's 5-Minute Bitcoin Binary Options](https://medium.com/@gwrx2005/ai-augmented-arbitrage-in-short-duration-prediction-markets-live-trading-analysis-of-polymarkets-8ce1b8c5f362) — analisis empirico del mismo problema que tu bot resuelve.

Thresholds usados en v3 del bot del autor:

| Signal | Threshold | Significado |
|---|---|---|
| DISLOCATION primary trigger | **0.05% (5 bps)** | BTC se movio >5 bps en window y Polymarket no ajusto |
| DIRECTIONAL signal | **0.03% (3 bps)** | con composite confidence >=0.45 + 10-min trend agree |
| Noise rejection | **<0.03% con >90s** | rechazar como ruido |

Cita exacta del paper:

> "v2 used a DISLOCATION trigger of 0.01% BTC move (identified as problematic) because it falls within the range of Brownian noise at 5-minute timescales"

**Conclusion del autor**: 1 bps es matematicamente ruido. El piso minimo no-ruido empieza en 3-5 bps.

### Otros bots open source

| Repo / autor | Threshold | Ventana | Notas |
|---|---|---|---|
| [console2002/polymarket-momentum-bot](https://deepwiki.com/console2002/polymarket-momentum-bot) | **2% en 60s** | 60s | "Hard directional moves". Tambien usa gap 3-5% spot/prediction. |
| [Polymarket BTC 5-Minute Up/Down (Archetapp)](https://gist.github.com/Archetapp/7680adabc48f812a561ca79d73cbac69) | no especifica | 5min | Bot publico tipo "set and forget" |
| [aulekator/Polymarket-BTC-15-Minute-Trading-Bot](https://github.com/aulekator/Polymarket-BTC-15-Minute-Trading-Bot) | multi-signal | 15min | 7-phase architecture, signal fusion |
| [ThinkEnigmatic/polymarket-bot-arena](https://github.com/ThinkEnigmatic/polymarket-bot-arena) | adaptativo | 5min | Adaptive bot arena testing |

### Range general en HFT crypto

Investigacion academica ([High frequency momentum trading with cryptocurrencies, ScienceDirect](https://www.sciencedirect.com/science/article/abs/pii/S0275531919308062)):

- Movimientos de **<2 bps en horizontes <1min** son indistinguibles de noise en mercados liquidos
- Strategies profitables usan thresholds de **10-50 bps** en ventanas de **5-60s**
- Best Sharpe ratios reportados con threshold 4.0% en horizonte de 10 minutos ([arxiv.org](https://arxiv.org/html/2602.18912v1))

## CAMBIO CRITICO: Fees dinamicos de Polymarket

### Enero 2026: fee dinamico en 15-min crypto markets

[Polymarket Introduces Dynamic Fees to Curb Latency Arbitrage](https://www.financemagnates.com/cryptocurrency/polymarket-introduces-dynamic-fees-to-curb-latency-arbitrage-in-short-term-crypto-markets/):

- Fee dinamico que es **mas alto cuando el precio esta cerca de 50%**
- Pico: **3.15% en un contrato de 50 centavos**
- Objetivo declarado: "neutralizar latency arbitrage strategies"
- Cita: "the new fee structure makes this strategy unprofitable at scale"
- Aplica solo a takers (los makers reciben rebate)

### Marzo 2026: expansion a casi todas las categorias

[Polymarket quietly introduces taker fees](https://www.tradingview.com/news/cointelegraph:e59c32089094b:0-polymarket-quietly-introduces-taker-fees-on-15-minute-crypto-markets/) — el 30 de marzo 2026, Polymarket extendio fees a:

- Finance, Politics, Economics, Culture
- Weather, Tech, Mentions, Other/General

Solo quedaron sin fee unos pocos mercados muy largos.

### Implicacion practica para tu bot

- Tu codigo usa `ARB_FEE_RATE_BPS=180` (1.80%) — desactualizado
- Fee real en markets 50/50 = **hasta 3.15%**
- En markets con precio asimetrico (ej. 0.30/0.70) el fee es menor pero >1.80% sigue siendo posible
- **El edge calculado por tu Strategy A subestima el costo real** — necesitas un edge mas grande que el "edge_per_unit" reportado en tus logs para ser rentable post-fees

## Resultados de bots ganadores en la prensa

[Arbitrage Bots Dominate Polymarket With Millions in Profits — Yahoo Finance](https://finance.yahoo.com/news/arbitrage-bots-dominate-polymarket-millions-100000888.html):

- Bot top: **$206,000 profit** con 85%+ win rate
- Mejor humano: $100K con misma estrategia
- $40M en arbitrage profits Apr 2024 - Apr 2025 (research IMDEA Networks)
- Bots winners: opero en BTC/ETH/SOL **15-min markets**, sizes **$4K-5K por trade**

[$313 a $414k en un mes con lag arb](https://www.financemagnates.com/cryptocurrency/polymarket-introduces-dynamic-fees-to-curb-latency-arbitrage-in-short-term-crypto-markets/) — un wallet famoso pre-fees. Post-fees Enero 2026 este nivel de retorno ya no se replica.

## Time windows en la literatura

El paper v3 usa 4 timeframes:

- **30s** — momentum primary
- **60s** — confirmation
- **120s** — trend
- **240s** — long trend
- **600s (10 min)** — trend filter agregado en v3

Tu config actual: solo **30s**. Funciona pero pierde el filtro de tendencias mas largas que rechaza falsos positivos.

## Trampas comunes citadas en la literatura

1. **Edge en paper-trading != edge en live trading**
   - LayerX research: bot mostro profits con Gamma API bid prices, fallaba con CLOB ask prices
   - Tu bot ya usa CLOB book directamente, asi que no caes en esto

2. **Latencia decay del edge**
   - "Momentum strategies face a fundamental challenge: edge decays faster than execution. By the time you detect it, it's gone." ([SwapHunt](https://medium.com/coinmonks/polymarket-just-changed-its-fees-heres-what-bot-traders-need-to-know-c11132e55d5c))
   - Polymarket CLOB lags Binance spot por **30-90 segundos** en moves grandes (no chicos)
   - Para movimientos chicos (1-5 bps), el MM ya digirio en <1s

3. **Spread come edge en markets thin**
   - Bid-ask spread en Polymarket 5-min puede ser 1-2 cents (1-2%)
   - Si entras como taker, ya pagas el spread + 1.8-3.15% fee
   - Edge necesario para break-even: 3-5% por trade

4. **Coinbase vs Binance como feed**
   - Bots top usan Binance (mayor volumen, mejor price discovery)
   - Tu bot usa Coinbase (menor volumen, posible mas lag)
   - Migrar a Binance daria mejor signal pero requiere reescribir cex_feed.rs

## Recomendaciones concretas para tu bot

### Threshold

| Valor | Cuando usar |
|---|---|
| 3 bps (actual) | Operacion frecuente, captura "DIRECTIONAL" del paper v3 |
| **5 bps** | Conservador, alineado con DISLOCATION primary trigger del paper v3 |
| 10 bps | Muy selectivo, solo eventos grandes |
| 20+ bps | Casi no opera, solo eventos extremos |

**Mi recomendacion**: probar **5 bps con `momentum_window_s: 60`** (mas filtro de tendencia) por una semana. Es el setup del paper v3 que fue publicado con resultados positivos.

### Fees

Actualizar `ARB_FEE_RATE_BPS` de 180 a **300** o calcular dinamico:

```
fee_taker_50_50 = 3.15%   (worst case en markets 50/50)
fee_taker_70_30 = ~2.10%  (markets asimetricos, estimado)
fee_taker_90_10 = ~0.50%  (markets muy asimetricos)
```

Strategy A tiende a entrar en markets cerca de 50/50 (donde el momentum decide la direccion), asi que el caso peor 3.15% es probable.

### Capital y size

Bots ganadores usan **$4K-5K por trade**. Tu cap actual de **$3 por trade** funciona para testing pero no genera profit relevante.

- Para testing/validacion: tu setup actual ($3, threshold 3-5 bps) esta OK
- Para produccion real: subir cap a $50-100 por trade y aceptar 5-15% drawdown como parte del juego
- Sin escala suficiente, los fees fijos comen toda la ganancia

### Filtros adicionales que el paper v3 agrega

1. **Composite confidence >=0.45** — el paper combina varias signals con un score; vos usas solo momentum simple
2. **10-min trend filter** — rechazar entries donde momentum 30s va contra trend 10min
3. **Skip noise <0.03% con >90s remaining** — vos rechazas con threshold, esto es similar pero condicionado al TTR

Implementar el filtro de 10-min trend agregaria ~30 lineas en `lag_arb.rs`.

## Resumen para tu pregunta original

**Threshold ideal hoy = 3 a 5 bps en ventana 30-60s** segun la unica fuente especifica al caso (Polymarket 5-min BTC binary options, paper marzo 2026).

**Lo mas critico no es el threshold sino los fees**: Polymarket cambio las reglas en enero 2026 y el lag arb que era profitable a $4-5K por trade pre-fees ya casi no lo es. Tu bot necesita o bien (a) operar markets que no tengan fee dinamico, (b) ser maker en lugar de taker, o (c) aceptar margenes muy delgados.

## Fuentes

### Especificas a Polymarket lag arb

- [AI-Augmented Arbitrage in Short-Duration Prediction Markets — Medium (Mar 2026)](https://medium.com/@gwrx2005/ai-augmented-arbitrage-in-short-duration-prediction-markets-live-trading-analysis-of-polymarkets-8ce1b8c5f362) — paper mas relevante, mismo caso
- [Beyond Simple Arbitrage: 4 Polymarket Strategies Bots Actually Profit From in 2026](https://medium.com/illumination/beyond-simple-arbitrage-4-polymarket-strategies-bots-actually-profit-from-in-2026-ddacc92c5b4f)
- [Unlocking Edges in Polymarket's 5-Minute Crypto Markets](https://medium.com/@benjamin.bigdev/unlocking-edges-in-polymarkets-5-minute-crypto-markets-last-second-dynamics-bot-strategies-and-db8efcb5c196)
- [console2002/polymarket-momentum-bot — DeepWiki](https://deepwiki.com/console2002/polymarket-momentum-bot)
- [aulekator/Polymarket-BTC-15-Minute-Trading-Bot — GitHub](https://github.com/aulekator/Polymarket-BTC-15-Minute-Trading-Bot)
- [ThinkEnigmatic/polymarket-bot-arena — GitHub](https://github.com/ThinkEnigmatic/polymarket-bot-arena)
- [ImMike/polymarket-arbitrage — GitHub](https://github.com/ImMike/polymarket-arbitrage)
- [LayerX research blog — automated trading bot](https://blog.layerx.xyz/polymarketbots)
- [Build a Polymarket Arbitrage Bot in Python (2h tutorial) — DEV.to](https://dev.to/chudi_nnorukam/how-i-built-a-polymarket-trading-bot-that-actually-makes-money-1fb9)
- [Polymarket Trading Bot Open Source — DEV.to](https://dev.to/benjamin_martin_749c1d57f/polymarket-trading-bot-real-time-arbitrage-momentum-strategies-and-production-features-open-17m1)
- [My Journey Building a Polymarket BTC Trading Engine — Medium](https://kaustubhpatange.medium.com/my-journey-building-a-polymarket-btc-trading-engine-577436189a3b)

### Cambio de fees (CRITICO)

- [Polymarket Introduces Dynamic Fees — Finance Magnates (Jan 2026)](https://www.financemagnates.com/cryptocurrency/polymarket-introduces-dynamic-fees-to-curb-latency-arbitrage-in-short-term-crypto-markets/)
- [Polymarket quietly introduces taker fees on 15-min crypto — TradingView/Cointelegraph](https://www.tradingview.com/news/cointelegraph:e59c32089094b:0-polymarket-quietly-introduces-taker-fees-on-15-minute-crypto-markets/)
- [Polymarket Just Changed Its Fees — What Bot Traders Need to Know — Coinmonks](https://medium.com/coinmonks/polymarket-just-changed-its-fees-heres-what-bot-traders-need-to-know-c11132e55d5c)
- [Polymarket Introduces Taker Fees for 15-Min Crypto — KuCoin](https://www.kucoin.com/news/flash/polymarket-introduces-taker-fees-for-15-minute-crypto-prediction-markets)
- [Polymarket Fees Explained 2026 — KuCoin](https://www.kucoin.com/blog/polymarket-fees-trading-guide-2026)
- [Polymarket Introduces Taker-Only Fees on 15-Minute Crypto — CoinMarketCap](https://coinmarketcap.com/academy/article/polymarket-introduces-fees-on-15-minute-crypto-bets)
- [Maker Rebates Program — Polymarket Documentation](https://docs.polymarket.com/developers/market-makers/maker-rebates-program)

### Resultados de bots ganadores

- [Arbitrage Bots Dominate Polymarket With Millions in Profits — Yahoo Finance](https://finance.yahoo.com/news/arbitrage-bots-dominate-polymarket-millions-100000888.html)
- [How Bots Make Millions on Polymarket While Humans Struggle — MEXC News](https://www.mexc.com/en-GB/news/417346)
- [Prediction Markets Are Turning Into a Bot Playground — Finance Magnates](https://www.financemagnates.com/trending/prediction-markets-are-turning-into-a-bot-playground/)

### Investigacion academica

- [Unravelling the Probabilistic Forest: Arbitrage in Prediction Markets — arXiv (2508.03474)](https://arxiv.org/abs/2508.03474)
- [Predicting Arbitrage Occurrences With ML in Live-Trading Crypto — Wiley](https://onlinelibrary.wiley.com/doi/full/10.1002/nem.70030)
- [High frequency momentum trading with cryptocurrencies — ScienceDirect](https://www.sciencedirect.com/science/article/abs/pii/S0275531919308062)
- [Overreaction as indicator for momentum: AAPL case — arXiv](https://arxiv.org/html/2602.18912v1)
- [HFT strategies, market fragility and price spikes — Springer](https://link.springer.com/article/10.1007/s10479-018-3019-4)
- [Optimal Threshold for HFT pairs trading via ML — Springer](https://link.springer.com/article/10.1007/s10614-025-10958-5)

### Guias generales

- [How Prediction Market Arbitrage Works (Polymarket, Kalshi) — Trevor Lasn](https://www.trevorlasn.com/blog/how-prediction-market-polymarket-kalshi-arbitrage-works)
- [Prediction Market Arbitrage Guide 2026 — NewYorkCityServers](https://newyorkcityservers.com/blog/prediction-market-arbitrage-guide)
- [Building a Prediction Market Arbitrage Bot — Substack](https://navnoorbawa.substack.com/p/building-a-prediction-market-arbitrage)
- [Arbitrage in Prediction Markets — Flashbots Collective](https://collective.flashbots.net/t/arbitrage-in-prediction-markets-strategies-impact-and-open-questions/5198)
