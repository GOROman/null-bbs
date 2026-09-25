//! XMODEM / XMODEM-1K / YMODEM のファイル転送
//!
//! シリアル I/O から切り離した状態機械として実装している。
//! 受信バイトを `input` に、時間経過を `tick` に渡すと、送るべきバイト列を返す。

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const EOT: u8 = 0x04;
const ACK: u8 = 0x06;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const CRC_REQ: u8 = b'C';
const SUB: u8 = 0x1a;

/// 応答待ちのタイムアウト
const ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// 送信側が受信側の開始合図を待つ時間
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// 受信側が開始合図 (C / NAK) を送り直す間隔
const START_INTERVAL: Duration = Duration::from_secs(3);
/// 同じブロックの再送・受信エラーの上限
const MAX_ERRORS: u32 = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protocol {
    Xmodem,
    Xmodem1k,
    Ymodem,
}

impl Protocol {
    pub const ALL: [Protocol; 3] = [Protocol::Xmodem, Protocol::Xmodem1k, Protocol::Ymodem];

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Xmodem => "XMODEM",
            Protocol::Xmodem1k => "XMODEM-1K",
            Protocol::Ymodem => "YMODEM",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "x" | "xmodem" => Protocol::Xmodem,
            "x1k" | "xmodem1k" | "1k" => Protocol::Xmodem1k,
            "y" | "ymodem" => Protocol::Ymodem,
            _ => bail!("プロトコルは xmodem / xmodem-1k / ymodem のいずれか: {s}"),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Send,
    Recv,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Direction::Send => "送信",
            Direction::Recv => "受信",
        }
    }
}

/// 転送の進み具合 (表示用)
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub file: String,
    pub bytes: u64,
    pub total: Option<u64>,
    pub files_done: u32,
    pub errors: u32,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Running,
    Done(String),
    Failed(String),
}

pub struct Transfer {
    pub protocol: Protocol,
    pub direction: Direction,
    pub progress: Progress,
    pub outcome: Outcome,
    inner: Inner,
}

enum Inner {
    Send(Sender),
    Recv(Receiver),
}

impl Transfer {
    pub fn send(protocol: Protocol, files: Vec<PathBuf>, now: Instant) -> Result<Self> {
        if files.is_empty() {
            bail!("送信するファイルを指定してください");
        }
        if protocol != Protocol::Ymodem && files.len() > 1 {
            bail!("{} は 1 ファイルずつしか送れません", protocol.label());
        }
        for f in &files {
            if !f.is_file() {
                bail!("ファイルがありません: {}", f.display());
            }
        }
        let mut t = Transfer {
            protocol,
            direction: Direction::Send,
            progress: Progress::default(),
            outcome: Outcome::Running,
            inner: Inner::Send(Sender::new(protocol, files.into(), now)),
        };
        t.sync();
        Ok(t)
    }

    /// `dest`: XMODEM なら保存するファイル名、YMODEM なら保存先ディレクトリ
    pub fn recv(protocol: Protocol, dest: PathBuf, now: Instant) -> Result<(Self, Vec<u8>)> {
        match protocol {
            Protocol::Ymodem => {
                if !dest.is_dir() {
                    bail!("保存先ディレクトリがありません: {}", dest.display());
                }
            }
            _ => {
                if dest.is_dir() {
                    bail!("XMODEM は保存するファイル名を指定してください: {}", dest.display());
                }
            }
        }
        let (r, out) = Receiver::new(protocol, dest, now);
        let mut t = Transfer {
            protocol,
            direction: Direction::Recv,
            progress: Progress::default(),
            outcome: Outcome::Running,
            inner: Inner::Recv(r),
        };
        t.sync();
        Ok((t, out))
    }

    pub fn is_running(&self) -> bool {
        matches!(self.outcome, Outcome::Running)
    }

    /// 受信したバイト列を処理し、送信すべきバイト列を返す
    pub fn input(&mut self, data: &[u8], now: Instant) -> Vec<u8> {
        let mut out = Vec::new();
        if self.is_running() {
            match &mut self.inner {
                Inner::Send(s) => s.input(data, now, &mut out),
                Inner::Recv(r) => r.input(data, now, &mut out),
            }
            self.sync();
        }
        out
    }

    /// タイムアウト処理。送信すべきバイト列を返す
    pub fn tick(&mut self, now: Instant) -> Vec<u8> {
        let mut out = Vec::new();
        if self.is_running() {
            match &mut self.inner {
                Inner::Send(s) => s.tick(now, &mut out),
                Inner::Recv(r) => r.tick(now, &mut out),
            }
            self.sync();
        }
        out
    }

    /// ユーザー操作による中止。相手に送る CAN 列を返す
    pub fn cancel(&mut self) -> Vec<u8> {
        if !self.is_running() {
            return Vec::new();
        }
        self.outcome = Outcome::Failed("中止しました".into());
        cancel_bytes()
    }

    fn sync(&mut self) {
        let (progress, outcome) = match &self.inner {
            Inner::Send(s) => (&s.progress, &s.outcome),
            Inner::Recv(r) => (&r.progress, &r.outcome),
        };
        self.progress = progress.clone();
        if self.is_running() {
            self.outcome = outcome.clone();
        }
    }
}

fn cancel_bytes() -> Vec<u8> {
    vec![CAN; 5]
}

pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

fn make_packet(blk: u8, payload: &[u8], size: usize, crc: bool) -> Vec<u8> {
    let mut data = payload.to_vec();
    data.resize(size, SUB);
    let mut p = Vec::with_capacity(size + 5);
    p.push(if size == 1024 { STX } else { SOH });
    p.push(blk);
    p.push(!blk);
    p.extend_from_slice(&data);
    if crc {
        p.extend_from_slice(&crc16(&data).to_be_bytes());
    } else {
        p.push(checksum(&data));
    }
    p
}

/// YMODEM のブロック 0 (「ファイル名 NUL サイズ 更新日時(8進) モード(8進)」)。`None` はバッチ終了
fn ymodem_header(file: Option<(&str, u64, u64)>) -> Vec<u8> {
    let mut h = Vec::new();
    if let Some((name, size, mtime)) = file {
        h.extend_from_slice(name.as_bytes());
        h.push(0);
        h.extend_from_slice(format!("{size} {mtime:o} 100644").as_bytes());
    }
    // 余りは SUB ではなく NUL で埋める
    let size = if h.len() > 128 { 1024 } else { 128 };
    h.resize(size, 0);
    make_packet(0, &h, size, true)
}

// ---------------------------------------------------------------- 送信側

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// YMODEM のブロック 0
    Header,
    /// データブロック
    Data,
    Eot,
    /// YMODEM の空ブロック 0 (バッチ終了)
    BatchEnd,
}

enum SendPhase {
    /// 受信側の開始合図 (C / NAK) 待ち
    WaitStart(Stage),
    /// 送ったパケットへの ACK 待ち
    WaitAck(Stage),
}

struct Sender {
    protocol: Protocol,
    files: VecDeque<PathBuf>,
    data: Vec<u8>,
    pos: usize,
    blk: u8,
    crc: bool,
    phase: SendPhase,
    last: Vec<u8>,
    last_len: usize,
    deadline: Instant,
    errors: u32,
    cans: u32,
    progress: Progress,
    outcome: Outcome,
    sent_files: Vec<String>,
    /// 送信中ファイルの更新日時 (UNIX 時刻)
    mtime: u64,
}

impl Sender {
    fn new(protocol: Protocol, files: VecDeque<PathBuf>, now: Instant) -> Self {
        let mut s = Sender {
            protocol,
            files,
            data: Vec::new(),
            pos: 0,
            blk: 1,
            crc: true,
            phase: SendPhase::WaitStart(Stage::Data),
            last: Vec::new(),
            last_len: 0,
            deadline: now + START_TIMEOUT,
            errors: 0,
            cans: 0,
            progress: Progress::default(),
            outcome: Outcome::Running,
            sent_files: Vec::new(),
            mtime: 0,
        };
        if let Err(e) = s.load_next() {
            s.outcome = Outcome::Failed(format!("{e:#}"));
        }
        s.phase = SendPhase::WaitStart(if protocol == Protocol::Ymodem { Stage::Header } else { Stage::Data });
        s
    }

    fn load_next(&mut self) -> Result<bool> {
        let Some(path) = self.files.pop_front() else { return Ok(false) };
        self.data = fs::read(&path).with_context(|| format!("読み込めません: {}", path.display()))?;
        self.mtime = fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.pos = 0;
        self.blk = 1;
        self.progress.file = file_name(&path);
        self.progress.bytes = 0;
        self.progress.total = Some(self.data.len() as u64);
        Ok(true)
    }

    fn fail(&mut self, msg: impl Into<String>, out: &mut Vec<u8>) {
        out.extend(cancel_bytes());
        self.outcome = Outcome::Failed(msg.into());
    }

    fn transmit(&mut self, packet: Vec<u8>, stage: Stage, now: Instant, out: &mut Vec<u8>) {
        out.extend_from_slice(&packet);
        self.last = packet;
        self.phase = SendPhase::WaitAck(stage);
        self.deadline = now + ACK_TIMEOUT;
    }

    fn send_stage(&mut self, stage: Stage, now: Instant, out: &mut Vec<u8>) {
        match stage {
            Stage::Header => {
                let name = self.progress.file.clone();
                let packet = ymodem_header(Some((&name, self.data.len() as u64, self.mtime)));
                self.transmit(packet, stage, now, out);
            }
            Stage::BatchEnd => self.transmit(ymodem_header(None), stage, now, out),
            Stage::Eot => self.transmit(vec![EOT], stage, now, out),
            Stage::Data => {
                if self.pos >= self.data.len() {
                    // 0 バイトのファイルや最後のブロックの後は EOT
                    return self.send_stage(Stage::Eot, now, out);
                }
                let rest = self.data.len() - self.pos;
                let use_1k = self.crc && self.protocol != Protocol::Xmodem && rest > 128;
                let size = if use_1k { 1024 } else { 128 };
                let len = rest.min(size);
                let packet = make_packet(self.blk, &self.data[self.pos..self.pos + len], size, self.crc);
                self.last_len = len;
                self.transmit(packet, stage, now, out);
            }
        }
    }

    fn input(&mut self, data: &[u8], now: Instant, out: &mut Vec<u8>) {
        for &b in data {
            if !matches!(self.outcome, Outcome::Running) {
                return;
            }
            if b == CAN {
                self.cans += 1;
                if self.cans >= 2 {
                    self.outcome = Outcome::Failed("相手が中止しました".into());
                }
                continue;
            }
            self.cans = 0;
            match self.phase {
                SendPhase::WaitStart(stage) => {
                    let ok = match b {
                        CRC_REQ => {
                            self.crc = true;
                            true
                        }
                        // チェックサム方式は XMODEM の開始時だけ受け付ける
                        NAK if stage == Stage::Data && self.protocol != Protocol::Ymodem && self.blk == 1 => {
                            self.crc = false;
                            true
                        }
                        _ => false,
                    };
                    if ok {
                        self.send_stage(stage, now, out);
                    }
                }
                SendPhase::WaitAck(stage) => match b {
                    ACK => {
                        self.errors = 0;
                        self.on_ack(stage, now, out);
                    }
                    NAK => {
                        self.errors += 1;
                        self.progress.errors += 1;
                        if self.errors > MAX_ERRORS {
                            self.fail("再送が多すぎます", out);
                        } else {
                            out.extend_from_slice(&self.last);
                            self.deadline = now + ACK_TIMEOUT;
                        }
                    }
                    // ヘッダ ACK 前に C が来ることがある (ACK を取りこぼした相手)
                    CRC_REQ if stage == Stage::Header => {}
                    _ => {}
                },
            }
        }
    }

    fn on_ack(&mut self, stage: Stage, now: Instant, out: &mut Vec<u8>) {
        match stage {
            Stage::Header => {
                // ヘッダの ACK の後、受信側が C を送ってからデータ開始
                self.phase = SendPhase::WaitStart(Stage::Data);
                self.deadline = now + START_TIMEOUT;
            }
            Stage::Data => {
                self.pos += self.last_len;
                self.blk = self.blk.wrapping_add(1);
                self.progress.bytes = self.pos as u64;
                self.send_stage(Stage::Data, now, out);
            }
            Stage::Eot => {
                self.progress.files_done += 1;
                self.sent_files.push(self.progress.file.clone());
                if self.protocol != Protocol::Ymodem {
                    self.outcome = Outcome::Done(format!("{} を送信しました", self.progress.file));
                    return;
                }
                match self.load_next() {
                    Ok(true) => {
                        self.phase = SendPhase::WaitStart(Stage::Header);
                        self.deadline = now + START_TIMEOUT;
                    }
                    Ok(false) => {
                        self.phase = SendPhase::WaitStart(Stage::BatchEnd);
                        self.deadline = now + START_TIMEOUT;
                    }
                    Err(e) => self.fail(format!("{e:#}"), out),
                }
            }
            Stage::BatchEnd => {
                self.outcome = Outcome::Done(format!("{} を送信しました", self.sent_files.join(", ")));
            }
        }
    }

    fn tick(&mut self, now: Instant, out: &mut Vec<u8>) {
        if now < self.deadline {
            return;
        }
        match self.phase {
            SendPhase::WaitStart(_) => self.fail("受信側が応答しません", out),
            SendPhase::WaitAck(_) => {
                self.errors += 1;
                self.progress.errors += 1;
                if self.errors > MAX_ERRORS {
                    self.fail("応答がありません", out);
                } else {
                    out.extend_from_slice(&self.last);
                    self.deadline = now + ACK_TIMEOUT;
                }
            }
        }
    }
}

// ---------------------------------------------------------------- 受信側

#[derive(PartialEq, Eq)]
enum RecvPhase {
    /// YMODEM のブロック 0 待ち
    Header,
    Data,
}

struct Receiver {
    protocol: Protocol,
    dest: PathBuf,
    crc: bool,
    phase: RecvPhase,
    expect: u8,
    buf: Vec<u8>,
    file: Option<File>,
    path: Option<PathBuf>,
    size: Option<u64>,
    /// XMODEM では末尾の SUB を削るため最後のブロックを保留する
    held: Vec<u8>,
    started: bool,
    start_tries: u32,
    deadline: Instant,
    errors: u32,
    cans: u32,
    progress: Progress,
    outcome: Outcome,
    saved: Vec<String>,
}

impl Receiver {
    fn new(protocol: Protocol, dest: PathBuf, now: Instant) -> (Self, Vec<u8>) {
        let r = Receiver {
            protocol,
            dest,
            crc: true,
            phase: if protocol == Protocol::Ymodem { RecvPhase::Header } else { RecvPhase::Data },
            expect: 1,
            buf: Vec::new(),
            file: None,
            path: None,
            size: None,
            held: Vec::new(),
            started: false,
            start_tries: 1,
            deadline: now + START_INTERVAL,
            errors: 0,
            cans: 0,
            progress: Progress::default(),
            outcome: Outcome::Running,
            saved: Vec::new(),
        };
        (r, vec![CRC_REQ])
    }

    fn fail(&mut self, msg: impl Into<String>, out: &mut Vec<u8>) {
        out.extend(cancel_bytes());
        self.close_file();
        self.outcome = Outcome::Failed(msg.into());
    }

    fn packet_len(&self, first: u8) -> usize {
        let size = if first == STX { 1024 } else { 128 };
        3 + size + if self.crc { 2 } else { 1 }
    }

    fn input(&mut self, data: &[u8], now: Instant, out: &mut Vec<u8>) {
        for &b in data {
            if !matches!(self.outcome, Outcome::Running) {
                return;
            }
            if self.buf.is_empty() {
                match b {
                    SOH | STX => {
                        self.buf.push(b);
                        self.cans = 0;
                    }
                    EOT => {
                        self.cans = 0;
                        self.on_eot(now, out);
                    }
                    CAN => {
                        self.cans += 1;
                        if self.cans >= 2 {
                            self.close_file();
                            self.outcome = Outcome::Failed("相手が中止しました".into());
                        }
                    }
                    _ => {} // パケット外のゴミは無視
                }
                continue;
            }
            self.buf.push(b);
            if self.buf.len() == self.packet_len(self.buf[0]) {
                let packet = std::mem::take(&mut self.buf);
                self.on_packet(&packet, now, out);
            }
        }
    }

    fn on_packet(&mut self, p: &[u8], now: Instant, out: &mut Vec<u8>) {
        self.started = true;
        self.deadline = now + ACK_TIMEOUT;
        let size = if p[0] == STX { 1024 } else { 128 };
        let (blk, nblk) = (p[1], p[2]);
        let body = &p[3..3 + size];
        let valid = blk == !nblk
            && if self.crc {
                crc16(body).to_be_bytes() == p[3 + size..3 + size + 2]
            } else {
                checksum(body) == p[3 + size]
            };
        if !valid {
            return self.nak(out);
        }
        if self.phase == RecvPhase::Header {
            if blk != 0 {
                return self.nak(out);
            }
            return self.on_header(body, now, out);
        }
        if blk == self.expect.wrapping_sub(1) {
            // 前のブロックの再送 (ACK が届かなかった)
            out.push(ACK);
            return;
        }
        if blk != self.expect {
            return self.fail("ブロック番号が飛びました", out);
        }
        if let Err(e) = self.write_block(body) {
            return self.fail(format!("{e:#}"), out);
        }
        self.expect = self.expect.wrapping_add(1);
        self.errors = 0;
        out.push(ACK);
    }

    fn nak(&mut self, out: &mut Vec<u8>) {
        self.errors += 1;
        self.progress.errors += 1;
        if self.errors > MAX_ERRORS {
            self.fail("受信エラーが多すぎます", out);
        } else {
            out.push(NAK);
        }
    }

    fn on_header(&mut self, body: &[u8], now: Instant, out: &mut Vec<u8>) {
        if body[0] == 0 {
            // 空のヘッダ = バッチ終了
            out.push(ACK);
            self.outcome = Outcome::Done(if self.saved.is_empty() {
                "受信するファイルはありませんでした".into()
            } else {
                format!("{} を受信しました", self.saved.join(", "))
            });
            return;
        }
        let nul = body.iter().position(|&c| c == 0).unwrap_or(body.len());
        let name = decode_name(&body[..nul]);
        let rest = &body[(nul + 1).min(body.len())..];
        let size_str: String = rest
            .iter()
            .take_while(|&&c| c != 0 && c != b' ')
            .map(|&c| c as char)
            .collect();
        self.size = size_str.parse().ok();
        let path = unique_path(&self.dest.join(safe_name(&name)));
        match File::create(&path) {
            Ok(f) => self.file = Some(f),
            Err(e) => return self.fail(format!("保存できません {}: {e}", path.display()), out),
        }
        self.progress.file = file_name(&path);
        self.progress.bytes = 0;
        self.progress.total = self.size;
        self.path = Some(path);
        self.phase = RecvPhase::Data;
        self.expect = 1;
        self.errors = 0;
        out.push(ACK);
        out.push(CRC_REQ);
        self.deadline = now + ACK_TIMEOUT;
    }

    fn open_xmodem_file(&mut self) -> Result<()> {
        if self.file.is_none() {
            let f = File::create(&self.dest).with_context(|| format!("保存できません: {}", self.dest.display()))?;
            self.file = Some(f);
            self.progress.file = file_name(&self.dest);
            self.path = Some(self.dest.clone());
        }
        Ok(())
    }

    fn write_block(&mut self, body: &[u8]) -> Result<()> {
        if self.protocol == Protocol::Ymodem {
            let data = match self.size {
                Some(size) => &body[..(size.saturating_sub(self.progress.bytes) as usize).min(body.len())],
                None => body,
            };
            self.file.as_mut().context("ファイルが開かれていません")?.write_all(data)?;
            self.progress.bytes += data.len() as u64;
        } else {
            self.open_xmodem_file()?;
            let held = std::mem::replace(&mut self.held, body.to_vec());
            self.file.as_mut().unwrap().write_all(&held)?;
            self.progress.bytes += body.len() as u64;
        }
        Ok(())
    }

    fn on_eot(&mut self, now: Instant, out: &mut Vec<u8>) {
        if self.phase == RecvPhase::Header {
            out.push(ACK);
            return;
        }
        // XMODEM: 最後のブロックの詰め物 (SUB) を取り除く
        if self.protocol != Protocol::Ymodem {
            if let Err(e) = self.open_xmodem_file() {
                return self.fail(format!("{e:#}"), out);
            }
            let mut last = std::mem::take(&mut self.held);
            while last.last() == Some(&SUB) {
                last.pop();
            }
            if let Err(e) = self.file.as_mut().unwrap().write_all(&last) {
                return self.fail(format!("{e:#}"), out);
            }
        }
        out.push(ACK);
        self.progress.files_done += 1;
        if let Some(p) = self.path.take() {
            self.saved.push(p.display().to_string());
        }
        self.close_file();
        if self.protocol == Protocol::Ymodem {
            // 次のファイル (またはバッチ終了) のヘッダを要求
            self.phase = RecvPhase::Header;
            self.expect = 1;
            out.push(CRC_REQ);
            self.deadline = now + ACK_TIMEOUT;
        } else {
            self.outcome = Outcome::Done(format!("{} を受信しました", self.saved.join(", ")));
        }
    }

    fn close_file(&mut self) {
        if let Some(mut f) = self.file.take() {
            let _ = f.flush();
        }
    }

    fn tick(&mut self, now: Instant, out: &mut Vec<u8>) {
        if now < self.deadline {
            return;
        }
        if !self.started {
            // 開始合図を送り直す。XMODEM は C に反応がなければチェックサム方式 (NAK) に切り替える
            self.start_tries += 1;
            if self.start_tries > 20 {
                return self.fail("送信側が応答しません", out);
            }
            if self.protocol == Protocol::Xmodem && self.start_tries > 3 {
                self.crc = false;
            }
            out.push(if self.crc { CRC_REQ } else { NAK });
            self.deadline = now + START_INTERVAL;
            return;
        }
        self.buf.clear();
        self.nak(out);
        self.deadline = now + ACK_TIMEOUT;
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

fn decode_name(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

/// パス区切りなどを取り除いたファイル名
fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let base: String = base.chars().filter(|c| !c.is_control()).collect();
    if base.is_empty() || base == "." || base == ".." {
        "received.bin".into()
    } else {
        base
    }
}

/// 既存ファイルを上書きしないよう name.1, name.2 ... を付ける
fn unique_path(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    (1..)
        .map(|i| PathBuf::from(format!("{}.{i}", p.display())))
        .find(|c| !c.exists())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_xmodem() {
        assert_eq!(crc16(b"123456789"), 0x31c3);
    }

    /// 送信側と受信側を直結して転送させる
    fn run(proto: Protocol, files: Vec<PathBuf>, dest: PathBuf) -> (Outcome, Outcome) {
        let mut now = Instant::now();
        let mut tx = Transfer::send(proto, files, now).unwrap();
        let (mut rx, mut to_tx) = Transfer::recv(proto, dest, now).unwrap();
        for _ in 0..100_000 {
            let to_rx = tx.input(&to_tx, now);
            to_tx = rx.input(&to_rx, now);
            if !tx.is_running() && !rx.is_running() {
                break;
            }
            if to_tx.is_empty() && to_rx.is_empty() {
                now += Duration::from_secs(1);
                to_tx = rx.tick(now);
                let more = tx.tick(now);
                to_tx.extend(rx.input(&more, now));
            }
        }
        (tx.outcome, rx.outcome)
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("null-bbs-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[test]
    fn ymodem_batch() {
        let d = tmpdir("y");
        let (src, dst) = (d.join("src"), d.join("dst"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        let sizes = [0usize, 1, 128, 129, 1024, 1025, 5000];
        let files: Vec<PathBuf> = sizes
            .iter()
            .map(|&n| {
                let p = src.join(format!("f{n}.bin"));
                fs::write(&p, sample(n)).unwrap();
                p
            })
            .collect();
        let (t, r) = run(Protocol::Ymodem, files, dst.clone());
        assert!(matches!(t, Outcome::Done(_)), "{t:?}");
        assert!(matches!(r, Outcome::Done(_)), "{r:?}");
        for n in sizes {
            assert_eq!(fs::read(dst.join(format!("f{n}.bin"))).unwrap(), sample(n), "size {n}");
        }
    }

    #[test]
    fn xmodem_variants() {
        for proto in [Protocol::Xmodem, Protocol::Xmodem1k] {
            let d = tmpdir(proto.label());
            let src = d.join("a.txt");
            // 末尾が SUB でないデータは長さも一致する
            let data = sample(3000);
            fs::write(&src, &data).unwrap();
            let dst = d.join("b.txt");
            let (t, r) = run(proto, vec![src], dst.clone());
            assert!(matches!(t, Outcome::Done(_)), "{t:?}");
            assert!(matches!(r, Outcome::Done(_)), "{r:?}");
            assert_eq!(fs::read(&dst).unwrap(), data);
        }
    }

    #[test]
    fn corrupted_packet_is_resent() {
        let d = tmpdir("err");
        let src = d.join("a.bin");
        fs::write(&src, sample(2000)).unwrap();
        let now = Instant::now();
        let mut tx = Transfer::send(Protocol::Xmodem, vec![src], now).unwrap();
        let (mut rx, start) = Transfer::recv(Protocol::Xmodem, d.join("b.bin"), now).unwrap();
        let mut pkt = tx.input(&start, now);
        pkt[10] ^= 0xff; // 1 ブロック目を壊す
        let reply = rx.input(&pkt, now);
        assert_eq!(reply, vec![NAK]);
        let mut to_tx = reply;
        for _ in 0..100 {
            let to_rx = tx.input(&to_tx, now);
            to_tx = rx.input(&to_rx, now);
            if !tx.is_running() {
                break;
            }
        }
        assert!(matches!(tx.outcome, Outcome::Done(_)));
        assert_eq!(fs::read(d.join("b.bin")).unwrap(), sample(2000));
        assert_eq!(tx.progress.errors, 1);
    }
}
