//! Persistencia: JSONL append-only (pure Rust, sin C deps).
//!
//! Cada opp se serializa como una linea JSON y se appendea al archivo.
//! Importable a SQLite con: `sqlite3 db.db ".mode json" ".import x.jsonl t"`
//! Para parity contra Python bot.
//!
//! Format por linea:
//! `{"strategy":"bilateral","market_id":"0x...","detected_at":"...",
//!   "legs":[...], "edge_per_unit":"0.094", "notional_usdc":"45",
//!   "expected_pnl_usdc":"4.7", "sum_ask":"0.90"}`

use crate::types::ArbOpportunity;
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Recorder {
    path: PathBuf,
    writer: Mutex<File>,
}

impl Recorder {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening {:?}", path))?;
        Ok(Self {
            path: path.to_path_buf(),
            writer: Mutex::new(file),
        })
    }

    pub fn insert(&self, opp: &ArbOpportunity) -> Result<u64> {
        let line = serde_json::to_string(opp)?;
        let mut w = self.writer.lock().unwrap();
        writeln!(w, "{line}")?;
        w.flush()?;
        Ok(0) // id sintetico — no usado por logic
    }

    /// Suma de expected_pnl_usdc en todas las opps.
    pub fn total_theoretical_pnl(&self) -> Result<f64> {
        let stats = self.stats()?;
        Ok(stats.total_pnl)
    }

    pub fn count(&self) -> Result<i64> {
        Ok(self.stats()?.count)
    }

    /// Re-lee el archivo y agrega stats. Idempotente.
    pub fn stats(&self) -> Result<RecorderStats> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => {
                return Ok(RecorderStats::default());
            }
        };
        let reader = BufReader::new(file);
        let mut count: i64 = 0;
        let mut total_pnl: f64 = 0.0;
        let mut sum_edge: f64 = 0.0;
        let mut max_edge: f64 = 0.0;
        let mut total_notional: f64 = 0.0;
        let mut t_min: Option<String> = None;
        let mut t_max: Option<String> = None;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if line.trim().is_empty() {
                continue;
            }
            let opp: ArbOpportunity = match serde_json::from_str(&line) {
                Ok(o) => o,
                Err(_) => continue,
            };
            count += 1;
            let pnl: f64 = opp
                .expected_pnl_usdc
                .to_string()
                .parse()
                .unwrap_or(0.0);
            let edge: f64 = opp.edge_per_unit.to_string().parse().unwrap_or(0.0);
            let notional: f64 = opp.notional_usdc.to_string().parse().unwrap_or(0.0);
            total_pnl += pnl;
            sum_edge += edge;
            if edge > max_edge {
                max_edge = edge;
            }
            total_notional += notional;
            let ts = opp.detected_at.to_rfc3339();
            match &t_min {
                None => t_min = Some(ts.clone()),
                Some(cur) if ts < *cur => t_min = Some(ts.clone()),
                _ => {}
            }
            match &t_max {
                None => t_max = Some(ts.clone()),
                Some(cur) if ts > *cur => t_max = Some(ts.clone()),
                _ => {}
            }
        }
        let avg_edge = if count > 0 {
            sum_edge / count as f64
        } else {
            0.0
        };
        Ok(RecorderStats {
            count,
            total_pnl,
            avg_edge,
            max_edge,
            total_notional,
            t_min,
            t_max,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct RecorderStats {
    pub count: i64,
    pub total_pnl: f64,
    pub avg_edge: f64,
    pub max_edge: f64,
    pub total_notional: f64,
    pub t_min: Option<String>,
    pub t_max: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ArbLeg, Side};
    use chrono::Utc;
    use rust_decimal_macros::dec;

    fn mk_opp() -> ArbOpportunity {
        ArbOpportunity {
            strategy: "bilateral".to_string(),
            market_id: "0xMKT".to_string(),
            detected_at: Utc::now(),
            legs: vec![
                ArbLeg {
                    asset_id: "TOKEN_A".to_string(),
                    side: Side::Buy,
                    price: dec!(0.40),
                    size_shares: dec!(50),
                },
                ArbLeg {
                    asset_id: "TOKEN_B".to_string(),
                    side: Side::Buy,
                    price: dec!(0.50),
                    size_shares: dec!(50),
                },
            ],
            edge_per_unit: dec!(0.094),
            notional_usdc: dec!(45),
            expected_pnl_usdc: dec!(4.7),
            sum_ask: dec!(0.90),
        }
    }

    #[test]
    fn insert_and_count_and_sum() {
        let path = std::env::temp_dir().join(format!(
            "rust_bot_test_{}.jsonl",
            chrono::Utc::now().timestamp_micros()
        ));
        let _ = std::fs::remove_file(&path);
        let rec = Recorder::open(&path).unwrap();
        rec.insert(&mk_opp()).unwrap();
        rec.insert(&mk_opp()).unwrap();
        assert_eq!(rec.count().unwrap(), 2);
        let pnl = rec.total_theoretical_pnl().unwrap();
        assert!((pnl - 9.4).abs() < 0.001);
        let s = rec.stats().unwrap();
        assert_eq!(s.count, 2);
        assert!((s.total_pnl - 9.4).abs() < 0.001);
        std::fs::remove_file(&path).ok();
    }
}
