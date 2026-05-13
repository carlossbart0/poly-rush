# Polybot — comandos operativos

Workflow tipico: iniciar live para abrir posiciones, postear TPs sobre las posiciones abiertas, parar el bot.

## Pre-requisitos

- Estar en el directorio del proyecto: `cd C:\Users\Py\Documents\bot-polymarket-rust`
- `.env` configurado con `PRIVATE_KEY`, `FUNDER_ADDRESS`, `SIGNATURE_TYPE`
- Binario release compilado en `target\release\polybot.exe`

Si necesitas recompilar tras un cambio:

```powershell
C:\Users\Py\rust-portable\installed\bin\cargo.exe build --release
```

## 1. Iniciar el bot en modo live

Ejecuta Strategy A (momentum CEX direccional). Duracion en segundos.

```powershell
.\target\release\polybot.exe live 300
```

Variantes:

```powershell
# 5 minutos
.\target\release\polybot.exe live 300

# 30 minutos
.\target\release\polybot.exe live 1800

# Sin limite practico (hasta que crees state\STOP)
.\target\release\polybot.exe live 999999
```

Limites de seguridad activos por default: `$30` total, `$3` por trade, `12` trades por hora.

Para ver mas detalle en stdout:

```powershell
$env:RUST_LOG="info,bot_polymarket_rust=debug"
.\target\release\polybot.exe live 300
```

## 2. Postear Take Profit sobre posiciones abiertas

Consulta las posiciones reales en tu wallet via Data API y postea un limit SELL GTC al `+pct%` sobre el `avg_price` de cada una.

```powershell
# 15% de TP (default)
.\target\release\polybot.exe place-tp 15

# Otro porcentaje
.\target\release\polybot.exe place-tp 10
.\target\release\polybot.exe place-tp 25
```

Salida: report con cuantos TPs se postearon, cuantos fallaron, y `order_id` de cada uno exitoso.

Notas:

- Si `avg_price * (1 + pct/100) > 0.99`, el TP se capa a `0.99` (limite Polymarket).
- Si el size redondeado es muy chico para minimo `$1` notional, se saltea.
- Si ya existe una orden TP previa para esa posicion, va a fallar con `not enough balance / allowance` (la orden vieja reserva las shares). En ese caso, cancelar la orden vieja primero (manual en polymarket.com → Portfolio → Open Orders).

## 3. Parar el bot limpio

Desde otra terminal en el mismo directorio:

```powershell
ni state\STOP -ItemType File -Force
```

El bot detecta el archivo en menos de 2 segundos, cierra WS, cierra CEX feed, y termina ordenadamente.

Si despues queres reiniciar, borra primero el STOP:

```powershell
Remove-Item state\STOP -ErrorAction SilentlyContinue
```

## 4. Otros comandos utiles

### Probar sin enviar trades reales

Ejecuta strategy A en modo deteccion pura. NO envia ordenes. Logs en `analysis_output\live_decisions.jsonl`.

```powershell
.\target\release\polybot.exe live-dry 180
```

### Verificar auth y conexion sin riesgo

```powershell
.\target\release\polybot.exe health-check
```

### Descubrir markets crypto activos

```powershell
.\target\release\polybot.exe discover
```

### Stats acumuladas

```powershell
.\target\release\polybot.exe stats
```

### Ayuda

```powershell
.\target\release\polybot.exe help
```

## 5. Workflow recomendado en una sesion

```powershell
# 1. Asegurarse que no hay STOP de runs anteriores
Remove-Item state\STOP -ErrorAction SilentlyContinue

# 2. Lanzar el bot 30 minutos
.\target\release\polybot.exe live 1800

# (espera o cierra cuando quieras desde OTRA terminal con: ni state\STOP -ItemType File -Force)

# 3. Una vez parado, postear TPs sobre las posiciones que quedaron abiertas
.\target\release\polybot.exe place-tp 15

# 4. Si despues haces otra corrida, repetir desde el paso 1
```

## 6. Donde ver lo que paso

| Archivo | Contenido |
|---|---|
| `analysis_output\live_decisions.jsonl` | Una linea por cada entry detectada (incluye decisiones de DRY-RUN y live) |
| `analysis_output\live_trades.jsonl` | Una linea por cada trade enviado al CLOB (con `order_id` si exitoso, `error` si fallo) |
| stdout / logs | Logs estructurados — usar `$env:RUST_LOG="debug"` para mas detalle |

Para inspeccionar los trades enviados:

```powershell
Get-Content analysis_output\live_trades.jsonl
```

Para contar cuantos entries detecto el ultimo run:

```powershell
(Get-Content analysis_output\live_decisions.jsonl).Count
```
