//! telnet プロトコルの最小限の処理 (I/O を持たない状態機械)
//!
//! - 接続時にサーバー側エコー (WILL ECHO) と SGA を宣言する
//! - IAC 列を取り除き、必要な応答を返す
//! - CR NUL / CR LF は CR 1 つにそろえる

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const OPT_BINARY: u8 = 0;
const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    Iac,
    Opt(u8),
    Sub,
    SubIac,
}

pub struct Telnet {
    /// バイナリ転送中は CR の後の NUL / LF を捨てない
    pub binary: bool,
    state: State,
    after_cr: bool,
    /// 送った応答 (同じ応答を繰り返さない)
    sent: Vec<(u8, u8)>,
}

impl Default for Telnet {
    fn default() -> Self {
        Self::new()
    }
}

impl Telnet {
    pub fn new() -> Self {
        Telnet { binary: false, state: State::Data, after_cr: false, sent: vec![(WILL, OPT_ECHO), (WILL, OPT_SGA)] }
    }

    /// 接続直後に送るネゴシエーション
    pub fn greeting() -> Vec<u8> {
        vec![IAC, WILL, OPT_ECHO, IAC, WILL, OPT_SGA, IAC, DO, OPT_SGA]
    }

    /// 送信データの 0xFF を IAC IAC にする
    pub fn escape(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        for &b in data {
            out.push(b);
            if b == IAC {
                out.push(IAC);
            }
        }
        out
    }

    fn reply(&mut self, cmd: u8, opt: u8, replies: &mut Vec<u8>) {
        if !self.sent.contains(&(cmd, opt)) {
            self.sent.push((cmd, opt));
            replies.extend_from_slice(&[IAC, cmd, opt]);
        }
    }

    fn negotiate(&mut self, cmd: u8, opt: u8, replies: &mut Vec<u8>) {
        match cmd {
            // こちらがするべきことの要求: ECHO と SGA だけ引き受ける
            DO if opt == OPT_ECHO || opt == OPT_SGA || opt == OPT_BINARY => self.reply(WILL, opt, replies),
            DO => self.reply(WONT, opt, replies),
            DONT => self.reply(WONT, opt, replies),
            // 相手がしたいこと: SGA だけ受け入れる
            WILL if opt == OPT_SGA || opt == OPT_BINARY => self.reply(DO, opt, replies),
            WILL => self.reply(DONT, opt, replies),
            _ => {} // WONT
        }
    }

    /// 受信データを処理する。(アプリ向けデータ, 相手への応答) を返す
    pub fn feed(&mut self, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut data = Vec::with_capacity(input.len());
        let mut replies = Vec::new();
        for &b in input {
            match self.state {
                State::Data => {
                    if b == IAC {
                        self.state = State::Iac;
                        continue;
                    }
                    if self.after_cr && !self.binary && (b == 0 || b == b'\n') {
                        self.after_cr = false;
                        continue;
                    }
                    self.after_cr = b == b'\r';
                    data.push(b);
                }
                State::Iac => {
                    self.state = match b {
                        IAC => {
                            self.after_cr = false;
                            data.push(IAC);
                            State::Data
                        }
                        DO | DONT | WILL | WONT => State::Opt(b),
                        SB => State::Sub,
                        _ => State::Data, // NOP / GA / AYT などは無視
                    };
                }
                State::Opt(cmd) => {
                    self.negotiate(cmd, b, &mut replies);
                    self.state = State::Data;
                }
                State::Sub => {
                    if b == IAC {
                        self.state = State::SubIac;
                    }
                }
                State::SubIac => {
                    self.state = if b == SE { State::Data } else { State::Sub };
                }
            }
        }
        (data, replies)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_commands_split_across_chunks() {
        let mut t = Telnet::new();
        let (d, r) = t.feed(&[b'a', IAC]);
        assert_eq!((d, r), (b"a".to_vec(), vec![]));
        let (d, r) = t.feed(&[DO, 24, b'b', IAC, SB, 24, 1, IAC, SE, b'c']);
        assert_eq!(d, b"bc");
        assert_eq!(r, vec![IAC, WONT, 24]);
        // 同じ要求には 2 度応答しない
        let (_, r) = t.feed(&[IAC, DO, 24]);
        assert!(r.is_empty());
    }

    #[test]
    fn accepts_echo_and_sga() {
        let mut t = Telnet::new();
        let (_, r) = t.feed(&[IAC, DO, OPT_ECHO, IAC, DO, OPT_SGA]);
        assert!(r.is_empty(), "すでに WILL を送っている");
        let (_, r) = t.feed(&[IAC, WILL, OPT_SGA, IAC, WILL, 31]);
        assert_eq!(r, vec![IAC, DO, OPT_SGA, IAC, DONT, 31]);
    }

    #[test]
    fn crlf_and_crnul() {
        let mut t = Telnet::new();
        assert_eq!(t.feed(b"ab\r\ncd\r\0e\r").0, b"ab\rcd\re\r");
        assert_eq!(t.feed(b"\n").0, b"", "CR と LF が分かれて届いても LF は捨てる");
        assert_eq!(t.feed(&[IAC, IAC]).0, vec![IAC]);
        assert_eq!(Telnet::escape(&[1, IAC, 2]), vec![1, IAC, IAC, 2]);
    }
}
