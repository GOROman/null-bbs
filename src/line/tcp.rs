//! TCP (telnet) 回線

use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::telnet::Telnet;
use super::{Conn, InEvent, LineInfo, LineKind, OutCmd};
use crate::session::{self, Ctx};

pub async fn listen(addr: String, ctx: Arc<Ctx>, stop: CancellationToken) -> Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("TCP {} で待ち受けを開始", listener.local_addr()?);
    serve(listener, ctx, stop).await
}

pub async fn serve(listener: TcpListener, ctx: Arc<Ctx>, stop: CancellationToken) -> Result<()> {
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => r?,
            _ = stop.cancelled() => return Ok(()),
        };
        let _ = stream.set_nodelay(true);
        let Some(no) = ctx.hub.alloc(LineKind::Tcp) else {
            tracing::warn!("{peer}: 回線が満杯のため接続を断りました");
            tokio::spawn(async move {
                let mut s = stream;
                let _ = s.write_all("\r\nただいま回線が混み合っています。しばらくしてからおかけ直しください。\r\n".as_bytes()).await;
                let _ = s.shutdown().await;
            });
            continue;
        };
        tracing::info!("CH{no:02}: TCP 接続 {peer}");
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let info = LineInfo::new(no, LineKind::Tcp, &peer.to_string());
            let mut conn = adapt(stream, info);
            session::run(&mut conn, &ctx).await;
            let _ = conn.tx.send(OutCmd::Hangup).await;
            ctx.hub.free_tcp(no);
            tracing::info!("CH{no:02}: TCP 切断 {peer}");
        });
    }
}

/// TcpStream を Conn に変換する (読み書きのタスクを立てる)
fn adapt(stream: TcpStream, info: Arc<LineInfo>) -> Conn {
    let (conn, in_tx, mut out_rx) = Conn::pair(info.clone());
    let (mut rd, mut wr) = stream.into_split();
    // telnet の応答は読み取り側から書き込み側へ回す
    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let rinfo = info.clone();
    tokio::spawn(async move {
        let mut telnet = Telnet::new();
        let mut buf = [0u8; 4096];
        loop {
            match rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    rinfo.add_rx(n);
                    telnet.binary = rinfo.binary.load(Ordering::Relaxed);
                    let (data, replies) = telnet.feed(&buf[..n]);
                    if !replies.is_empty() {
                        let _ = reply_tx.send(replies);
                    }
                    if !data.is_empty() && in_tx.send(InEvent::Data(data)).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = in_tx.send(InEvent::Closed).await;
    });

    tokio::spawn(async move {
        if wr.write_all(&Telnet::greeting()).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                cmd = out_rx.recv() => match cmd {
                    Some(OutCmd::Write(data)) => {
                        if info.abort.load(Ordering::Relaxed) {
                            continue;
                        }
                        let data = Telnet::escape(&data);
                        info.add_tx(data.len());
                        if wr.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Some(OutCmd::Hangup) | None => break,
                },
                Some(r) = reply_rx.recv() => {
                    if wr.write_all(&r).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = wr.shutdown().await;
    });
    conn
}
