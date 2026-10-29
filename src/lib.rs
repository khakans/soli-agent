use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub const QUOTE_TTL: u64 = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub amount: u64,
    pub cost_usdc: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub id: String,
    pub quote_id: String,
    pub side: String,
    pub mint: String,
    pub input_amount: u64,
    pub output_amount: u64,
    pub estimated_fee_usdc: u64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ledger {
    pub initial_usdc: u64,
    pub cash_usdc: u64,
    pub positions: BTreeMap<String, Position>,
    pub fills: Vec<Fill>,
    pub halted: bool,
    pub day: u64,
    pub daily_realized_pnl_usdc: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    pub id: String,
    pub side: String,
    pub mint: String,
    pub input_amount: u64,
    pub quoted_output: u64,
    pub minimum_output: u64,
    pub estimated_fee_usdc: u64,
    pub created_at: u64,
    pub price_impact_pct: f64,
}

pub fn valid_mint(mint: &str) -> bool {
    bs58::decode(mint).into_vec().is_ok_and(|v| v.len() == 32)
}

impl Ledger {
    pub fn new(initial_usdc: u64) -> Result<Self, String> {
        if !(1_000_000..=1_000_000_000_000).contains(&initial_usdc) {
            return Err("virtual capital must be between 1 and 1,000,000 USDC".into());
        }
        Ok(Self {
            initial_usdc,
            cash_usdc: initial_usdc,
            positions: BTreeMap::new(),
            fills: vec![],
            halted: false,
            day: 0,
            daily_realized_pnl_usdc: 0,
        })
    }

    pub fn fill(&mut self, p: &Preview, now: u64) -> Result<Fill, String> {
        // Work on a copy: a rejected fill must never partially mutate the ledger.
        let mut next = self.clone();
        let fill = next.apply(p, now)?;
        *self = next;
        Ok(fill)
    }

    fn apply(&mut self, p: &Preview, now: u64) -> Result<Fill, String> {
        if self.fills.iter().any(|f| f.quote_id == p.id) {
            return Err("quote already filled".into());
        }
        if now < p.created_at || now - p.created_at > QUOTE_TTL {
            return Err("quote expired; request a new preview".into());
        }
        if !valid_mint(&p.mint)
            || p.mint == USDC
            || p.input_amount == 0
            || p.minimum_output == 0
            || p.minimum_output > p.quoted_output
            || !p.price_impact_pct.is_finite()
            || p.price_impact_pct.abs() > 1.0
        {
            return Err("invalid quote or price impact above 1%".into());
        }
        // Daily loss counter uses UTC, independent of the host timezone.
        if self.day != now / 86400 {
            self.day = now / 86400;
            self.daily_realized_pnl_usdc = 0;
        }
        match p.side.as_str() {
            "buy" => {
                if self.halted {
                    return Err("new entries are halted".into());
                }
                if self.daily_realized_pnl_usdc <= -(i128::from(self.initial_usdc) * 3 / 100) {
                    return Err("daily realized loss limit reached".into());
                }
                if self.positions.contains_key(&p.mint) {
                    return Err("averaging down/additional buys disabled".into());
                }
                if self.positions.len() >= 2 {
                    return Err("maximum two positions".into());
                }
                let debit = p
                    .input_amount
                    .checked_add(p.estimated_fee_usdc)
                    .ok_or("amount overflow")?;
                if debit > self.initial_usdc / 20 {
                    return Err(
                        "position including estimated fees exceeds 5% of initial capital".into(),
                    );
                }
                let exposure: u64 = self.positions.values().map(|v| v.cost_usdc).sum();
                if exposure + debit > self.initial_usdc / 10 {
                    return Err("exposure exceeds 10%".into());
                }
                self.cash_usdc = self
                    .cash_usdc
                    .checked_sub(debit)
                    .ok_or("insufficient virtual USDC")?;
                self.positions.insert(
                    p.mint.clone(),
                    Position {
                        amount: p.minimum_output,
                        cost_usdc: debit,
                    },
                );
                self.daily_realized_pnl_usdc -= i128::from(p.estimated_fee_usdc);
            }
            "sell" => {
                let position = self.positions.get(&p.mint).ok_or("position not found")?;
                if p.input_amount != position.amount {
                    return Err("v1 only supports full-position exits".into());
                }
                let credit = p
                    .minimum_output
                    .checked_sub(p.estimated_fee_usdc)
                    .ok_or("estimated fee exceeds sale proceeds")?;
                self.cash_usdc = self
                    .cash_usdc
                    .checked_add(credit)
                    .ok_or("balance overflow")?;
                // Entry fee was already realized on purchase; do not count it twice.
                self.daily_realized_pnl_usdc +=
                    i128::from(credit) - i128::from(position.cost_usdc - self.entry_fee(&p.mint));
                self.positions.remove(&p.mint);
            }
            _ => return Err("side must be buy or sell".into()),
        }
        let fill = Fill {
            id: uuid::Uuid::new_v4().to_string(),
            quote_id: p.id.clone(),
            side: p.side.clone(),
            mint: p.mint.clone(),
            input_amount: p.input_amount,
            output_amount: p.minimum_output,
            estimated_fee_usdc: p.estimated_fee_usdc,
            timestamp: now,
        };
        self.fills.push(fill.clone());
        Ok(fill)
    }

    fn entry_fee(&self, mint: &str) -> u64 {
        self.fills
            .iter()
            .rev()
            .find(|f| f.mint == mint && f.side == "buy")
            .map_or(0, |f| f.estimated_fee_usdc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn quote(side: &str, amount: u64, out: u64) -> Preview {
        Preview {
            id: uuid::Uuid::new_v4().to_string(),
            side: side.into(),
            mint: "So11111111111111111111111111111111111111112".into(),
            input_amount: amount,
            quoted_output: out,
            minimum_output: out,
            estimated_fee_usdc: 100_000,
            created_at: 100_000,
            price_impact_pct: 0.1,
        }
    }
    #[test]
    fn round_trip_conserves_cash_and_realizes_fees_once() {
        let mut l = Ledger::new(1_000_000_000).unwrap();
        let buy = quote("buy", 10_000_000, 500);
        l.fill(&buy, 100_001).unwrap();
        assert_eq!(l.cash_usdc, 989_900_000);
        assert!(l.fill(&buy, 100_002).is_err());
        l.halted = true;
        l.fill(&quote("sell", 500, 11_000_000), 100_003).unwrap();
        assert_eq!(l.cash_usdc, 1_000_800_000);
        assert_eq!(l.daily_realized_pnl_usdc, 800_000);
        assert!(l.positions.is_empty());
    }
    #[test]
    fn rejects_risk_and_stale_inputs_without_mutation() {
        let mut l = Ledger::new(1_000_000_000).unwrap();
        let before = serde_json::to_string(&l).unwrap();
        assert!(l.fill(&quote("buy", 50_000_000, 500), 100_001).is_err());
        assert!(l.fill(&quote("buy", 10_000_000, 500), 100_021).is_err());
        let mut p = quote("buy", 10_000_000, 500);
        p.price_impact_pct = f64::NAN;
        assert!(l.fill(&p, 100_001).is_err());
        assert_eq!(serde_json::to_string(&l).unwrap(), before);
        l.daily_realized_pnl_usdc = -30_000_000;
        l.day = 1;
        assert!(l.fill(&quote("buy", 10_000_000, 500), 100_001).is_err());
    }
}
