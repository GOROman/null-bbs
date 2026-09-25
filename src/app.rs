//! 起動と終了処理

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::db::Db;
use crate::hub::Hub;
use crate::line::{modem, tcp};
use crate::session::Ctx;
use crate::{logbuf, tui};

pub async fn run(cfg: Config, headless: bool) -> Result<()> {
    logbuf::init(&cfg.bbs.log_file, headless)?;
    let db = Db::open(&cfg.bbs.db)?;
    std::fs::create_dir_all(&cfg.bbs.files_dir)?;
    let modems: Vec<(u16, String)> = cfg.modem.iter().map(|m| (m.line, m.path.clone())).collect();
    let hub = Hub::new(&cfg.bbs.name, cfg.bbs.max_lines, &modems);
    let ctx = Arc::new(Ctx { hub: hub.clone(), db, cfg: Arc::new(cfg) });
    let stop = CancellationToken::new();
    tracing::info!("{} を起動しました (最大 {} 回線)", ctx.cfg.bbs.name, ctx.cfg.bbs.max_lines);

    for addr in ctx.cfg.tcp.listen.clone() {
        let (ctx, stop) = (ctx.clone(), stop.clone());
        tokio::spawn(async move {
            if let Err(e) = tcp::listen(addr.clone(), ctx, stop.clone()).await {
                tracing::error!("TCP {addr} で待ち受けできません: {e:#}");
            }
        });
    }
    for m in ctx.cfg.modem.clone() {
        tokio::spawn(modem::run(m, ctx.clone(), stop.clone()));
    }

    let console = if headless {
        None
    } else {
        let (ctx, stop) = (ctx.clone(), stop.clone());
        Some(tokio::task::spawn_blocking(move || {
            let r = tui::run(ctx, stop.clone());
            stop.cancel();
            r
        }))
    };

    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT を受けました"),
        _ = stop.cancelled() => {}
        _ = sigterm() => tracing::info!("SIGTERM を受けました"),
    }
    stop.cancel();
    shutdown(&hub).await;
    if let Some(c) = console {
        c.await??;
    }
    Ok(())
}

#[cfg(unix)]
async fn sigterm() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn sigterm() {
    std::future::pending().await
}

/// 利用中の全員に知らせてから切断する
async fn shutdown(hub: &Hub) {
    let online = hub.who();
    if !online.is_empty() {
        hub.broadcast("まもなくシステムを終了します");
        tokio::time::sleep(Duration::from_secs(2)).await;
        for v in &online {
            hub.kick(v.no, "システムを終了しました");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    tracing::info!("終了しました");
}
