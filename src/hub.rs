//! 全回線で共有する状態: 回線スロット、通知、チャット部屋

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{bail, Result};
use chrono::{DateTime, Local};
use tokio::sync::{broadcast, mpsc};

use crate::line::{LineInfo, LineKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineState {
    /// 未使用 (TCP の空き回線)
    Free,
    /// モデム初期化中
    Init,
    /// 着信待ち
    Idle,
    Ringing,
    Connecting,
    /// 接続済み、ログイン前
    Login,
    Online,
    Hangup,
    Error,
}

impl LineState {
    pub fn label(self) -> &'static str {
        match self {
            LineState::Free => "空き",
            LineState::Init => "初期化",
            LineState::Idle => "待機",
            LineState::Ringing => "着信",
            LineState::Connecting => "接続中",
            LineState::Login => "ログイン",
            LineState::Online => "利用中",
            LineState::Hangup => "切断中",
            LineState::Error => "エラー",
        }
    }
}

/// セッションへの割り込み通知
#[derive(Debug, Clone)]
pub enum Notice {
    /// SYSOP からの全体放送
    System(String),
    /// 電報 (1 対 1 のメッセージ)
    Telegram { from: String, text: String },
    Chat { room: String, text: String },
    /// 強制切断
    Kick(String),
}

#[derive(Debug)]
struct Slot {
    kind: Option<LineKind>,
    /// モデム回線ならポート
    modem_path: Option<String>,
    state: LineState,
    info: Option<Arc<LineInfo>>,
    user_id: Option<i64>,
    login: String,
    handle: String,
    place: String,
    connected_at: Option<DateTime<Local>>,
    notice: Option<mpsc::Sender<Notice>>,
    monitor: Option<broadcast::Sender<Vec<u8>>>,
    note: String,
}

impl Slot {
    fn empty(modem_path: Option<String>) -> Slot {
        Slot {
            kind: modem_path.as_ref().map(|_| LineKind::Modem),
            state: if modem_path.is_some() { LineState::Init } else { LineState::Free },
            modem_path,
            info: None,
            user_id: None,
            login: String::new(),
            handle: String::new(),
            place: String::new(),
            connected_at: None,
            notice: None,
            monitor: None,
            note: String::new(),
        }
    }
}

/// 表示用のスナップショット
#[derive(Debug, Clone)]
pub struct SlotView {
    pub no: u16,
    pub kind: Option<LineKind>,
    pub state: LineState,
    pub port: String,
    pub peer: String,
    pub speed: String,
    pub user_id: Option<i64>,
    pub login: String,
    pub handle: String,
    pub place: String,
    pub connected_at: Option<DateTime<Local>>,
    pub rx: u64,
    pub tx: u64,
    pub note: String,
}

pub struct Hub {
    pub bbs_name: String,
    slots: Mutex<Vec<Slot>>,
    /// チャット部屋 → 参加している回線
    rooms: Mutex<BTreeMap<String, BTreeSet<u16>>>,
    pub started: Instant,
    pub calls: AtomicU32,
    /// 管理画面から「今すぐ応答 (ATA)」を指示されたモデム回線
    answer_requests: Mutex<BTreeSet<u16>>,
}

const NOTICE_CAP: usize = 64;

impl Hub {
    /// `modems`: (回線番号, ポート) — モデム回線はその番号に固定で割り当てる
    pub fn new(bbs_name: &str, max_lines: u16, modems: &[(u16, String)]) -> Arc<Hub> {
        let slots = (1..=max_lines)
            .map(|no| Slot::empty(modems.iter().find(|(n, _)| *n == no).map(|(_, p)| p.clone())))
            .collect();
        Arc::new(Hub {
            bbs_name: bbs_name.to_string(),
            slots: Mutex::new(slots),
            rooms: Mutex::new(BTreeMap::new()),
            started: Instant::now(),
            calls: AtomicU32::new(0),
            answer_requests: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn max_lines(&self) -> u16 {
        self.slots.lock().unwrap().len() as u16
    }

    fn with<R>(&self, no: u16, f: impl FnOnce(&mut Slot) -> R) -> Option<R> {
        let mut slots = self.slots.lock().unwrap();
        slots.get_mut(no.checked_sub(1)? as usize).map(f)
    }

    /// TCP 用に空き回線を確保する
    pub fn alloc_tcp(&self) -> Option<u16> {
        let mut slots = self.slots.lock().unwrap();
        let (i, slot) = slots.iter_mut().enumerate().find(|(_, s)| s.modem_path.is_none() && s.state == LineState::Free)?;
        slot.kind = Some(LineKind::Tcp);
        slot.state = LineState::Login;
        Some(i as u16 + 1)
    }

    /// TCP 回線を空きに戻す
    pub fn free_tcp(&self, no: u16) {
        self.with(no, |s| {
            if s.modem_path.is_none() {
                *s = Slot::empty(None);
            }
        });
    }

    /// モデム回線に、RING を待たずに応答 (ATA) するよう指示する
    pub fn request_answer(&self, no: u16) -> Result<()> {
        let v = self.snapshot().into_iter().find(|v| v.no == no);
        match v {
            Some(v) if v.kind == Some(LineKind::Modem) && matches!(v.state, LineState::Idle | LineState::Ringing) => {
                self.answer_requests.lock().unwrap().insert(no);
                Ok(())
            }
            Some(v) if v.kind == Some(LineKind::Modem) => bail!("CH{no:02} は{}のため応答できません", v.state.label()),
            _ => bail!("CH{no:02} はモデム回線ではありません"),
        }
    }

    /// 応答の指示があれば取り出す (モデム回線のタスクが呼ぶ)
    pub fn take_answer_request(&self, no: u16) -> bool {
        self.answer_requests.lock().unwrap().remove(&no)
    }

    pub fn set_state(&self, no: u16, state: LineState) {
        self.with(no, |s| s.state = state);
    }

    pub fn set_note(&self, no: u16, note: &str) {
        self.with(no, |s| s.note = note.to_string());
    }

    /// セッション開始。通知の受け口とモニタ用の送信口を返す
    pub fn attach(&self, info: Arc<LineInfo>) -> (mpsc::Receiver<Notice>, broadcast::Sender<Vec<u8>>) {
        let (tx, rx) = mpsc::channel(NOTICE_CAP);
        let (mon, _) = broadcast::channel(256);
        self.calls.fetch_add(1, Ordering::Relaxed);
        let mon2 = mon.clone();
        self.with(info.no, move |s| {
            s.info = Some(info);
            s.notice = Some(tx);
            s.monitor = Some(mon2);
            s.state = LineState::Login;
            s.connected_at = Some(Local::now());
            s.note.clear();
        });
        (rx, mon)
    }

    pub fn set_user(&self, no: u16, user_id: i64, login: &str, handle: &str) {
        self.with(no, |s| {
            s.user_id = Some(user_id);
            s.login = login.to_string();
            s.handle = handle.to_string();
            s.state = LineState::Online;
        });
    }

    /// 今いる場所 (WHO に出す)
    pub fn set_place(&self, no: u16, place: &str) {
        self.with(no, |s| s.place = place.to_string());
    }

    /// セッション終了
    pub fn detach(&self, no: u16) {
        self.rooms.lock().unwrap().values_mut().for_each(|m| {
            m.remove(&no);
        });
        self.with(no, |s| {
            s.user_id = None;
            s.login.clear();
            s.handle.clear();
            s.place.clear();
            s.notice = None;
            s.monitor = None;
            s.connected_at = None;
            s.state = LineState::Hangup;
        });
    }

    fn notify(&self, no: u16, n: Notice) -> bool {
        let tx = self.with(no, |s| s.notice.clone()).flatten();
        match tx {
            // 遅い回線で溜まりすぎたら捨てる (全体を止めないため)
            Some(tx) => tx.try_send(n).is_ok(),
            None => false,
        }
    }

    pub fn kick(&self, no: u16, reason: &str) -> bool {
        self.notify(no, Notice::Kick(reason.to_string()))
    }

    /// 全員に放送する。届いた人数を返す
    pub fn broadcast(&self, text: &str) -> usize {
        let targets: Vec<u16> = self.who().iter().map(|v| v.no).collect();
        targets.into_iter().filter(|&no| self.notify(no, Notice::System(text.to_string()))).count()
    }

    /// 電報。`target` は回線番号かログイン ID。送り先の表示名を返す
    pub fn telegram(&self, target: &str, from: &str, text: &str) -> Result<String> {
        let who = self.who();
        let dest = match target.parse::<u16>() {
            Ok(no) => who.iter().find(|v| v.no == no),
            Err(_) => who.iter().find(|v| v.login.eq_ignore_ascii_case(target)),
        };
        let Some(dest) = dest else { bail!("{target} は接続していません") };
        if !self.notify(dest.no, Notice::Telegram { from: from.to_string(), text: text.to_string() }) {
            bail!("{} に届けられませんでした", dest.handle);
        }
        Ok(format!("CH{:02} {}", dest.no, dest.handle))
    }

    pub fn monitor(&self, no: u16) -> Option<broadcast::Receiver<Vec<u8>>> {
        self.with(no, |s| s.monitor.as_ref().map(|m| m.subscribe())).flatten()
    }

    fn view(no: u16, s: &Slot) -> SlotView {
        let (peer, speed, rx, tx) = match &s.info {
            Some(i) => (
                i.peer(),
                i.speed(),
                i.rx_bytes.load(Ordering::Relaxed),
                i.tx_bytes.load(Ordering::Relaxed),
            ),
            None => (String::new(), String::new(), 0, 0),
        };
        SlotView {
            no,
            kind: s.kind,
            state: s.state,
            port: s.modem_path.clone().unwrap_or_default(),
            peer,
            speed,
            user_id: s.user_id,
            login: s.login.clone(),
            handle: s.handle.clone(),
            place: s.place.clone(),
            connected_at: s.connected_at,
            rx,
            tx,
            note: s.note.clone(),
        }
    }

    pub fn snapshot(&self) -> Vec<SlotView> {
        let slots = self.slots.lock().unwrap();
        slots.iter().enumerate().map(|(i, s)| Self::view(i as u16 + 1, s)).collect()
    }

    /// ログイン中の回線
    pub fn who(&self) -> Vec<SlotView> {
        self.snapshot().into_iter().filter(|v| v.user_id.is_some()).collect()
    }

    // ------------------------------------------------------------ chat

    pub fn chat_members(&self, room: &str) -> Vec<(u16, String)> {
        let members: Vec<u16> = self.rooms.lock().unwrap().get(room).map(|m| m.iter().copied().collect()).unwrap_or_default();
        let who = self.who();
        members
            .into_iter()
            .filter_map(|no| who.iter().find(|v| v.no == no).map(|v| (no, v.handle.clone())))
            .collect()
    }

    pub fn chat_rooms(&self) -> Vec<(String, usize)> {
        self.rooms.lock().unwrap().iter().filter(|(_, m)| !m.is_empty()).map(|(r, m)| (r.clone(), m.len())).collect()
    }

    /// 部屋の自分以外の全員に送る
    fn chat_send(&self, room: &str, except: u16, text: &str) {
        let members: Vec<u16> = self.rooms.lock().unwrap().get(room).map(|m| m.iter().copied().collect()).unwrap_or_default();
        for no in members.into_iter().filter(|&n| n != except) {
            self.notify(no, Notice::Chat { room: room.to_string(), text: text.to_string() });
        }
    }

    pub fn chat_join(&self, room: &str, no: u16, handle: &str) {
        self.rooms.lock().unwrap().entry(room.to_string()).or_default().insert(no);
        self.chat_send(room, no, &format!("*** {handle} さんが入室しました (CH{no:02})"));
    }

    pub fn chat_leave(&self, room: &str, no: u16, handle: &str) {
        if let Some(m) = self.rooms.lock().unwrap().get_mut(room) {
            m.remove(&no);
        }
        self.chat_send(room, no, &format!("*** {handle} さんが退室しました"));
    }

    pub fn chat_say(&self, room: &str, no: u16, handle: &str, text: &str) {
        self.chat_send(room, no, &format!("{handle}> {text}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_skips_modem_lines() {
        let hub = Hub::new("T", 3, &[(1, "/dev/x".into())]);
        assert_eq!(hub.alloc_tcp(), Some(2));
        assert_eq!(hub.alloc_tcp(), Some(3));
        assert_eq!(hub.alloc_tcp(), None);
        hub.free_tcp(2);
        assert_eq!(hub.alloc_tcp(), Some(2));
    }

    #[tokio::test]
    async fn telegram_and_chat() {
        let hub = Hub::new("T", 4, &[]);
        let a = hub.alloc_tcp().unwrap();
        let b = hub.alloc_tcp().unwrap();
        let (_ra, _) = hub.attach(LineInfo::new(a, LineKind::Tcp, "x"));
        let (mut rb, _) = hub.attach(LineInfo::new(b, LineKind::Tcp, "y"));
        hub.set_user(a, 1, "alice", "アリス");
        hub.set_user(b, 2, "bob", "ボブ");
        assert!(hub.telegram("BOB", "アリス", "やあ").is_ok());
        assert!(matches!(rb.recv().await, Some(Notice::Telegram { .. })));
        hub.chat_join("LOBBY", b, "ボブ");
        hub.chat_join("LOBBY", a, "アリス");
        assert!(matches!(rb.recv().await, Some(Notice::Chat { text, .. }) if text.contains("入室")));
        hub.chat_say("LOBBY", a, "アリス", "こんにちは");
        assert!(matches!(rb.recv().await, Some(Notice::Chat { text, .. }) if text == "アリス> こんにちは"));
        assert_eq!(hub.chat_members("LOBBY").len(), 2);
        assert!(hub.telegram("9", "x", "y").is_err());
    }
}
