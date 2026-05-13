//! Live trading wrapper sobre polymarket_client_sdk_v2 oficial.
//!
//! El SDK oficial maneja toda la firma EIP-712, headers L1/L2, derive API keys, etc.
//! Aca agregamos: safety limits + executor de trades + tracking de fills +
//! TP-on-place automatico + cliente CLOB cacheado (1 sola auth por session).

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use polymarket_client_sdk_v2::auth::{state::Authenticated, Normal};
use polymarket_client_sdk_v2::clob::{
    types::{Amount, OrderType, Side as PolySide, SignatureType},
    Client as ClobClient, Config as ClobConfig,
};
use polymarket_client_sdk_v2::types::{Decimal as PolyDecimal, U256};
use polymarket_client_sdk_v2::POLYGON;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Instant;
use tracing::{info, warn};

// Traits necesarios para los metodos en uso
use alloy::signers::Signer as _;

// NOTA: clob-v2.polymarket.com redirige 301 a clob.polymarket.com. El SDK
// no sigue redirects en POST, por eso usamos el host final directamente.
const CLOB_HOST: &str = "https://clob.polymarket.com";

/// TP price limites (mismos que tp_manager.rs).
const TP_MAX_PRICE: Decimal = dec!(0.99);
const TP_MIN_NOTIONAL_USDC: Decimal = dec!(1);
const TICK_LOW: Decimal = dec!(0.001);
const TICK_NORMAL: Decimal = dec!(0.01);
const PRICE_LOW_THRESHOLD: Decimal = dec!(0.10);
const PRICE_HIGH_THRESHOLD: Decimal = dec!(0.90);
const SIZE_LOT_SIZE: Decimal = dec!(0.01);

#[derive(Debug, Clone)]
pub struct SafetyLimits {
    pub max_total_capital_usdc: Decimal,
    pub max_per_trade_usdc: Decimal,
    pub max_trades_per_hour: u32,
}

impl Default for SafetyLimits {
    fn default() -> Self {
        // Configuracion de TEST: $3 por trade, $30 cap total, 12 trades/h.
        Self {
            max_total_capital_usdc: Decimal::from(30),
            max_per_trade_usdc: Decimal::from(3),
            max_trades_per_hour: 12,
        }
    }
}

impl SafetyLimits {
    /// Lee env vars y cae al default si no estan o son invalidas:
    ///   SAFETY_MAX_TOTAL_USDC     (ej "100")
    ///   SAFETY_MAX_PER_TRADE_USDC (ej "3")
    ///   SAFETY_MAX_TRADES_PER_HOUR (ej "50")
    pub fn from_env_or_default() -> Self {
        let mut s = Self::default();
        if let Ok(v) = std::env::var("SAFETY_MAX_TOTAL_USDC") {
            if let Ok(d) = Decimal::from_str_exact(v.trim()) {
                s.max_total_capital_usdc = d;
            }
        }
        if let Ok(v) = std::env::var("SAFETY_MAX_PER_TRADE_USDC") {
            if let Ok(d) = Decimal::from_str_exact(v.trim()) {
                s.max_per_trade_usdc = d;
            }
        }
        if let Ok(v) = std::env::var("SAFETY_MAX_TRADES_PER_HOUR") {
            if let Ok(d) = v.trim().parse::<u32>() {
                s.max_trades_per_hour = d;
            }
        }
        s
    }
}

#[derive(Debug, Default)]
struct SafetyState {
    capital_used_usdc: Decimal,
    recent_trade_times: Vec<DateTime<Utc>>,
}

impl SafetyState {
    fn prune_old(&mut self) {
        let cutoff = Utc::now() - chrono::Duration::hours(1);
        self.recent_trade_times.retain(|t| *t > cutoff);
    }
    fn recent_trades_count(&mut self) -> usize {
        self.prune_old();
        self.recent_trade_times.len()
    }
}

/// Cliente CLOB autenticado y signer, ambos cacheados al `init()` para evitar
/// re-autenticar en cada trade (300+ ms de handshake TLS + L1/L2 auth).
///
/// `Client<Authenticated<Normal>>` es internamente `Arc<ClientInner>`, asi que
/// clonarlo es barato (Arc clone) — lo hacemos en cada execute_* sin perder
/// performance.
pub struct LiveExecutor {
    signer: PrivateKeySigner,
    client: ClobClient<Authenticated<Normal>>,
    /// FUNDER address (proxy/safe wallet) si SIGNATURE_TYPE != 0 (EOA).
    funder: Option<Address>,
    /// SignatureType: 0=EOA, 1=Proxy, 2=GnosisSafe.
    signature_type: SignatureType,
    limits: SafetyLimits,
    state: Mutex<SafetyState>,
    /// Si Some, despues de cada entry exitoso se postea limit SELL GTC a
    /// `entry_price * (1 + tp_pct/100)` (cap 0.99). Si None, no se postea TP.
    tp_pct: Option<Decimal>,
}

/// Parse env vars FUNDER_ADDRESS + SIGNATURE_TYPE.
/// Default: EOA puro (sin funder).
fn parse_funder_config() -> Result<(Option<Address>, SignatureType)> {
    let sig_type_str = std::env::var("SIGNATURE_TYPE").unwrap_or_else(|_| "0".to_string());
    let sig_type = match sig_type_str.trim() {
        "0" => SignatureType::Eoa,
        "1" => SignatureType::Proxy,
        "2" => SignatureType::GnosisSafe,
        other => return Err(anyhow!("SIGNATURE_TYPE invalido: {}", other)),
    };
    let funder = match std::env::var("FUNDER_ADDRESS") {
        Ok(s) if !s.trim().is_empty() => {
            let addr: Address = s
                .trim()
                .parse()
                .with_context(|| format!("FUNDER_ADDRESS no es Address valida: {}", s))?;
            Some(addr)
        }
        _ => None,
    };
    Ok((funder, sig_type))
}

#[derive(Debug, Clone)]
pub struct ArbLegOrder {
    pub token_id: String,
    pub side: ArbSide,
    pub size_usdc: Decimal,
    /// Precio de entry esperado (best_ask en el momento de la decision).
    /// Usado para calcular tp_price y shares del TP automatico.
    pub entry_price: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct ArbTradeResult {
    pub timestamp: DateTime<Utc>,
    pub market_id: String,
    pub total_usdc: Decimal,
    pub order_id_a: Option<String>,
    pub order_id_b: Option<String>,
    pub error_a: Option<String>,
    pub error_b: Option<String>,
    pub status: ArbStatus,
    /// Si se posteo TP, contiene el order_id del limit SELL. None si no se
    /// posteo (tp_pct=None, entry fallido, o TP failure).
    pub tp_order_id: Option<String>,
    /// Si el intento de TP fallo, contiene el error. None si OK o no se intento.
    pub tp_error: Option<String>,
    /// Precio TP calculado (avg_price * (1 + tp_pct/100), capped + rounded).
    pub tp_price: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbStatus {
    BothFilled,
    OnlyAFilled,
    OnlyBFilled,
    BothFailed,
}

/// Tick size segun precio (regla Polymarket).
fn tick_size_for(price: Decimal) -> Decimal {
    if price < PRICE_LOW_THRESHOLD || price > PRICE_HIGH_THRESHOLD {
        TICK_LOW
    } else {
        TICK_NORMAL
    }
}

/// Redondea price hacia abajo al tick (para SELL — no queremos quedar arriba del target).
fn round_down_to_tick(price: Decimal, tick: Decimal) -> Decimal {
    if tick.is_zero() {
        return price;
    }
    let n = (price / tick).floor();
    n * tick
}

impl LiveExecutor {
    /// Inicializa el executor: valida PK, autentica con el CLOB UNA VEZ, y
    /// cachea cliente + signer para reutilizar en cada trade.
    ///
    /// `tp_pct`: si Some, cada entry exitoso postea un limit SELL al
    /// `entry_price * (1 + tp_pct/100)` (TP-on-place). Si None, no postea TP.
    pub async fn init(
        private_key_hex: String,
        limits: SafetyLimits,
        tp_pct: Option<Decimal>,
    ) -> Result<Self> {
        let pk_trim = private_key_hex.trim().trim_start_matches("0x");
        if pk_trim.len() != 64 {
            return Err(anyhow!(
                "PRIVATE_KEY debe ser 64 hex chars (opcionalmente prefijada 0x)"
            ));
        }
        for c in pk_trim.chars() {
            if !c.is_ascii_hexdigit() {
                return Err(anyhow!("PRIVATE_KEY contiene caracter no-hex"));
            }
        }
        let (funder, signature_type) = parse_funder_config()?;
        info!(
            funder = ?funder,
            sig_type = ?signature_type,
            tp_pct = ?tp_pct,
            "live_executor_config"
        );

        let signer = PrivateKeySigner::from_str(pk_trim)
            .context("PrivateKeySigner from PRIVATE_KEY")?
            .with_chain_id(Some(POLYGON));
        let config = ClobConfig::builder().use_server_time(true).build();
        let auth_builder = ClobClient::new(CLOB_HOST, config)
            .context("ClobClient::new")?
            .authentication_builder(&signer)
            .signature_type(signature_type);
        let auth_builder = if let Some(f) = funder {
            auth_builder.funder(f)
        } else {
            auth_builder
        };
        let client = auth_builder.authenticate().await.context("CLOB authenticate")?;
        info!(addr = ?signer.address(), "live_executor_authenticated_ok");

        Ok(Self {
            signer,
            client,
            funder,
            signature_type,
            limits,
            state: Mutex::new(SafetyState::default()),
            tp_pct,
        })
    }

    /// Health-check NO-destructivo: usa el cliente cacheado para validar
    /// endpoints READ. NO envia ninguna orden.
    pub async fn health_check(&self) -> Result<()> {
        use polymarket_client_sdk_v2::clob::types::request::BalanceAllowanceRequest;

        // api_keys
        match self.client.api_keys().await {
            Ok(_) => info!("health_check: api_keys OK"),
            Err(e) => warn!(error = %format!("{:#}", e), "health_check: api_keys FAILED"),
        }

        // balance_allowance
        match self
            .client
            .balance_allowance(BalanceAllowanceRequest::default())
            .await
        {
            Ok(b) => info!(result = ?b, "health_check: balance_allowance OK"),
            Err(e) => warn!(
                error = %format!("{:#}", e),
                "health_check: balance_allowance FAILED"
            ),
        }

        // build+sign dummy (no posted)
        let dummy_token = U256::from(1u64);
        let dummy_amount = Amount::usdc(PolyDecimal::from(1u32)).context("Amount usdc")?;
        match self
            .client
            .market_order()
            .token_id(dummy_token)
            .amount(dummy_amount)
            .side(PolySide::Buy)
            .build()
            .await
        {
            Ok(order) => match self.client.sign(&self.signer, order).await {
                Ok(_signed) => info!("health_check: build+sign OK (no posted)"),
                Err(e) => warn!(error = %format!("{:#}", e), "health_check: sign FAILED"),
            },
            Err(e) => warn!(error = %format!("{:#}", e), "health_check: build FAILED"),
        }

        Ok(())
    }

    pub fn validate(&self, size_usdc: Decimal) -> Result<()> {
        if size_usdc > self.limits.max_per_trade_usdc {
            return Err(anyhow!(
                "size {} > max_per_trade {}",
                size_usdc,
                self.limits.max_per_trade_usdc
            ));
        }
        let mut state = self.state.lock().unwrap();
        if state.capital_used_usdc + size_usdc > self.limits.max_total_capital_usdc {
            return Err(anyhow!(
                "capital used {} + new {} > max {}",
                state.capital_used_usdc,
                size_usdc,
                self.limits.max_total_capital_usdc
            ));
        }
        let recent = state.recent_trades_count();
        if recent >= self.limits.max_trades_per_hour as usize {
            return Err(anyhow!(
                "rate limit: {} trades/h >= max {}",
                recent,
                self.limits.max_trades_per_hour
            ));
        }
        Ok(())
    }

    /// Postea limit SELL GTC para TP automatico. Asume que el entry fue
    /// exitoso. Calcula tp_price y shares desde entry_price y size_usdc.
    /// Retorna order_id si OK o un error explicativo si fallo.
    async fn place_tp_for_entry(
        &self,
        token_id: U256,
        entry_price: Decimal,
        size_usdc: Decimal,
        tp_pct: Decimal,
    ) -> Result<String> {
        if entry_price <= Decimal::ZERO {
            return Err(anyhow!("entry_price invalido: {}", entry_price));
        }

        // shares = size_usdc / entry_price, redondeadas down al lot 0.01
        let raw_shares = size_usdc / entry_price;
        let shares = round_down_to_tick(raw_shares, SIZE_LOT_SIZE);
        if shares <= Decimal::ZERO {
            return Err(anyhow!(
                "shares invalidas: {} (size={}, entry={})",
                shares,
                size_usdc,
                entry_price
            ));
        }

        // tp_price = entry * (1 + pct/100), cap 0.99, redondeado down al tick
        let raw_tp = entry_price * (Decimal::ONE + tp_pct / dec!(100));
        let capped = raw_tp.min(TP_MAX_PRICE);
        let tick = tick_size_for(capped);
        let tp_price = round_down_to_tick(capped, tick);
        if tp_price <= entry_price {
            return Err(anyhow!(
                "tp_price <= entry: tp={} entry={} (cap dejo sin margen)",
                tp_price,
                entry_price
            ));
        }
        let notional = tp_price * shares;
        if notional < TP_MIN_NOTIONAL_USDC {
            return Err(anyhow!(
                "tp notional {} < min {}",
                notional,
                TP_MIN_NOTIONAL_USDC
            ));
        }

        let price_poly = PolyDecimal::from_str(&tp_price.to_string()).context("tp_price parse")?;
        let size_poly = PolyDecimal::from_str(&shares.to_string()).context("shares parse")?;

        let resp = self
            .client
            .limit_order()
            .token_id(token_id)
            .side(PolySide::Sell)
            .price(price_poly)
            .size(size_poly)
            .order_type(OrderType::GTC)
            .build_sign_and_post(&self.signer)
            .await
            .context("place TP limit SELL")?;
        Ok(resp.order_id)
    }

    /// Ejecuta UNA pierna direccional FOK. Para Strategy A (lag_arb / momentum).
    /// Si `tp_pct` esta configurado en el executor y el entry fillea, postea
    /// inmediatamente un limit SELL GTC al precio TP usando el cliente cacheado.
    pub async fn execute_single_leg(
        &self,
        market_id: String,
        leg: ArbLegOrder,
    ) -> Result<ArbTradeResult> {
        let total = leg.size_usdc;
        self.validate(total)?;

        info!(
            mkt = %market_id,
            token = %leg.token_id,
            side = ?leg.side,
            size = %total,
            entry_px = %leg.entry_price,
            tp_pct = ?self.tp_pct,
            "live_executing_single_leg"
        );

        let t_start = Instant::now();

        let token = U256::from_str(&leg.token_id)
            .with_context(|| format!("parse token_id: {}", leg.token_id))?;
        let amount =
            PolyDecimal::from_str(&leg.size_usdc.to_string()).context("amount parse")?;
        let amount = Amount::usdc(amount).context("Amount::usdc")?;
        let side = match leg.side {
            ArbSide::Buy => PolySide::Buy,
            ArbSide::Sell => PolySide::Sell,
        };

        // --- BUILD ---
        let t_build_start = Instant::now();
        let order = self
            .client
            .market_order()
            .token_id(token)
            .amount(amount)
            .side(side)
            .build()
            .await;
        let t_build_ms = t_build_start.elapsed().as_millis() as u64;
        let order = match order {
            Ok(o) => o,
            Err(e) => {
                let t_total_ms = t_start.elapsed().as_millis() as u64;
                warn!(
                    mkt = %market_id,
                    stage = "build",
                    t_build_ms,
                    t_total_ms,
                    err = %format!("{:#}", e),
                    "live_trade_latency_failed"
                );
                return Err(anyhow::Error::new(e).context("build order"));
            }
        };

        // --- SIGN ---
        let t_sign_start = Instant::now();
        let signed = self.client.sign(&self.signer, order).await;
        let t_sign_ms = t_sign_start.elapsed().as_millis() as u64;
        let signed = match signed {
            Ok(s) => s,
            Err(e) => {
                let t_total_ms = t_start.elapsed().as_millis() as u64;
                warn!(
                    mkt = %market_id,
                    stage = "sign",
                    t_build_ms,
                    t_sign_ms,
                    t_total_ms,
                    err = %format!("{:#}", e),
                    "live_trade_latency_failed"
                );
                return Err(anyhow::Error::new(e).context("sign order"));
            }
        };

        // --- POST ---
        let t_post_start = Instant::now();
        let post_result = self.client.post_order(signed).await;
        let t_post_ms = t_post_start.elapsed().as_millis() as u64;
        let t_total_ms = t_start.elapsed().as_millis() as u64;

        // Update safety state SIEMPRE (intento cuenta para rate limit)
        {
            let mut state = self.state.lock().unwrap();
            state.recent_trade_times.push(Utc::now());
            state.capital_used_usdc += total;
        }

        let (order_id, error) = match post_result {
            Ok(r) if r.success => {
                info!(
                    mkt = %market_id,
                    t_build_ms,
                    t_sign_ms,
                    t_post_ms,
                    t_total_ms,
                    order_id = %r.order_id,
                    "live_trade_latency_ok"
                );
                (Some(r.order_id), None)
            }
            Ok(r) => {
                let err_str = format!("order success=false: {:?}", r);
                warn!(
                    mkt = %market_id,
                    stage = "post_unsuccess",
                    t_build_ms,
                    t_sign_ms,
                    t_post_ms,
                    t_total_ms,
                    err = %err_str,
                    "live_trade_latency_failed"
                );
                (None, Some(err_str))
            }
            Err(e) => {
                let err_str = format!("post order: {:#}", e);
                warn!(
                    mkt = %market_id,
                    stage = "post",
                    t_build_ms,
                    t_sign_ms,
                    t_post_ms,
                    t_total_ms,
                    err = %err_str,
                    "live_trade_latency_failed"
                );
                (None, Some(err_str))
            }
        };

        // --- TP-on-place (solo si entry fillea y tp_pct configurado) ---
        let (tp_order_id, tp_error, tp_price_logged) = if let (Some(_), Some(tp_pct)) =
            (&order_id, self.tp_pct)
        {
            // Sleep 4s para que Polymarket actualice el balance de shares antes
            // del TP. Sin esto, la limit SELL falla con "balance: 0" porque el
            // matching engine y el balance store no se sincronizan al instante
            // (race condition observado en runs reales).
            info!(
                mkt = %market_id,
                "live_tp_waiting_balance_update"
            );
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;

            let t_tp_start = Instant::now();
            let entry_price = leg.entry_price;
            match self
                .place_tp_for_entry(token, entry_price, total, tp_pct)
                .await
            {
                Ok(tp_id) => {
                    let t_tp_ms = t_tp_start.elapsed().as_millis() as u64;
                    info!(
                        mkt = %market_id,
                        tp_order_id = %tp_id,
                        entry_px = %entry_price,
                        tp_pct = %tp_pct,
                        t_tp_ms,
                        "live_tp_on_place_ok"
                    );
                    // computamos tp_price para retornar (mismo calc del helper)
                    let raw_tp = entry_price * (Decimal::ONE + tp_pct / dec!(100));
                    let capped = raw_tp.min(TP_MAX_PRICE);
                    let tick = tick_size_for(capped);
                    let tp_price = round_down_to_tick(capped, tick);
                    (Some(tp_id), None, Some(tp_price))
                }
                Err(e) => {
                    let err_str = format!("{:#}", e);
                    warn!(
                        mkt = %market_id,
                        err = %err_str,
                        "live_tp_on_place_failed"
                    );
                    (None, Some(err_str), None)
                }
            }
        } else {
            (None, None, None)
        };

        let status = if order_id.is_some() {
            ArbStatus::OnlyAFilled
        } else {
            ArbStatus::BothFailed
        };
        Ok(ArbTradeResult {
            timestamp: Utc::now(),
            market_id,
            total_usdc: total,
            order_id_a: order_id,
            order_id_b: None,
            error_a: error,
            error_b: None,
            status,
            tp_order_id,
            tp_error,
            tp_price: tp_price_logged,
        })
    }

    /// Ejecuta 2 piernas FOK en paralelo. Mantenido por compatibilidad con
    /// posibles strategies bilaterales (no usado por Strategy A actual).
    /// Usa el cliente cacheado.
    pub async fn execute_arb(
        &self,
        market_id: String,
        leg_a: ArbLegOrder,
        leg_b: ArbLegOrder,
    ) -> Result<ArbTradeResult> {
        let total = leg_a.size_usdc + leg_b.size_usdc;
        self.validate(total)?;

        info!(mkt = %market_id, total = %total, "live_executing_arb");

        let token_a = U256::from_str(&leg_a.token_id)
            .with_context(|| format!("parse token_id A: {}", leg_a.token_id))?;
        let token_b = U256::from_str(&leg_b.token_id)
            .with_context(|| format!("parse token_id B: {}", leg_b.token_id))?;

        let amount_a =
            PolyDecimal::from_str(&leg_a.size_usdc.to_string()).context("amount A parse")?;
        let amount_b =
            PolyDecimal::from_str(&leg_b.size_usdc.to_string()).context("amount B parse")?;
        let amount_a = Amount::usdc(amount_a).context("Amount::usdc A")?;
        let amount_b = Amount::usdc(amount_b).context("Amount::usdc B")?;

        let side_a = match leg_a.side {
            ArbSide::Buy => PolySide::Buy,
            ArbSide::Sell => PolySide::Sell,
        };
        let side_b = match leg_b.side {
            ArbSide::Buy => PolySide::Buy,
            ArbSide::Sell => PolySide::Sell,
        };

        // FOK paralelo. Cliente reusado (Arc cloneable internamente).
        let client_a = self.client.clone();
        let client_b = self.client.clone();
        let signer_a = self.signer.clone();
        let signer_b = self.signer.clone();
        let fut_a = async move {
            let o = client_a
                .market_order()
                .token_id(token_a)
                .amount(amount_a)
                .side(side_a)
                .build()
                .await
                .context("build leg A")?;
            let s = client_a.sign(&signer_a, o).await.context("sign leg A")?;
            let r = client_a.post_order(s).await.context("post leg A")?;
            if !r.success {
                return Err(anyhow!("leg A success=false: {:?}", r));
            }
            Ok::<String, anyhow::Error>(r.order_id)
        };
        let fut_b = async move {
            let o = client_b
                .market_order()
                .token_id(token_b)
                .amount(amount_b)
                .side(side_b)
                .build()
                .await
                .context("build leg B")?;
            let s = client_b.sign(&signer_b, o).await.context("sign leg B")?;
            let r = client_b.post_order(s).await.context("post leg B")?;
            if !r.success {
                return Err(anyhow!("leg B success=false: {:?}", r));
            }
            Ok::<String, anyhow::Error>(r.order_id)
        };
        let (res_a, res_b) = tokio::join!(fut_a, fut_b);

        {
            let mut state = self.state.lock().unwrap();
            state.recent_trade_times.push(Utc::now());
            state.capital_used_usdc += total;
        }

        let order_id_a = res_a.as_ref().ok().cloned();
        let order_id_b = res_b.as_ref().ok().cloned();
        let error_a = res_a.as_ref().err().map(|e| format!("{:#}", e));
        let error_b = res_b.as_ref().err().map(|e| format!("{:#}", e));
        let status = match (&order_id_a, &order_id_b) {
            (Some(_), Some(_)) => ArbStatus::BothFilled,
            (Some(_), None) => ArbStatus::OnlyAFilled,
            (None, Some(_)) => ArbStatus::OnlyBFilled,
            (None, None) => ArbStatus::BothFailed,
        };
        if matches!(status, ArbStatus::OnlyAFilled | ArbStatus::OnlyBFilled) {
            warn!(
                mkt = %market_id,
                status = ?status,
                "live_partial_fill_LEG_EXPOSED"
            );
        }
        Ok(ArbTradeResult {
            timestamp: Utc::now(),
            market_id,
            total_usdc: total,
            order_id_a,
            order_id_b,
            error_a,
            error_b,
            status,
            tp_order_id: None,
            tp_error: None,
            tp_price: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_limits_default_conservative() {
        let lim = SafetyLimits::default();
        assert_eq!(lim.max_total_capital_usdc, Decimal::from(30));
        assert_eq!(lim.max_per_trade_usdc, Decimal::from(3));
        assert_eq!(lim.max_trades_per_hour, 12);
    }

    #[test]
    fn safety_state_prunes_old_trades() {
        let mut s = SafetyState::default();
        s.recent_trade_times
            .push(Utc::now() - chrono::Duration::hours(2));
        s.recent_trade_times.push(Utc::now());
        s.prune_old();
        assert_eq!(s.recent_trade_times.len(), 1);
    }

    #[test]
    fn arb_side_maps() {
        assert_eq!(ArbSide::Buy as u8, 0);
        assert_eq!(ArbSide::Sell as u8, 1);
    }

    #[test]
    fn tick_size_zones() {
        assert_eq!(tick_size_for(dec!(0.50)), TICK_NORMAL);
        assert_eq!(tick_size_for(dec!(0.05)), TICK_LOW);
        assert_eq!(tick_size_for(dec!(0.95)), TICK_LOW);
    }

    #[test]
    fn round_down_basic() {
        assert_eq!(round_down_to_tick(dec!(0.567), TICK_NORMAL), dec!(0.56));
        assert_eq!(round_down_to_tick(dec!(0.045), TICK_LOW), dec!(0.045));
    }
}
