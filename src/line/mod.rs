//! 回線 (TCP / モデム) の共通インターフェース
//!
//! どの回線もチャネルの組 `Conn` としてセッションに渡す。

pub mod modem;
pub mod serial;
pub mod tcp;
pub mod telnet;
pub mod ws;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

/// 回線から届くもの
#[derive(Debug)]
pub enum InEvent {
    Data(Vec<u8>),
    /// キャリア断 (モデム)
    CarrierLost,
    /// 接続が閉じた
    Closed,
}

/// 回線へ送るもの
#[derive(Debug)]
pub enum OutCmd {
    Write(Vec<u8>),
    /// 回線を切る
    Hangup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Tcp,
    Modem,
    /// ブラウザのソフトウェアモデム (null-modem) などからの WebSocket
    Ws,
}

impl LineKind {
    pub fn label(self) -> &'static str {
        match self {
            LineKind::Tcp => "TCP",
            LineKind::Modem => "MODEM",
            LineKind::Ws => "WS",
        }
    }
}

/// 回線の情報 (統計は各スレッドから更新する)
#[derive(Debug)]
pub struct LineInfo {
    pub no: u16,
    pub kind: LineKind,
    /// 接続元 (TCP のアドレス / シリアルポート)
    pub peer: Mutex<String>,
    /// 接続速度 (モデムの CONNECT の値)
    pub speed: Mutex<String>,
    pub rx_bytes: AtomicU64,
    pub tx_bytes: AtomicU64,
    /// 立っている間は未送信のデータを捨てる (切断を急ぐとき)
    pub abort: AtomicBool,
    /// ファイル転送中 (telnet の改行変換を止める)
    pub binary: AtomicBool,
}

impl LineInfo {
    pub fn new(no: u16, kind: LineKind, peer: &str) -> Arc<LineInfo> {
        Arc::new(LineInfo {
            no,
            kind,
            peer: Mutex::new(peer.to_string()),
            speed: Mutex::new(String::new()),
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            abort: AtomicBool::new(false),
            binary: AtomicBool::new(false),
        })
    }

    pub fn peer(&self) -> String {
        self.peer.lock().unwrap().clone()
    }

    pub fn speed(&self) -> String {
        self.speed.lock().unwrap().clone()
    }

    pub fn add_rx(&self, n: usize) {
        self.rx_bytes.fetch_add(n as u64, Ordering::Relaxed);
    }

    pub fn add_tx(&self, n: usize) {
        self.tx_bytes.fetch_add(n as u64, Ordering::Relaxed);
    }
}

pub struct Conn {
    pub rx: mpsc::Receiver<InEvent>,
    pub tx: mpsc::Sender<OutCmd>,
    pub info: Arc<LineInfo>,
}

/// 送受信チャネルの容量。遅い回線の送信待ちが背圧としてセッションに伝わる
pub const CHANNEL_CAP: usize = 64;

impl Conn {
    /// テストやアダプタ用: Conn と、回線側から使う反対の端を作る
    pub fn pair(info: Arc<LineInfo>) -> (Conn, mpsc::Sender<InEvent>, mpsc::Receiver<OutCmd>) {
        let (in_tx, in_rx) = mpsc::channel(CHANNEL_CAP);
        let (out_tx, out_rx) = mpsc::channel(CHANNEL_CAP);
        (Conn { rx: in_rx, tx: out_tx, info }, in_tx, out_rx)
    }
}
