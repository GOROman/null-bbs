//! 利用者の端末とのやりとり: 表示、1 行入力、割り込み表示、タイムアウト

pub mod editor;

use std::collections::VecDeque;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio::time::{sleep_until, Instant};

use crate::hub::Notice;
use crate::line::{Conn, InEvent, OutCmd};
use crate::transfer::{Outcome, Transfer};
pub use editor::EditOpts;
use editor::Editor;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("回線が切れました")]
    Disconnected,
    #[error("無操作のため切断しました")]
    IdleTimeout,
    #[error("接続時間の上限になりました")]
    TimeUp,
    #[error("{0}")]
    Kicked(String),
    /// 利用者が終了を選んだ
    #[error("ログオフ")]
    Logoff,
    #[error("{0:#}")]
    Other(#[from] anyhow::Error),
}

impl SessionError {
    /// access_log に残す理由
    pub fn reason(&self) -> &'static str {
        match self {
            SessionError::Disconnected => "回線断",
            SessionError::IdleTimeout => "無操作",
            SessionError::TimeUp => "時間切れ",
            SessionError::Kicked(_) => "強制切断",
            SessionError::Logoff => "ログオフ",
            SessionError::Other(_) => "エラー",
        }
    }
}

pub type SResult<T> = Result<T, SessionError>;

pub struct Term<'a> {
    conn: &'a mut Conn,
    notices: mpsc::Receiver<Notice>,
    monitor: broadcast::Sender<Vec<u8>>,
    /// 先行入力 (確定した行の後ろに続いて届いたバイト)
    pending: VecDeque<u8>,
    pub idle: Duration,
    /// 接続時間の上限
    pub deadline: Option<Instant>,
    /// 1 ページの行数
    pub rows: usize,
    last_input: Instant,
}

enum Wake {
    Input(Option<InEvent>),
    Notice(Notice),
    Warn,
    Idle,
    TimeUp,
}

/// 改行を CRLF にそろえる
pub fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

impl<'a> Term<'a> {
    pub fn new(conn: &'a mut Conn, notices: mpsc::Receiver<Notice>, monitor: broadcast::Sender<Vec<u8>>, idle: Duration) -> Self {
        Term {
            conn,
            notices,
            monitor,
            pending: VecDeque::new(),
            idle,
            deadline: None,
            rows: 24,
            last_input: Instant::now(),
        }
    }

    pub fn line_no(&self) -> u16 {
        self.conn.info.no
    }

    pub fn info(&self) -> &crate::line::LineInfo {
        &self.conn.info
    }

    pub async fn write(&mut self, bytes: Vec<u8>) -> SResult<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let _ = self.monitor.send(bytes.clone());
        self.conn.tx.send(OutCmd::Write(bytes)).await.map_err(|_| SessionError::Disconnected)
    }

    pub async fn print(&mut self, s: &str) -> SResult<()> {
        self.write(crlf(s).into_bytes()).await
    }

    pub async fn println(&mut self, s: &str) -> SResult<()> {
        self.print(&format!("{s}\n")).await
    }

    fn format_notice(n: &Notice) -> String {
        match n {
            Notice::System(t) => format!("*** SYSOP より: {t} ***"),
            Notice::Telegram { from, text } => format!("*** 電報 {from}: {text} ***"),
            Notice::Chat { text, .. } => text.clone(),
            Notice::Kick(r) => format!("*** {r} ***"),
        }
    }

    /// 1 行入力。待っている間に届いた通知は割り込んで表示する
    pub async fn read_line(&mut self, prompt: &str, opts: EditOpts) -> SResult<String> {
        self.print(prompt).await?;
        let mut ed = Editor::new(opts);
        let mut warned = false;
        loop {
            while let Some(b) = self.pending.pop_front() {
                let mut echo = Vec::new();
                let line = ed.feed(b, &mut echo);
                self.write(echo).await?;
                if let Some(line) = line {
                    return Ok(line);
                }
            }
            let idle_at = self.last_input + self.idle;
            let warn_at = idle_at.checked_sub(Duration::from_secs(60)).filter(|_| self.idle >= Duration::from_secs(120));
            let deadline = self.deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(86400 * 365));
            let wake = tokio::select! {
                ev = self.conn.rx.recv() => Wake::Input(ev),
                Some(n) = self.notices.recv() => Wake::Notice(n),
                _ = sleep_until(warn_at.unwrap_or(idle_at)), if !warned && warn_at.is_some() => Wake::Warn,
                _ = sleep_until(idle_at) => Wake::Idle,
                _ = sleep_until(deadline) => Wake::TimeUp,
            };
            match wake {
                Wake::Input(Some(InEvent::Data(d))) => {
                    self.last_input = Instant::now();
                    warned = false;
                    self.pending.extend(d);
                }
                Wake::Input(_) => return Err(SessionError::Disconnected),
                Wake::Notice(Notice::Kick(r)) => return Err(SessionError::Kicked(r)),
                Wake::Notice(n) => {
                    let text = format!("\n{}\n{prompt}{}", Self::format_notice(&n), ed.display());
                    self.print(&text).await?;
                }
                Wake::Warn => {
                    warned = true;
                    self.print(&format!("\n*** 無操作のため、あと 1 分で切断します ***\n{prompt}{}", ed.display())).await?;
                }
                Wake::Idle => return Err(SessionError::IdleTimeout),
                Wake::TimeUp => return Err(SessionError::TimeUp),
            }
        }
    }

    /// コマンド入力 (前後の空白を除く、全角英数字は半角に)
    pub async fn input(&mut self, prompt: &str) -> SResult<String> {
        Ok(self.read_line(prompt, EditOpts::default()).await?.trim().to_string())
    }

    /// 文章の入力 (全角をそのまま残す)
    pub async fn input_text(&mut self, prompt: &str, max_chars: usize) -> SResult<String> {
        let opts = EditOpts { max_chars, mask: false, normalize: false };
        Ok(self.read_line(prompt, opts).await?.trim_end().to_string())
    }

    pub async fn password(&mut self, prompt: &str) -> SResult<String> {
        self.read_line(prompt, EditOpts { max_chars: 64, mask: true, normalize: false }).await
    }

    pub async fn yes_no(&mut self, prompt: &str) -> SResult<bool> {
        let a = self.input(&format!("{prompt} (Y/N)? ")).await?;
        Ok(a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes"))
    }

    /// X/YMODEM 転送を最後まで進める。`first` は最初に送るバイト列 (受信側の開始合図)。
    /// `limit` を超えて受信したら中止する。転送中に届いた通知は終わってから表示する
    pub async fn transfer(&mut self, t: &mut Transfer, first: Vec<u8>, limit: Option<u64>) -> SResult<Outcome> {
        use std::sync::atomic::Ordering;
        self.pending.clear();
        self.conn.info.binary.store(true, Ordering::Relaxed);
        let result = self.transfer_loop(t, first, limit).await;
        // 相手の送り残し (CAN や改行) を読み捨ててから通常の入力に戻す
        let quiet = tokio::time::sleep(Duration::from_millis(800));
        tokio::pin!(quiet);
        loop {
            tokio::select! {
                ev = self.conn.rx.recv() => if !matches!(ev, Some(InEvent::Data(_))) { break },
                _ = &mut quiet => break,
            }
        }
        self.conn.info.binary.store(false, Ordering::Relaxed);
        self.last_input = Instant::now();
        let (outcome, deferred) = result?;
        for n in deferred {
            self.println(&format!("\n{}", Self::format_notice(&n))).await?;
        }
        Ok(outcome)
    }

    async fn send_raw(&mut self, bytes: Vec<u8>) -> SResult<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.conn.tx.send(OutCmd::Write(bytes)).await.map_err(|_| SessionError::Disconnected)
    }

    async fn transfer_loop(&mut self, t: &mut Transfer, first: Vec<u8>, limit: Option<u64>) -> SResult<(Outcome, Vec<Notice>)> {
        self.send_raw(first).await?;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut deferred = Vec::new();
        while t.is_running() {
            let wake = tokio::select! {
                ev = self.conn.rx.recv() => Wake::Input(ev),
                Some(n) = self.notices.recv() => Wake::Notice(n),
                _ = tick.tick() => Wake::Warn, // タイマー処理に流用
            };
            let now = std::time::Instant::now();
            let out = match wake {
                Wake::Input(Some(InEvent::Data(d))) => t.input(&d, now),
                Wake::Input(_) => return Err(SessionError::Disconnected),
                Wake::Notice(Notice::Kick(r)) => {
                    let out = t.cancel();
                    let _ = self.send_raw(out).await;
                    return Err(SessionError::Kicked(r));
                }
                Wake::Notice(n) => {
                    deferred.push(n);
                    Vec::new()
                }
                _ => t.tick(now),
            };
            self.send_raw(out).await?;
            if let Some(limit) = limit {
                if t.progress.bytes > limit && t.is_running() {
                    let out = t.cancel();
                    self.send_raw(out).await?;
                    return Ok((Outcome::Failed(format!("{} KB を超えたので中止しました", limit / 1024)), deferred));
                }
            }
        }
        Ok((t.outcome.clone(), deferred))
    }

    /// 行をページ単位で表示する。途中で Q が押されたら false
    pub async fn page(&mut self, lines: &[String]) -> SResult<bool> {
        let per_page = self.rows.saturating_sub(1).max(4);
        for (i, chunk) in lines.chunks(per_page).enumerate() {
            if i > 0 {
                let a = self.input("-- 続きます (Enter:次 Q:中止) --").await?;
                if a.eq_ignore_ascii_case("q") {
                    return Ok(false);
                }
            }
            let mut text = chunk.join("\n");
            text.push('\n');
            self.print(&text).await?;
        }
        Ok(true)
    }
}
