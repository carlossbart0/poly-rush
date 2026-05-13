//! Entry point. Subcomandos:
//! - `polybot run`      → arranca el bot de arbitraje (DRY_RUN).
//! - `polybot stats`    → imprime stats acumulados del recorder.
//! - `polybot discover` → corre solo discovery una vez (debug).

use anyhow::Result;
use bot_polymarket_rust::{
    config::Settings, gamma::GammaClient, live, live_runner, recorder::Recorder, runner,
    tp_manager, trinity_runner,
};
use std::str::FromStr;
use tracing_subscriber::{prelude::*, EnvFilter};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    init_tracing();
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("run");

    let settings = Settings::from_env()?;
    match cmd {
        "run" => runner::run(settings).await,
        "trinity" => {
            // 3rd arg: duration seconds (default 600 = 10 min)
            let dur: u64 = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(600);
            trinity_runner::run(settings, dur).await
        }
        "smoke" => {
            // Smoke: corre el runner pero con stop file forzado tras 30s.
            // Util para validar conexion sin esperar opps reales.
            let stop_path = settings.stop_file_path.clone();
            let timer = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                std::fs::write(&stop_path, "smoke timeout\n").ok();
                tracing::info!("smoke_timer_fired");
            });
            let res = runner::run(settings).await;
            timer.abort();
            res
        }
        "stats" => {
            let rec = Recorder::open(&settings.db_path)?;
            let stats = rec.stats()?;
            println!("count: {}", stats.count);
            println!("total_pnl_teorico_usdc: {:.4}", stats.total_pnl);
            println!("avg_edge: {:.4}", stats.avg_edge);
            println!("max_edge: {:.4}", stats.max_edge);
            println!("total_notional_usdc: {:.4}", stats.total_notional);
            if let (Some(a), Some(b)) = (&stats.t_min, &stats.t_max) {
                println!("window: {} → {}", a, b);
            }
            Ok(())
        }
        "live-dry" => {
            // DRY-RUN del live runner: detecta + loguea, NO ejecuta.
            let dur: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600);
            live_runner::run(
                settings,
                dur,
                live_runner::LiveMode::DryRun,
                live::SafetyLimits::default(),
            )
            .await
        }
        "live" => {
            // LIVE REAL — ejecuta trades. Requiere PRIVATE_KEY en env.
            // Safety: $50 total, $5 por trade, 12 trades/h por default.
            let dur: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600);
            eprintln!(
                "⚠️  LIVE MODE: trades reales. \
                 max_total=$30  max_trade=$3  max_per_h=12. \
                 Crear state/STOP para parar."
            );
            live_runner::run(
                settings,
                dur,
                live_runner::LiveMode::Live,
                live::SafetyLimits::default(),
            )
            .await
        }
        "place-tp" => {
            // Postea limit SELL GTC con TP% sobre avg_price en cada posición abierta.
            // Default 15%. Owner = FUNDER_ADDRESS (proxy/safe) si SIGNATURE_TYPE != 0,
            // sino el address derivado del PRIVATE_KEY.
            let tp_pct_str = args.get(2).map(String::as_str).unwrap_or("15");
            let tp_pct = rust_decimal::Decimal::from_str_exact(tp_pct_str)
                .map_err(|e| anyhow::anyhow!("tp_pct invalido '{tp_pct_str}': {e}"))?;
            let pk = std::env::var("PRIVATE_KEY")
                .or_else(|_| std::env::var("POLYMARKET_PRIVATE_KEY"))
                .map_err(|_| anyhow::anyhow!("Falta env PRIVATE_KEY"))?;
            let sig_type_str = std::env::var("SIGNATURE_TYPE").unwrap_or_else(|_| "0".into());
            let signature_type = match sig_type_str.trim() {
                "0" => polymarket_client_sdk_v2::clob::types::SignatureType::Eoa,
                "1" => polymarket_client_sdk_v2::clob::types::SignatureType::Proxy,
                "2" => polymarket_client_sdk_v2::clob::types::SignatureType::GnosisSafe,
                other => return Err(anyhow::anyhow!("SIGNATURE_TYPE invalido: {other}")),
            };
            let funder: Option<alloy::primitives::Address> = match std::env::var("FUNDER_ADDRESS") {
                Ok(s) if !s.trim().is_empty() => Some(
                    s.trim()
                        .parse()
                        .map_err(|e| anyhow::anyhow!("FUNDER_ADDRESS invalido: {e}"))?,
                ),
                _ => None,
            };
            // Owner: si hay funder usar funder (las shares estan en el proxy);
            // sino el address derivado del private key.
            let owner: alloy::primitives::Address = if let Some(f) = funder {
                f
            } else {
                use alloy::signers::Signer as _;
                let pk_trim = pk.trim().trim_start_matches("0x");
                let s = alloy::signers::local::LocalSigner::from_str(pk_trim)?
                    .with_chain_id(Some(polymarket_client_sdk_v2::POLYGON));
                s.address()
            };
            eprintln!(
                "⚠️  TP MODE: postea limit SELL GTC al +{}% sobre avg_price. \
                 owner={} sig_type={:?}",
                tp_pct, owner, signature_type
            );
            let report =
                tp_manager::place_tp_on_open_positions(pk, owner, signature_type, funder, tp_pct)
                    .await?;
            println!("=== TP REPORT ===");
            println!("total_positions:           {}", report.total_positions);
            println!("posted_ok:                 {}", report.posted_ok);
            println!("posted_failed:             {}", report.posted_failed);
            println!("skipped_redeemable:        {}", report.skipped_redeemable);
            println!("skipped_zero_size:         {}", report.skipped_zero_size);
            println!(
                "skipped_tp_unprofitable:   {}",
                report.skipped_tp_capped_unprofitable
            );
            println!(
                "skipped_below_min_notional:{}",
                report.skipped_below_min_notional
            );
            for o in &report.orders {
                let st = match (&o.order_id, &o.error) {
                    (Some(id), _) => format!("OK {id}"),
                    (None, Some(e)) => format!("ERR {e}"),
                    _ => "UNKNOWN".to_string(),
                };
                println!(
                    "  mkt={}.. size={} avg={} cur={} tp={} -> {}",
                    &o.condition_id[..o.condition_id.len().min(14)],
                    o.size,
                    o.avg_price,
                    o.cur_price,
                    o.tp_price,
                    st
                );
            }
            Ok(())
        }
        "health-check" => {
            // Validación NO-destructiva: auth + reads API. NO envía órdenes.
            let pk = std::env::var("PRIVATE_KEY")
                .or_else(|_| std::env::var("POLYMARKET_PRIVATE_KEY"))
                .map_err(|_| anyhow::anyhow!("Falta env PRIVATE_KEY"))?;
            let executor = live::LiveExecutor::init(pk, live::SafetyLimits::default()).await?;
            executor.health_check().await?;
            Ok(())
        }
        "derive-keys" => {
            // Auth con tu wallet y derivar API keys (1 vez por wallet).
            // dotenvy se cargó en Settings::from_env(); leer aquí.
            let pk = std::env::var("PRIVATE_KEY")
                .or_else(|_| std::env::var("POLYMARKET_PRIVATE_KEY"))
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Falta env PRIVATE_KEY o POLYMARKET_PRIVATE_KEY (en .env o env shell)"
                    )
                })?;
            // No log del PK.
            tracing::info!(
                "derive-keys: PK cargada ({}* chars), autenticando con CLOB...",
                pk.len()
            );
            let executor = live::LiveExecutor::init(pk, live::SafetyLimits::default()).await?;
            let _ = executor; // solo validamos auth OK
            println!("derive-keys: AUTH OK. SDK oficial autenticado.");
            println!("Tu wallet ya tiene API key creada/derivada (el SDK maneja esto internamente).");
            Ok(())
        }
        "discover" => {
            let g = GammaClient::new(&settings.gamma_url)?;
            let m = g
                .discover_hourly_crypto(settings.arb_horizon_seconds, settings.arb_gamma_limit)
                .await?;
            println!("discovered: {}", m.len());
            for x in m.iter().take(10) {
                println!("  {}  {}", x.condition_id, x.slug);
            }
            Ok(())
        }
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("unknown subcommand: {}", other);
            print_help();
            std::process::exit(2);
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| {
        EnvFilter::new("info,bot_polymarket_rust=debug")
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_ansi(false),
        )
        .init();
}

fn print_help() {
    println!("polybot — arbitraje bilateral Polymarket (DRY_RUN)");
    println!();
    println!("USAGE:");
    println!("  polybot <command> [args]");
    println!("    discover         lista markets activos crypto");
    println!("    smoke            arranca y para tras 30s (validacion)");
    println!("    run              strategy B sola, DRY-RUN, persiste a jsonl");
    println!("    trinity [s]      A+B+C en paralelo, DRY-RUN, default 600s");
    println!("    live-dry [s]     strategy B con flow live pero sin enviar");
    println!("    live [s]         LIVE REAL — envia trades, $5 max, requiere PK");
    println!("    place-tp [pct]   postea limit SELL al +pct% sobre avg en cada posicion (default 15)");
    println!("    derive-keys      autentica wallet y deriva API keys (1 vez)");
    println!("    stats            stats acumuladas de state/rust_bot.jsonl");
    println!("    help             muestra esto");
    println!();
    println!("DEFAULTS:");
    println!("  Sin argumentos => run");
    println!();
    println!("ENV:");
    println!("  Ver .env.example");
    println!("  Crear state/STOP para parar limpio.");
}
