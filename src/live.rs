//! Live trading wrapper sobre polymarket_client_sdk_v2 oficial.
//!
//! El SDK oficial maneja toda la firma EIP-712, headers L1/L2, derive API keys, etc.
//! Acá solo agregamos: safety limits + executor de arb (2 FOK paralelos) +
//! tracking de fills.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use alloy::primitives::Address;
use polymarket_client_sdk_v2::clob::{
    types::{Amount, Side as PolySide, SignatureType},
    Client as ClobClient, Config as ClobConfig,
};
use polymarket_client_sdk_v2::types::{Decimal as PolyDecimal, U256};
use polymarket_client_sdk_v2::POLYGON;
use std::str::FromStr;
use std::sync::Mutex;
use tracing::{info, warn};

// Traits necesarios para los métodos en uso
use alloy::signers::Signer as _;

// NOTA: clob-v2.polymarket.com redirige 301 a clob.polymarket.com. El SDK
// no sigue redirects en POST, por eso usamos el host final directamente.
const CLOB_HOST: &str = "https://clob.polymarket.com";

#[derive(Debug, Clone)]
pub struct SafetyLimits {
    pub max_total_capital_usdc: rust_decimal::Decimal,
    pub max_per_trade_usdc: rust_decimal::Decimal,
    pub max_trades_per_hour: u32,
}

impl Default for SafetyLimits {
    fn default() -> Self {
        // Configuración de TEST: $3 por arb (notional total YES + NO),
        // $30 cap total (10 trades teóricos), 12 trades/h.
        Self {
            max_total_capital_usdc: rust_decimal::Decimal::from(30),
            max_per_trade_usdc: rust_decimal::Decimal::from(3),
            max_trades_per_hour: 12,
        }
    }
}

#[derive(Debug, Default)]
struct SafetyState {
    capital_used_usdc: rust_decimal::Decimal,
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

pub struct LiveExecutor {
    private_key_hex: String,
    /// FUNDER address (proxy/safe wallet) si SIGNATURE_TYPE != 0 (EOA).
    /// Si None → EOA puro (maker = signer).
    funder: Option<Address>,
    /// SignatureType: 0=EOA, 1=Proxy, 2=GnosisSafe.
    signature_type: SignatureType,
    limits: SafetyLimits,
    state: Mutex<SafetyState>,
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
    pub size_usdc: rust_decimal::Decimal,
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
    pub total_usdc: rust_decimal::Decimal,
    pub order_id_a: Option<String>,
    pub order_id_b: Option<String>,
    pub error_a: Option<String>,
    pub error_b: Option<String>,
    pub status: ArbStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbStatus {
    BothFilled,
    OnlyAFilled,
    OnlyBFilled,
    BothFailed,
}

impl LiveExecutor {
    /// Validate private key format and try authenticating once at startup.
    pub async fn init(private_key_hex: String, limits: SafetyLimits) -> Result<Self> {
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
        // Parse funder + signature_type del env
        let (funder, signature_type) = parse_funder_config()?;
        info!(
            funder = ?funder,
            sig_type = ?signature_type,
            "live_executor_config"
        );

        // Probar autenticación una vez para fallar rápido si hay problemas
        let signer = alloy::signers::local::LocalSigner::from_str(pk_trim)
            .context("LocalSigner from PRIVATE_KEY")?
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
        let _client = auth_builder.authenticate().await.context("CLOB authenticate")?;
        info!(addr = ?signer.address(), "live_executor_authenticated_ok");

        Ok(Self {
            private_key_hex: pk_trim.to_string(),
            funder,
            signature_type,
            limits,
            state: Mutex::new(SafetyState::default()),
        })
    }

    /// Health-check NO-destructivo: valida que el host + auth + endpoints
    /// READ funcionan llamando `api_keys()` y `balance_allowance()`.
    /// NO envía ninguna orden. Útil para diagnosticar problemas de host/URL
    /// sin riesgo financiero.
    pub async fn health_check(&self) -> Result<()> {
        use polymarket_client_sdk_v2::clob::types::request::BalanceAllowanceRequest;

        let signer = alloy::signers::local::LocalSigner::from_str(&self.private_key_hex)
            .context("signer")?
            .with_chain_id(Some(POLYGON));
        let config = ClobConfig::builder().use_server_time(true).build();
        let auth_builder = ClobClient::new(CLOB_HOST, config)
            .context("client init")?
            .authentication_builder(&signer)
            .signature_type(self.signature_type);
        let auth_builder = if let Some(f) = self.funder {
            auth_builder.funder(f)
        } else {
            auth_builder
        };
        let client = auth_builder.authenticate().await.context("authenticate")?;

        // Test 1: api_keys (GET request) - solo validamos que devuelve Ok
        match client.api_keys().await {
            Ok(_) => info!("health_check: api_keys OK"),
            Err(e) => warn!(error = %format!("{:#}", e), "health_check: api_keys FAILED"),
        }

        // Test 2: balance_allowance (GET request → valida approvals on-chain)
        match client
            .balance_allowance(BalanceAllowanceRequest::default())
            .await
        {
            Ok(b) => info!(result = ?b, "health_check: balance_allowance OK"),
            Err(e) => warn!(
                error = %format!("{:#}", e),
                "health_check: balance_allowance FAILED"
            ),
        }

        // Test 3: Construir una orden DRY (build + sign, NO post)
        // Esto valida que el flow de signing funciona localmente.
        use polymarket_client_sdk_v2::clob::types::{Amount, Side as PolySide};
        let dummy_token = U256::from(1u64); // token id ficticio, NO se envía
        let dummy_amount = Amount::usdc(PolyDecimal::from(1u32)).context("Amount usdc")?;
        match client
            .market_order()
            .token_id(dummy_token)
            .amount(dummy_amount)
            .side(PolySide::Buy)
            .build()
            .await
        {
            Ok(order) => match client.sign(&signer, order).await {
                Ok(_signed) => info!("health_check: build+sign OK (no posted)"),
                Err(e) => warn!(error = %format!("{:#}", e), "health_check: sign FAILED"),
            },
            Err(e) => warn!(error = %format!("{:#}", e), "health_check: build FAILED"),
        }

        Ok(())
    }

    pub fn validate(&self, size_usdc: rust_decimal::Decimal) -> Result<()> {
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

    /// Ejecuta UNA pierna direccional FOK. Para Strategy A (lag_arb / momentum).
    /// Retorna el order_id si fillea o un error explicativo.
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
            "live_executing_single_leg"
        );

        let signer = alloy::signers::local::LocalSigner::from_str(&self.private_key_hex)
            .context("signer init")?
            .with_chain_id(Some(POLYGON));
        let config = ClobConfig::builder().use_server_time(true).build();
        let auth_builder = ClobClient::new(CLOB_HOST, config)
            .context("client init")?
            .authentication_builder(&signer)
            .signature_type(self.signature_type);
        let auth_builder = if let Some(f) = self.funder {
            auth_builder.funder(f)
        } else {
            auth_builder
        };
        let client = auth_builder.authenticate().await.context("authenticate")?;

        let token = U256::from_str(&leg.token_id)
            .with_context(|| format!("parse token_id: {}", leg.token_id))?;
        let amount =
            PolyDecimal::from_str(&leg.size_usdc.to_string()).context("amount parse")?;
        let amount = Amount::usdc(amount).context("Amount::usdc")?;
        let side = match leg.side {
            ArbSide::Buy => PolySide::Buy,
            ArbSide::Sell => PolySide::Sell,
        };

        let result = async {
            let o = client
                .market_order()
                .token_id(token)
                .amount(amount)
                .side(side)
                .build()
                .await
                .context("build order")?;
            let s = client.sign(&signer, o).await.context("sign order")?;
            let r = client.post_order(s).await.context("post order")?;
            if !r.success {
                return Err(anyhow!("order success=false: {:?}", r));
            }
            Ok::<String, anyhow::Error>(r.order_id)
        }
        .await;

        {
            let mut state = self.state.lock().unwrap();
            state.recent_trade_times.push(Utc::now());
            state.capital_used_usdc += total;
        }

        let order_id = result.as_ref().ok().cloned();
        let error = result.as_ref().err().map(|e| format!("{:#}", e));
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
        })
    }

    /// Ejecuta 2 piernas FOK en paralelo. Retorna resultado con order IDs.
    pub async fn execute_arb(
        &self,
        market_id: String,
        leg_a: ArbLegOrder,
        leg_b: ArbLegOrder,
    ) -> Result<ArbTradeResult> {
        let total = leg_a.size_usdc + leg_b.size_usdc;
        self.validate(total)?;

        info!(mkt = %market_id, total = %total, "live_executing_arb");

        // Construimos un client fresco por trade (no compartimos state mutable
        // entre threads/tasks). El SDK maneja conn pool internamente.
        let signer = alloy::signers::local::LocalSigner::from_str(&self.private_key_hex)
            .context("signer init")?
            .with_chain_id(Some(POLYGON));
        let config = ClobConfig::builder().use_server_time(true).build();
        let auth_builder = ClobClient::new(CLOB_HOST, config)
            .context("client init")?
            .authentication_builder(&signer)
            .signature_type(self.signature_type);
        let auth_builder = if let Some(f) = self.funder {
            auth_builder.funder(f)
        } else {
            auth_builder
        };
        let client = auth_builder.authenticate().await.context("authenticate")?;

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

        // FOK paralelo. Cada futuro construye/firma/postea independientemente.
        let fut_a = async {
            let o = client
                .market_order()
                .token_id(token_a)
                .amount(amount_a)
                .side(side_a)
                .build()
                .await
                .context("build leg A")?;
            let s = client.sign(&signer, o).await.context("sign leg A")?;
            let r = client.post_order(s).await.context("post leg A")?;
            if !r.success {
                return Err(anyhow!("leg A success=false: {:?}", r));
            }
            Ok::<String, anyhow::Error>(r.order_id)
        };
        let fut_b = async {
            let o = client
                .market_order()
                .token_id(token_b)
                .amount(amount_b)
                .side(side_b)
                .build()
                .await
                .context("build leg B")?;
            let s = client.sign(&signer, o).await.context("sign leg B")?;
            let r = client.post_order(s).await.context("post leg B")?;
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
        // {:#} preserva la chain completa de anyhow (causa raíz)
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_limits_default_conservative() {
        let lim = SafetyLimits::default();
        assert_eq!(lim.max_total_capital_usdc, rust_decimal::Decimal::from(30));
        assert_eq!(lim.max_per_trade_usdc, rust_decimal::Decimal::from(3));
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
}
