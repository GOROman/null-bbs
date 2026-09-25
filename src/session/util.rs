//! セッションで使う小物

use chrono::{Local, TimeZone};
use unicode_width::UnicodeWidthStr;

use crate::config::Config;

/// UNIX 秒を「2026/09/25 14:10」に
pub fn fmt_time(ts: i64) -> String {
    Local.timestamp_opt(ts, 0).single().map(|t| t.format("%Y/%m/%d %H:%M").to_string()).unwrap_or_default()
}

/// UNIX 秒を「09/25 14:10」に
pub fn fmt_short(ts: i64) -> String {
    Local.timestamp_opt(ts, 0).single().map(|t| t.format("%m/%d %H:%M").to_string()).unwrap_or_default()
}

/// 表示幅 (全角 = 2) で右を空白で埋める。長ければ切り詰める
pub fn pad(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push_str(&" ".repeat(width - w));
    out
}

pub fn width(s: &str) -> usize {
    s.width()
}

/// 「コマンド 引数」に分ける。コマンドは大文字に
pub fn split_cmd(s: &str) -> (String, String) {
    let s = s.trim();
    match s.split_once(char::is_whitespace) {
        Some((c, rest)) => (c.to_uppercase(), rest.trim().to_string()),
        None => {
            // 「R12」のように続けて書いた番号も受け付ける
            let split = s.find(|c: char| c.is_ascii_digit()).filter(|&i| i > 0);
            match split {
                Some(i) if s[..i].chars().all(|c| c.is_ascii_alphabetic()) => {
                    (s[..i].to_uppercase(), s[i..].to_string())
                }
                _ => (s.to_uppercase(), String::new()),
            }
        }
    }
}

/// text_dir のファイルを読む ({name} は BBS 名に置き換える)
pub fn load_text(cfg: &Config, file: &str) -> Option<String> {
    let text = std::fs::read_to_string(cfg.bbs.text_dir.join(file)).ok()?;
    Some(text.replace("{name}", &cfg.bbs.name))
}

pub fn default_banner(name: &str) -> String {
    let line = "=".repeat(width(name) + 8);
    format!("\n{line}\n    {name}\n{line}\nパソコン通信ホスト局へようこそ。\n\n")
}

/// 経過時間を「1:23:45」や「12:34」に
pub fn fmt_elapsed(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands() {
        assert_eq!(split_cmd(" re 12 "), ("RE".into(), "12".into()));
        assert_eq!(split_cmd("R12"), ("R".into(), "12".into()));
        assert_eq!(split_cmd("12"), ("12".into(), "".into()));
        assert_eq!(split_cmd("t goroman やあ"), ("T".into(), "goroman やあ".into()));
    }

    #[test]
    fn padding() {
        assert_eq!(pad("あいう", 5), "あい ");
        assert_eq!(pad("ab", 4), "ab  ");
    }
}
