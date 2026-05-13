# bot-polymarket-rust

Bot de arbitraje bilateral en Polymarket — port Rust del bot Python (`../bot-polymarket`).

## Objetivo

Detectar `ask(YES) + ask(NO) + fees < 1` en mercados crypto cortos (5m/15m/1h)
y persistir cada oportunidad en SQLite. Validacion paralela contra el bot
Python: parity de detection counts + suma de `expected_pnl_usdc`.

## NO HACE

- No firma ordenes EIP-712 (DRY_RUN puro).
- No ejecuta trades reales.
- No toca `../bot-polymarket/state/bot.db`.
- No implementa `lag_arb` (advisor flagged como predictivo, no arb real).

## Stack

- `tokio` async runtime
- `tokio-tungstenite` 0.21 (WS al CLOB)
- `reqwest` (HTTP a Gamma)
- `rust_decimal` (money, NEVER f64)
- `rusqlite` con `bundled` (SQLite compilado in-tree)
- `tracing` + `tracing-subscriber` (structured logs)

## Setup

### 1. Instalar Rust

```sh
# Toolchain GNU (no requiere VS C++ build tools):
C:\Users\Py\rustup-init.exe -y --default-toolchain stable-x86_64-pc-windows-gnu --profile minimal --no-modify-path
```

Si ya tenes VS Build Tools instalados podes usar el msvc target estandar.

### 2. Compilar

```sh
cd C:\Users\Py\Documents\bot-polymarket-rust
$HOME/.cargo/bin/cargo.exe build --release
```

### 3. Configurar .env

```sh
cp .env.example .env
# Editar segun tus parametros (defaults sirven para empezar)
```

## Comandos

```sh
# Discovery una vez (smoke test offline-ish):
cargo run --release -- discover

# Bot en DRY_RUN — corre hasta que cree state/STOP:
cargo run --release -- run

# Stats acumulados:
cargo run --release -- stats

# Parar el bot (en otra terminal):
touch state/STOP

# Tests unitarios + integration:
cargo test --release
```

## Verificacion de paridad con Python

```sh
# Ventana de 5 min: arrancar ambos bots y comparar.
# Terminal 1 (Rust):
cargo run --release -- run

# Terminal 2 (Python):
cd ../bot-polymarket && uv run python -m polymarket_bot arb --dry-run

# Tras 5 min, parar ambos y comparar:
cargo run --release -- stats
# vs
sqlite3 ../bot-polymarket/state/bot.db \
  "SELECT COUNT(*), SUM(CAST(json_extract(payload,'$.expected_pnl_usdc') AS REAL)) \
   FROM arb_opportunities WHERE detected_at > datetime('now','-5 minutes');"
```

Tolerancia: ±20% en counts y en sum PnL teorico.

## Arquitectura

```
src/
├── types.rs       — ArbOpportunity, ArbLeg, Market, Side
├── fees.rs        — Polymarket fee math: rate * p * (1-p)
├── book_state.rs  — BookSide (BTreeMap precio→size), BookState
├── bilateral.rs   — Detector: ask(A)+ask(B)+fees<1
├── gamma.rs       — Gamma /markets discovery + crypto regex filter
├── poly_ws.rs     — WS client al CLOB (custom_feature_enabled=true)
├── recorder.rs    — SQLite persistence (arb_opportunities table)
├── config.rs      — Settings desde env
├── runner.rs      — Orquestador: discovery + WS + detector + recorder
└── main.rs        — Entry point + CLI (run/stats/discover)
```

## Decisiones / tradeoffs

- **DRY_RUN puro**: el goal es validar la deteccion. EIP-712 signing en Rust
  sumaria 500+ lineas con `alloy` y nada al PnL teorico. Si se quiere ir a
  live, se agrega un modulo `executor.rs` con alloy::primitives::Signature.
- **rusqlite con `bundled`**: SQLite compilado in-tree → cero dependencias
  externas. Costo: build time +5s.
- **`tokio-tungstenite` 0.21**: la API de Message variants cambio en 0.22+
  (`Utf8Bytes`, `Bytes`). Pin a 0.21 = match exacto al codigo.
- **Schema SQLite**: superset del Python (anade `notional_usdc`, `expected_pnl_usdc`
  como columnas en lugar de solo en `payload` JSON). Permite stats rapidos.
- **WS resubscribe via reconnect**: server no acepta resubscribe en misma
  conexion. Cuando se anaden markets nuevos, se notifica y se fuerza un
  reconnect limpio (espejo del fix `769e45a` del bot Python).

## Operacion

- Stop limpio: `touch state/STOP`. El bot loguea `stop_file_detected` y cierra.
- DB en `state/rust_bot.db`. WAL mode → safe para query concurrente.
- Logs estructurados a stdout (JSON con `RUST_LOG=info` + JSON layer).
