//! WebSocket 回線 (ブラウザのソフトウェアモデム null-modem などから)
//!
//! データはバイナリフレームでそのままやりとりする (telnet の処理はしない)。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use super::{Conn, InEvent, LineInfo, LineKind, OutCmd};
use crate::session::{self, Ctx};

pub async fn listen(addr: String, ctx: Arc<Ctx>, stop: CancellationToken) -> Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("WebSocket {} で待ち受けを開始", listener.local_addr()?);
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => r?,
            _ = stop.cancelled() => return Ok(()),
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(e) = serve(stream, peer.to_string(), ctx).await {
                tracing::warn!("WebSocket {peer}: {e:#}");
            }
        });
    }
}

async fn serve(stream: TcpStream, mut peer: String, ctx: Arc<Ctx>) -> Result<()> {
    let _ = stream.set_nodelay(true);
    // Cloudflare Tunnel 経由なら本当の接続元はヘッダにある
    let mut forwarded = None;
    let ws = tokio_tungstenite::accept_hdr_async(stream, |req: &Request, resp: Response| {
        forwarded = req
            .headers()
            .get("cf-connecting-ip")
            .or_else(|| req.headers().get("x-forwarded-for"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(',').next().unwrap_or(s).trim().to_string());
        Ok(resp)
    })
    .await?;
    if let Some(f) = forwarded {
        peer = format!("{f} (WS)");
    }
    let (mut sink, mut stream) = ws.split();
    let Some(no) = ctx.hub.alloc(LineKind::Ws) else {
        tracing::warn!("{peer}: 回線が満杯のため接続を断りました");
        let msg = "\r\nただいま回線が混み合っています。しばらくしてからおかけ直しください。\r\n";
        let _ = sink.send(Message::binary(msg.as_bytes().to_vec())).await;
        let _ = sink.close().await;
        return Ok(());
    };
    tracing::info!("CH{no:02}: WebSocket 接続 {peer}");
    let info = LineInfo::new(no, LineKind::Ws, &peer);
    let (mut conn, in_tx, mut out_rx) = Conn::pair(info.clone());

    let rinfo = info.clone();
    let reader = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            let data = match msg {
                Ok(Message::Binary(b)) => b.to_vec(),
                Ok(Message::Text(t)) => t.as_bytes().to_vec(),
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue, // ping / pong
            };
            rinfo.add_rx(data.len());
            if in_tx.send(InEvent::Data(data)).await.is_err() {
                break;
            }
        }
        let _ = in_tx.send(InEvent::Closed).await;
    });

    let winfo = info.clone();
    let writer = tokio::spawn(async move {
        while let Some(cmd) = out_rx.recv().await {
            match cmd {
                OutCmd::Write(data) => {
                    if winfo.abort.load(Ordering::Relaxed) {
                        continue;
                    }
                    winfo.add_tx(data.len());
                    if sink.send(Message::binary(data)).await.is_err() {
                        break;
                    }
                }
                OutCmd::Hangup => break,
            }
        }
        let _ = sink.close().await;
    });

    session::run(&mut conn, &ctx).await;
    let _ = conn.tx.send(OutCmd::Hangup).await;
    drop(conn);
    let _ = writer.await;
    reader.abort();
    ctx.hub.free_tcp(no);
    tracing::info!("CH{no:02}: WebSocket 切断 {peer}");
    Ok(())
}
