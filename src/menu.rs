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
            {"label":"Tutup satu posisi Soli","command":"force-close {mint} CONFIRM","write":true,"input":"tracked-position"},
            {"label":"Tutup semua posisi Soli","command":"close-all CONFIRM","write":true}]},
        {"label":"Pengaturan","items":[
            {"label":"Lihat pengaturan","command":"settings","write":false},
            {"label":"Laporan setiap 30 menit","command":"settings set report-minutes 30","write":true},
            {"label":"Laporan setiap 1 jam","command":"settings set report-minutes 60","write":true},
            {"label":"Laporan setiap 2 jam","command":"settings set report-minutes 120","write":true},
            {"label":"Matikan laporan berkala","command":"settings set report-minutes off","write":true},
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
    let Some(base) = 10u128.checked_pow(decimals) else {
        return format!("{n}e-{decimals}");
    };
    let magnitude = n.unsigned_abs();
    format!(
        "{}{}.{:0width$}",
        if n < 0 { "-" } else { "" },
        magnitude / base,
        magnitude % base,
        width = decimals as usize
    )
}
fn money(v: &Value) -> String {
    let text = amount(v, 6);
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}
fn report_lines(r: &Value) -> String {
    format!(
        "Saldo simulasi: {} USDC\nPnL terealisasi hari ini: {} USDC\nTransaksi hari ini: {}\nPenurunan ekuitas: {}",
        money(&r["paper_cash_usdc_atomic"]),
        money(&r["paper_realized_pnl_today_usdc_atomic"]),
        r["paper_fills_today"].as_array().map_or(0, Vec::len),
        r["peak_to_current_drawdown_pct"]
            .as_f64()
            .filter(|n| n.is_finite())
            .map(|n| format!("{n:.2}%"))
            .unwrap_or_else(|| "belum tersedia".into())
    )
}
/// Only selected business fields enter chat. Provider responses and audit logs stay on disk.
pub fn notification(v: &Value, stamp: u64) -> String {
    let d = &v["data"];
    let time = chrono::DateTime::from_timestamp(v["timestamp"].as_i64().unwrap_or(0), 0)
        .map(|t| {
            t.with_timezone(&chrono::FixedOffset::east_opt(25200).unwrap())
                .format("%d-%m %H:%M WIB")
                .to_string()
        })
        .unwrap_or_else(|| "waktu belum tersedia".into());
    match v["kind"].as_str().unwrap_or("") {
        "lifecycle_report" if d["status"]["live_enabled"] == true => {
            let l = &d["report"]["live"];
            format!(
                "Soli · laporan {time}\n\nMode: trading nyata\nStatus signer: {}\nPosisi dikelola: {}\nGunakan !soli wallet untuk saldo dan PnL terverifikasi.",
                if l["error"].is_string() {
                    "belum dapat diverifikasi"
                } else if l["halted"] == true {
                    "entry dihentikan"
                } else {
                    "tersambung"
                },
                l["tracked_positions"]
                    .as_object()
                    .map(|p| p.len().to_string())
                    .unwrap_or_else(|| "belum tersedia".into())
            )
        }
        "lifecycle_report" => {
            let s = &d["status"];
            let cycle = &s["paper_lifecycle"];
            let mode = match cycle["mode"].as_str() {
                Some("full") => "otomatis penuh",
                Some("semi") => "entry perlu persetujuan",
                _ => "manual",
            };
            let state = if cycle["paused"] == true {
                "otomatisasi dijeda"
            } else if s["ledger"]["halted"] == true {
                "entry dihentikan"
            } else if cycle["expires_at"].as_u64().is_some_and(|t| t <= stamp)
                && cycle["mode"] != "manual"
            {
                "izin otomatis sudah berakhir"
            } else {
                "berjalan"
            };
            let fresh = d["latest_sample_at"]
                .as_u64()
                .is_some_and(|t| t <= stamp && stamp - t <= 300);
            format!(
                "Soli · laporan {time}\n\nMode: simulasi, {mode}\nStatus: {state}\nEntry per transaksi: {} USDC\nPosisi terbuka: {}\n{}\nData pasar: {}\n\nSimulasi, bukan uang asli. PnL harian mengikuti UTC.",
                money(
                    cycle
                        .get("effective_entry_usdc")
                        .unwrap_or(&cycle["entry_usdc"])
                ),
                s["ledger"]["positions"]
                    .as_object()
                    .map_or(0, serde_json::Map::len),
                report_lines(&d["report"]),
                if fresh {
                    "terbaru"
                } else {
                    "belum tersedia atau lebih dari 5 menit"
                }
            )
        }
        "daily_report" => format!(
            "Soli · laporan harian {} (UTC)\n\n{}\n\nAngka simulasi, bukan saldo wallet asli.",
            d["date_utc"].as_str().unwrap_or("tanggal belum tersedia"),
            report_lines(d)
        ),
        "paper_entry_filled" | "paper_exit_filled" => {
            let buy = d["side"] == "buy";
            format!(
                "Soli · {} simulasi selesai\n\nToken: {}\nNilai {}: {} USDC\nEstimasi biaya: {} USDC\n\nBukan transaksi uang asli.",
                if buy { "entry" } else { "exit" },
                d["mint"]
                    .as_str()
                    .filter(|m| soli_agent::valid_mint(m))
                    .unwrap_or("lihat detail posisi"),
                if buy { "pembelian" } else { "penjualan" },
                money(if buy {
                    &d["input_amount"]
                } else {
                    &d["output_amount"]
                }),
                money(&d["estimated_fee_usdc"])
            )
        }
        "live_receipt" => format!(
            "Soli · hasil transaksi nyata\nStatus: {}\nPerubahan USDC: {}\nGunakan !soli wallet untuk detail.",
            match d["state"].as_str() {
                Some("finalized") => "terkonfirmasi",
                Some("failed") => "gagal",
                _ => "belum dapat dipastikan",
            },
            money(&d["usdc_delta_atomic"])
        ),
        kind => {
            let title = match kind {
                "live_entry_candidate" => "Peluang entry ditemukan; perlu persetujuan owner.",
                "entry_signal" | "wallet_convergence_decision" => {
                    "Sinyal entry memenuhi filter; belum berarti transaksi sudah terjadi."
                }
                "exit_signal" | "live_exit_recommendation" => {
                    "Sinyal keluar terdeteksi; cek hasil eksekusinya di posisi."
                }
                "drawdown_halt" => "Batas penurunan ekuitas tercapai; entry dihentikan.",
                "tick_failed" | "data_unavailable" => {
                    "Pemeriksaan pasar terkendala; entry menunggu data yang valid."
                }
                "mark_failed" => {
                    "Valuasi posisi belum dapat diperbarui; PnL terbaru belum terverifikasi."
                }
                _ => "Ada pembaruan status; gunakan !soli status untuk detail.",
            };
            let mint = d["mint"]
                .as_str()
                .filter(|m| soli_agent::valid_mint(m))
                .map(|m| format!("\nToken: {m}"))
                .unwrap_or_default();
            format!("Soli · pemberitahuan {time}\n\n{title}{mint}")
        }
    }
}
pub fn render(v: &Value) -> String {
    if v["kind"].is_string() && v["data"].is_object() {
        return notification(v, crate::now());
    }
    if let Some(e) = v.get("error") {
        return format!("Soli: {}", e.as_str().unwrap_or("permintaan ditolak"));
    }
    let notification = v.get("notifications").map(|n| {
        if n["enabled"] == true {
            format!("Laporan lifecycle: setiap {} menit", n["minutes"])
        } else {
            "Laporan lifecycle berkala: nonaktif (notifikasi kejadian tetap aktif)".into()
        }
    });
    if v["strategy"].is_null()
        && let Some(text) = &notification
    {
        return format!("{text}\nTersimpan; langsung berlaku.");
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
                notification
                    .as_deref()
                    .unwrap_or("Nominal entry juga mengikuti mandat mode dan batas risiko.")
            }
        );
    }
    if let Some(wallet) = v["wallet"]
        .as_str()
        .filter(|_| v.get("balance_sol_lamports").is_some())
    {
        let mut text = format!(
            "Wallet: {wallet}\nSOL: {}\nAkun token berisi saldo: {}\nPosisi paper: {}\nPnL live: {} USDC\nPaper dan wallet asli dihitung terpisah.",
            amount(&v["balance_sol_lamports"], 9),
            v["token_accounts"].as_array().map_or(0, Vec::len),
            v["paper"]["positions"]
                .as_object()
                .map_or(0, serde_json::Map::len),
            amount(&v["live"]["pnl"]["realized_usdc_atomic"], 6)
        );
        if let Some(tokens) = v["token_accounts"].as_array() {
            for token in tokens.iter().take(20) {
                let mint = token["mint"]
                    .as_str()
                    .filter(|m| soli_agent::valid_mint(m))
                    .unwrap_or("unknown mint");
                let decimals = token["balance"]["decimals"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0);
                text.push_str(&format!(
                    "\n{mint}: {}",
                    amount(&token["balance"]["amount"], decimals)
                ));
            }
        }
        if let Some(positions) = v["paper"]["positions"].as_object() {
            for (mint, p) in positions {
                text.push_str(&format!(
                    "\nPaper {mint}: {} raw token · biaya {} USDC",
                    p["amount"],
                    amount(&p["cost_usdc"], 6)
                ));
            }
        }
        if let Some(positions) = v["live"]["marks"]["positions"].as_array() {
            for p in positions {
                text.push_str(&format!(
                    "\nLive {} · nilai {} USDC · unrealized {} USDC",
                    p["mint"].as_str().unwrap_or("unknown"),
                    amount(&p["liquidation_usdc_atomic"], 6),
                    amount(&p["unrealized_pnl_usdc_atomic"], 6)
                ));
            }
        }
        return text;
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
            "Laporan Soli · {}\nSaldo virtual: {} USDC\nPnL paper hari ini: {} USDC\nFill hari ini: {}\nDrawdown: {}",
            v["mode"].as_str().unwrap_or("unknown"),
            amount(&v["paper_cash_usdc_atomic"], 6),
            amount(&v["paper_realized_pnl_today_usdc_atomic"], 6),
            v["paper_fills_today"].as_array().map_or(0, Vec::len),
            v["peak_to_current_drawdown_pct"]
                .as_f64()
                .map(|n| format!("{n:.2}%"))
                .unwrap_or_else(|| "belum tersedia".into())
        );
    }
    serde_json::to_string_pretty(v).unwrap_or_default()
}
pub async fn positions() -> Result<Value, String> {
    let status = crate::mcp::api("/v1/status", None).await?;
    let (data, key) = if status["live_enabled"] == true {
        (
            crate::mcp::api("/v1/live/status", None).await?,
            "tracked_positions",
        )
    } else {
        (status["ledger"].clone(), "positions")
    };
    let rows = data[key]
        .as_object()
        .map(|positions| {
            positions
                .keys()
                .filter(|m| soli_agent::valid_mint(m))
                .map(|m| json!({"mint":m,"label":m,"command":format!("force-close {m} CONFIRM")}))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(json!({"positions":rows,"scope":"tracked positions only"}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notifications_never_dump_provider_or_audit_json_and_preserve_exact_values() {
        let report = json!({"date_utc":"2026-10-29","paper_cash_usdc_atomic":1000000000u64,"paper_realized_pnl_today_usdc_atomic":-1000001,"paper_fills_today":[],"peak_to_current_drawdown_pct":null,"events":[{"goplus":"RAW_PROVIDER_SENTINEL"}]});
        let daily = notification(
            &json!({"kind":"daily_report","timestamp":1000,"data":report}),
            1000,
        );
        assert!(daily.contains("1000 USDC") && daily.contains("-1.000001 USDC"));
        assert!(
            daily.contains("belum tersedia")
                && !daily.contains("RAW_PROVIDER_SENTINEL")
                && daily.len() < 800
        );
        let data = json!({"report":report,"latest_sample_at":1000,"status":{"ledger":{"positions":{},"halted":false},"paper_lifecycle":{"mode":"full","paused":false,"effective_entry_usdc":1000000,"expires_at":2000}}});
        let cycle = notification(
            &json!({"kind":"lifecycle_report","timestamp":1000,"data":data}),
            1000,
        );
        assert!(
            cycle.contains("otomatis penuh")
                && cycle.contains("Status: berjalan")
                && cycle.contains("Data pasar: terbaru")
        );
        assert_eq!(cycle.matches("Saldo simulasi:").count(), 1);
        assert!(!cycle.contains("false") && !cycle.contains("dry_run") && cycle.len() < 900);
        let fault = notification(
            &json!({"kind":"tick_failed","timestamp":1000,"data":{"reason":"RAW_PROVIDER_SENTINEL"}}),
            1000,
        );
        assert!(!fault.contains("RAW_PROVIDER_SENTINEL"));
        let unknown = notification(
            &json!({"kind":"future_event","data":{"receipt":"RAW_PROVIDER_SENTINEL"}}),
            1000,
        );
        assert!(!unknown.contains("RAW_PROVIDER_SENTINEL"));
    }
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
