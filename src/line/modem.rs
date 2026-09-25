//! モデム回線: 初期化 → 着信待ち → 応答 → セッション → 切断 → 再初期化 を繰り返す

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use tokio::time::{timeout, Instant};
use tokio_util::sync::CancellationToken;

use super::serial::{self, CarrierWatch};
use super::{Conn, InEvent, LineInfo, LineKind, OutCmd};
use crate::config::ModemConfig;
use crate::hub::LineState;
use crate::session::{self, Ctx};

/// モデムの応答コード
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModemResult {
    Ok,
    Error,
    Ring,
    /// CONNECT の後ろ (「2400/V42BIS」など。無ければ空)
    Connect(String),
    NoCarrier,
    Busy,
    NoDialtone,
    NoAnswer,
    Other(String),
}

pub fn parse_result(line: &str) -> ModemResult {
    let l = line.trim();
    let u = l.to_ascii_uppercase();
    match u.as_str() {
        "OK" | "0" => ModemResult::Ok,
        "ERROR" | "4" => ModemResult::Error,
        "RING" | "2" => ModemResult::Ring,
        "NO CARRIER" | "3" => ModemResult::NoCarrier,
        "BUSY" | "7" => ModemResult::Busy,
        "NO DIALTONE" | "NO DIAL TONE" | "6" => ModemResult::NoDialtone,
        "NO ANSWER" | "8" => ModemResult::NoAnswer,
        "CONNECT" | "1" => ModemResult::Connect(String::new()),
        _ if u.starts_with("CONNECT") => ModemResult::Connect(l[7..].trim().to_string()),
        _ => ModemResult::Other(l.to_string()),
    }
}

/// 受信データを行ごとに取り出す
struct Lines {
    buf: String,
}

impl Lines {
    /// 1 行読む。`wait` の間に行が来なければ None
    async fn next(&mut self, conn: &mut Conn, wait: Duration) -> Result<Option<String>> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(i) = self.buf.find(['\r', '\n']) {
                let line = self.buf[..i].trim().to_string();
                self.buf.drain(..=i);
                if !line.is_empty() {
                    return Ok(Some(line));
                }
                continue;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            match timeout(left, conn.rx.recv()).await {
                Err(_) => return Ok(None),
                Ok(Some(InEvent::Data(d))) => {
                    self.buf.push_str(&String::from_utf8_lossy(&d));
                    if self.buf.len() > 4096 {
                        self.buf.clear();
                    }
                }
                Ok(Some(InEvent::CarrierLost)) => {}
                Ok(Some(InEvent::Closed)) | Ok(None) => bail!("ポートが閉じられました"),
            }
        }
    }
}

async fn send(conn: &Conn, s: &str) -> Result<()> {
    conn.tx.send(OutCmd::Write(s.as_bytes().to_vec())).await.map_err(|_| anyhow::anyhow!("ポートが閉じられました"))
}

/// 1 つのモデム回線を受け持つ
pub async fn run(cfg: ModemConfig, ctx: Arc<Ctx>, stop: CancellationToken) {
    let no = cfg.line;
    while !stop.is_cancelled() {
        let info = LineInfo::new(no, LineKind::Modem, &cfg.path);
        let (mut conn, watch) = match serial::open(&cfg, info.clone()) {
            Ok(x) => x,
            Err(e) => {
                ctx.hub.set_state(no, LineState::Error);
                ctx.hub.set_note(no, "ポートを開けません");
                tracing::error!("CH{no:02}: {e:#}");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(10)) => continue,
                    _ = stop.cancelled() => return,
                }
            }
        };
        tracing::info!("CH{no:02}: {} を開きました ({} bps)", cfg.path, cfg.baud);
        let r = tokio::select! {
            r = line_loop(&cfg, &ctx, &mut conn, &watch, &info) => r,
            _ = stop.cancelled() => Ok(()),
        };
        if let Err(e) = r {
            ctx.hub.set_state(no, LineState::Error);
            ctx.hub.set_note(no, &format!("{e:#}"));
            tracing::error!("CH{no:02}: {e:#}");
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                _ = stop.cancelled() => return,
            }
        }
    }
}

async fn line_loop(cfg: &ModemConfig, ctx: &Ctx, conn: &mut Conn, watch: &CarrierWatch, info: &Arc<LineInfo>) -> Result<()> {
    let no = cfg.line;
    let mut lines = Lines { buf: String::new() };
    loop {
        ctx.hub.set_state(no, LineState::Init);
        ctx.hub.set_note(no, "");
        if let Err(e) = init_modem(cfg, conn, &mut lines).await {
            ctx.hub.set_state(no, LineState::Error);
            ctx.hub.set_note(no, &format!("{e:#}"));
            tracing::warn!("CH{no:02}: 初期化に失敗: {e:#}");
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        }
        ctx.hub.set_state(no, LineState::Idle);
        let Some(speed) = wait_call(cfg, ctx, conn, &mut lines).await? else { continue };
        *info.speed.lock().unwrap() = if speed.is_empty() { "CONNECT".into() } else { speed.clone() };
        tracing::info!("CH{no:02}: 着信 CONNECT {speed}");
        watch.online.store(true, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(cfg.connect_delay_ms)).await;
        // 接続直後の雑音などを読み捨てる
        while conn.rx.try_recv().is_ok() {}
        session::run(conn, ctx).await;
        watch.online.store(false, Ordering::Relaxed);
        ctx.hub.set_state(no, LineState::Hangup);
        let _ = conn.tx.send(OutCmd::Hangup).await;
        // 切断処理 (DTR や +++ATH0) が終わるのを待って残りを読み捨てる
        tokio::time::sleep(Duration::from_secs(if cfg.hangup == "escape" { 4 } else { 2 })).await;
        while conn.rx.try_recv().is_ok() {}
        lines.buf.clear();
        info.speed.lock().unwrap().clear();
    }
}

/// 初期化コマンドを 1 つずつ送り OK を待つ
async fn init_modem(cfg: &ModemConfig, conn: &mut Conn, lines: &mut Lines) -> Result<()> {
    send(conn, "\r").await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    while conn.rx.try_recv().is_ok() {}
    lines.buf.clear();
    for cmd in &cfg.init {
        let mut ok = false;
        for _attempt in 0..3 {
            send(conn, &format!("{cmd}\r")).await?;
            let deadline = Instant::now() + Duration::from_secs(3);
            while let Some(line) = lines.next(conn, deadline.saturating_duration_since(Instant::now())).await? {
                match parse_result(&line) {
                    ModemResult::Ok => {
                        ok = true;
                        break;
                    }
                    ModemResult::Error => bail!("{cmd} が ERROR になりました"),
                    _ => {} // エコーなど
                }
            }
            if ok {
                break;
            }
        }
        if !ok {
            bail!("モデムが応答しません ({cmd})");
        }
        // ATZ の後はモデムが落ち着くまで少し待つ
        if cmd.to_ascii_uppercase().starts_with("ATZ") {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(())
}

/// 着信を待って応答する。CONNECT したら速度を返す。失敗して再初期化するなら None
async fn wait_call(cfg: &ModemConfig, ctx: &Ctx, conn: &mut Conn, lines: &mut Lines) -> Result<Option<String>> {
    let no = cfg.line;
    let manual = cfg.answer != "auto";
    let mut rings = 0u32;
    let mut last_ring: Option<Instant> = None;
    let mut answered_at: Option<Instant> = None;
    loop {
        let line = lines.next(conn, Duration::from_secs(1)).await?;
        if let Some(t) = answered_at {
            if t.elapsed() > Duration::from_secs(cfg.connect_timeout_secs) {
                tracing::info!("CH{no:02}: 接続できませんでした (タイムアウト)");
                send(conn, "\r").await?; // 応答中の処理を中止させる
                return Ok(None);
            }
        } else if last_ring.is_some_and(|t| t.elapsed() > Duration::from_secs(10)) {
            // 呼び出しが止んだ
            rings = 0;
            last_ring = None;
            ctx.hub.set_state(no, LineState::Idle);
        }
        let Some(line) = line else { continue };
        match parse_result(&line) {
            ModemResult::Ring => {
                rings += 1;
                last_ring = Some(Instant::now());
                ctx.hub.set_state(no, LineState::Ringing);
                ctx.hub.set_note(no, &format!("RING x{rings}"));
                if manual && answered_at.is_none() && rings >= cfg.rings.max(1) {
                    send(conn, "ATA\r").await?;
                    answered_at = Some(Instant::now());
                    ctx.hub.set_state(no, LineState::Connecting);
                } else if !manual && answered_at.is_none() {
                    answered_at = Some(Instant::now());
                }
            }
            ModemResult::Connect(speed) => {
                ctx.hub.set_note(no, "");
                return Ok(Some(speed));
            }
            ModemResult::NoCarrier | ModemResult::Busy | ModemResult::NoAnswer | ModemResult::NoDialtone | ModemResult::Error
                if answered_at.is_some() =>
            {
                tracing::info!("CH{no:02}: 接続できませんでした ({line})");
                return Ok(None);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_codes() {
        assert_eq!(parse_result("CONNECT 2400/V42BIS"), ModemResult::Connect("2400/V42BIS".into()));
        assert_eq!(parse_result("CONNECT"), ModemResult::Connect(String::new()));
        assert_eq!(parse_result("  RING "), ModemResult::Ring);
        assert_eq!(parse_result("NO CARRIER"), ModemResult::NoCarrier);
        assert_eq!(parse_result("ok"), ModemResult::Ok);
        assert_eq!(parse_result("ATZ"), ModemResult::Other("ATZ".into()));
    }

    #[tokio::test]
    async fn line_splitting() {
        let (mut conn, tx, _out) = Conn::pair(LineInfo::new(1, LineKind::Modem, "x"));
        tx.send(InEvent::Data(b"\r\nRI".to_vec())).await.unwrap();
        tx.send(InEvent::Data(b"NG\r\n\r\nCONNECT 2400\r".to_vec())).await.unwrap();
        let mut l = Lines { buf: String::new() };
        let w = Duration::from_millis(100);
        assert_eq!(l.next(&mut conn, w).await.unwrap().as_deref(), Some("RING"));
        assert_eq!(l.next(&mut conn, w).await.unwrap().as_deref(), Some("CONNECT 2400"));
        assert_eq!(l.next(&mut conn, w).await.unwrap(), None);
    }
}
