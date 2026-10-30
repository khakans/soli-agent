//! Host-neutral menu data and concise output. Transport widgets stay in adapters.
use serde_json::{Value, json};
pub fn catalogue() -> Value {
    json!({"title":"Soli","groups":[
        {"label":"Status & wallet","items":[
            {"label":"Status","command":"status","write":false},
            {"label":"Wallet & posisi","command":"wallet","write":false},
            {"label":"Laporan","command":"report","write":false},
            {"label":"Learning","command":"learning","write":false}]},
        {"label":"Mode & pause","items":[
            {"label":"Semi · 1 USDC · 24 jam","command":"mode semi","write":true},
            {"label":"Full · 1 USDC · 24 jam","command":"mode full","write":true},
            {"label":"Manual","command":"mode manual","write":true},
            {"label":"Pause entry","command":"pause entries","write":true},
            {"label":"Pause semua otomatisasi","command":"pause all","write":true},
            {"label":"Lanjut entry","command":"resume-cycle entries","write":true},
            {"label":"Lanjut otomatisasi","command":"resume-cycle all","write":true}]},
        {"label":"Tutup posisi","items":[
            {"label":"Tutup semua posisi Soli","command":"close-all CONFIRM","write":true}]},
        {"label":"Pengaturan","items":[
            {"label":"Lihat pengaturan","command":"settings","write":false},
            {"label":"Batas entry strategi 1 USDC","command":"settings set entry 1USDC","write":true},
            {"label":"Batas entry strategi 5 USDC","command":"settings set entry 5USDC","write":true},
            {"label":"Stop-loss 15%","command":"settings set stop-loss 15","write":true},
            {"label":"Take-profit 30%","command":"settings set take-profit 30","write":true},
            {"label":"Maksimal hold 6 jam","command":"settings set max-hold-hours 6","write":true}]},
        {"label":"Command lengkap","items":[{"label":"Bantuan","command":"menu","write":false}]}
    ]})
}
fn number(v: &Value) -> Option<i128> {
    v.as_i64()
        .map(i128::from)
        .or_else(|| v.as_u64().map(i128::from))
        .or_else(|| v.as_str()?.parse().ok())
}
fn amount(v: &Value, decimals: u32) -> String {
    let Some(n) = number(v) else {
        return "belum tersedia".into();
    };
    let base = 10u128.pow(decimals);
    let magnitude = n.unsigned_abs();
    format!(
        "{}{}.{:0width$}",
        if n < 0 { "-" } else { "" },
        magnitude / base,
        magnitude % base,
        width = decimals as usize
    )
}
pub fn render(v: &Value) -> String {
    if let Some(e) = v.get("error") {
        return format!("Soli: {}", e.as_str().unwrap_or("permintaan ditolak"));
    }
    if v["strategy"].is_object() {
        let s = &v["strategy"];
        return format!(
            "Strategi: {}\nBatas entry: {} USDC\nStop-loss: {}% · take-profit: {}%\nMaksimal hold: {} jam\n{}",
            s["version"].as_str().unwrap_or("unknown"),
            amount(s.get("effective_entry_usdc").unwrap_or(&s["entry_usdc"]), 6),
            s["stop_loss_pct"],
            s["take_profit_pct"],
            s["max_hold_secs"].as_u64().unwrap_or(0) / 3600,
            if v["saved"] == true {
                if v["reload_scheduled"] == true {
                    "Tersimpan; engine memuat ulang, posisi/jurnal tetap tersimpan."
                } else {
                    "Tersimpan; restart engine untuk menerapkan."
                }
            } else {
                "Nominal entry juga mengikuti mandat mode dan batas risiko."
            }
        );
    }
    if let Some(wallet) = v["wallet"]
        .as_str()
        .filter(|_| v.get("balance_sol_lamports").is_some())
    {
        return format!(
            "Wallet: {wallet}\nSOL: {}\nAkun token berisi saldo: {}\nPosisi paper: {}\nPnL live: {} USDC\nPaper dan wallet asli dihitung terpisah.",
            amount(&v["balance_sol_lamports"], 9),
            v["token_accounts"].as_array().map_or(0, Vec::len),
            v["paper"]["positions"]
                .as_object()
                .map_or(0, serde_json::Map::len),
            amount(&v["live"]["pnl"]["realized_usdc_atomic"], 6)
        );
    }
    if v.get("ledger").is_some() {
        let s = &v["paper_lifecycle"];
        return format!(
            "Soli · {}\nMode: {} · pause: {} · entry dihentikan: {}\nEntry: {} USDC\nSaldo virtual: {} USDC\nPosisi paper: {}",
            v["mode"].as_str().unwrap_or("unknown"),
            s["mode"].as_str().unwrap_or("lihat live-status"),
            s["paused"],
            v["ledger"]["halted"],
            amount(s.get("effective_entry_usdc").unwrap_or(&s["entry_usdc"]), 6),
            amount(&v["ledger"]["cash_usdc"], 6),
            v["ledger"]["positions"]
                .as_object()
                .map_or(0, serde_json::Map::len)
        );
    }
    if v.get("evidence").is_some() {
        let e = &v["evidence"];
        return format!(
            "Learning · {}\nPutaran selesai: {} · menang: {}\nNet: {} USDC\nBukti lengkap: {}\nAuto-apply paper: {}",
            e["source"].as_str().unwrap_or("unknown"),
            e["closed"],
            e["wins"],
            amount(&e["net_usdc_atomic"], 6),
            e["complete"],
            v["automatic_apply_allowed"]
        );
    }
    if v.get("lifecycle").is_some() {
        let s = &v["lifecycle"];
        return format!(
            "Soli · paper\nMode: {} · pause: {}\nEntry: {} USDC\nBerlaku sampai epoch {}",
            s["mode"].as_str().unwrap_or("unknown"),
            s["paused"],
            amount(&s["entry_usdc"], 6),
            s["expires_at"]
        );
    }
    if v.get("paper_cash_usdc_atomic").is_some() {
        return format!(
            "Laporan Soli · {}\nSaldo virtual: {} USDC\nPnL paper hari ini: {} USDC\nFill hari ini: {}\nDrawdown: {}%",
            v["mode"].as_str().unwrap_or("unknown"),
            amount(&v["paper_cash_usdc_atomic"], 6),
            amount(&v["paper_realized_pnl_today_usdc_atomic"], 6),
            v["paper_fills_today"].as_array().map_or(0, Vec::len),
            v["peak_to_current_drawdown_pct"]
        );
    }
    serde_json::to_string_pretty(v).unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menu_and_output_preserve_units_and_missing_evidence() {
        assert_eq!(amount(&json!(-1000001), 6), "-1.000001");
        assert_eq!(amount(&Value::Null, 6), "belum tersedia");
        let menu = catalogue();
        for group in menu["groups"].as_array().unwrap() {
            for item in group["items"].as_array().unwrap() {
                assert!(item["command"].is_string());
                assert!(item["write"].is_boolean());
            }
        }
        assert!(
            render(&json!({"wallet":"public","balance_sol_lamports":1000000000}))
                .contains("1.000000000")
        );
    }
}
