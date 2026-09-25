//! 1 行入力のエディタ (I/O を持たない)
//!
//! 受信バイトを 1 つずつ渡すと、エコーするバイト列と確定した行を返す。
//! UTF-8 のマルチバイト文字が分割されて届いても扱える。

use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy)]
pub struct EditOpts {
    /// 最大文字数
    pub max_chars: usize,
    /// パスワード入力 (* でエコー)
    pub mask: bool,
    /// 全角英数字を半角にそろえる (コマンド入力用)
    pub normalize: bool,
}

impl Default for EditOpts {
    fn default() -> Self {
        EditOpts { max_chars: 200, mask: false, normalize: true }
    }
}

pub struct Editor {
    opts: EditOpts,
    buf: String,
    /// 途中まで届いた UTF-8
    pending: Vec<u8>,
    after_cr: bool,
    /// エスケープシーケンス (矢印キーなど) を読み飛ばし中: 0 なし / 1 ESC の直後 / 2 CSI
    esc: u8,
}

pub const BS_ERASE: &[u8] = b"\x08 \x08";

fn width(c: char, mask: bool) -> usize {
    if mask { 1 } else { c.width().unwrap_or(0).max(1) }
}

/// 全角英数字・記号と全角空白を半角にする
pub fn to_halfwidth(c: char) -> char {
    match c {
        '\u{3000}' => ' ',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

impl Editor {
    pub fn new(opts: EditOpts) -> Self {
        Editor { opts, buf: String::new(), pending: Vec::new(), after_cr: false, esc: 0 }
    }

    /// 入力途中の文字列 (割り込み表示の後に出し直す用)
    pub fn display(&self) -> String {
        if self.opts.mask { "*".repeat(self.buf.chars().count()) } else { self.buf.clone() }
    }

    fn erase_last(&mut self, echo: &mut Vec<u8>) {
        if let Some(c) = self.buf.pop() {
            for _ in 0..width(c, self.opts.mask) {
                echo.extend_from_slice(BS_ERASE);
            }
        }
    }

    /// 1 バイト処理する。行が確定したら返す
    pub fn feed(&mut self, b: u8, echo: &mut Vec<u8>) -> Option<String> {
        if self.after_cr {
            self.after_cr = false;
            if b == b'\n' || b == 0 {
                return None;
            }
        }
        match self.esc {
            1 => {
                self.esc = if b == b'[' || b == b'O' { 2 } else { 0 };
                return None;
            }
            2 => {
                if (0x40..=0x7e).contains(&b) {
                    self.esc = 0;
                }
                return None;
            }
            _ => {}
        }
        match b {
            b'\r' | b'\n' => {
                self.after_cr = b == b'\r';
                self.pending.clear();
                echo.extend_from_slice(b"\r\n");
                return Some(std::mem::take(&mut self.buf));
            }
            0x08 | 0x7f => self.erase_last(echo),
            // Ctrl-U: 行を消す
            0x15 => {
                while !self.buf.is_empty() {
                    self.erase_last(echo);
                }
            }
            0x1b => self.esc = 1,
            0x00..=0x1f => {} // その他の制御文字は無視
            _ => {
                self.pending.push(b);
                match std::str::from_utf8(&self.pending) {
                    Ok(s) => {
                        let mut c = s.chars().next().unwrap();
                        self.pending.clear();
                        if self.opts.normalize {
                            c = to_halfwidth(c);
                        }
                        if c.is_control() {
                            return None;
                        }
                        if self.buf.chars().count() >= self.opts.max_chars {
                            echo.push(0x07); // BEL
                        } else {
                            self.buf.push(c);
                            if self.opts.mask {
                                echo.push(b'*');
                            } else {
                                let mut tmp = [0u8; 4];
                                echo.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
                            }
                        }
                    }
                    // 不正なバイト列は捨てる。途中までなら続きを待つ
                    Err(e) if e.error_len().is_some() || self.pending.len() >= 4 => self.pending.clear(),
                    Err(_) => {}
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(ed: &mut Editor, input: &[u8]) -> (Vec<u8>, Vec<String>) {
        let mut echo = Vec::new();
        let mut lines = Vec::new();
        for &b in input {
            if let Some(l) = ed.feed(b, &mut echo) {
                lines.push(l);
            }
        }
        (echo, lines)
    }

    #[test]
    fn multibyte_split_and_backspace() {
        let mut ed = Editor::new(EditOpts::default());
        let s = "あい".as_bytes();
        let (_, l) = run(&mut ed, &s[..2]);
        assert!(l.is_empty());
        let (echo, _) = run(&mut ed, &s[2..]);
        assert_eq!(echo, "あい".as_bytes());
        // 全角 1 文字の削除は 2 桁ぶん消す
        let (echo, _) = run(&mut ed, b"\x7f");
        assert_eq!(echo, b"\x08 \x08\x08 \x08");
        let (_, l) = run(&mut ed, b"x\r\n");
        assert_eq!(l, vec!["あx"]);
    }

    #[test]
    fn line_endings() {
        let mut ed = Editor::new(EditOpts::default());
        let (_, l) = run(&mut ed, b"a\r\nb\r\0c\nd\r");
        assert_eq!(l, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn mask_limit_normalize_escape() {
        let mut ed = Editor::new(EditOpts { max_chars: 3, mask: true, normalize: false });
        let (echo, l) = run(&mut ed, b"abcd\r");
        assert_eq!(echo, b"***\x07\r\n");
        assert_eq!(l, vec!["abc"]);
        let mut ed = Editor::new(EditOpts::default());
        let (_, l) = run(&mut ed, "ＢＢＳ\x1b[A１\r".as_bytes());
        assert_eq!(l, vec!["BBS1"]);
    }
}
