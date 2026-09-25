//! 設定ファイル (TOML)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub bbs: BbsConfig,
    pub limits: Limits,
    pub tcp: TcpConfig,
    pub modem: Vec<ModemConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BbsConfig {
    /// ホスト局の名前
    pub name: String,
    /// SQLite データベース
    pub db: PathBuf,
    /// banner.txt / goodbye.txt などを置くディレクトリ
    pub text_dir: PathBuf,
    /// 回線数の上限 (1〜128)
    pub max_lines: u16,
    pub allow_guest: bool,
    /// チャットの発言を DB に記録する
    pub chat_log: bool,
    /// ログファイル
    pub log_file: PathBuf,
    /// ファイルライブラリの保存先
    pub files_dir: PathBuf,
    /// アップロードできる 1 ファイルの上限 (KB)
    pub max_upload_kb: u64,
}

impl Default for BbsConfig {
    fn default() -> Self {
        BbsConfig {
            name: "NULL-BBS".into(),
            db: "null-bbs.db".into(),
            text_dir: "text".into(),
            max_lines: 128,
            allow_guest: true,
            chat_log: false,
            log_file: "null-bbs.log".into(),
            files_dir: "files".into(),
            max_upload_kb: 4096,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// 無操作で切断するまでの秒数
    pub idle_timeout_secs: u64,
    /// 1 回の接続の最大時間 (分)
    pub max_session_mins: u64,
    pub guest_session_mins: u64,
    /// パスワードを間違えられる回数
    pub login_attempts: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { idle_timeout_secs: 300, max_session_mins: 60, guest_session_mins: 15, login_attempts: 3 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TcpConfig {
    /// telnet で待ち受けるアドレス (平文なので既定はローカルのみ)
    pub listen: Vec<String>,
}

impl Default for TcpConfig {
    fn default() -> Self {
        TcpConfig { listen: vec!["127.0.0.1:2323".into()] }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ModemConfig {
    /// 割り当てる回線番号 (1〜max_lines)
    pub line: u16,
    pub path: String,
    /// DTE 速度
    pub baud: u32,
    /// none / hardware / software
    pub flow: String,
    /// 初期化コマンド (1 つずつ OK を待つ)
    pub init: Vec<String>,
    /// manual: RING で ATA / auto: モデムの自動着信 (S0=1) に任せる
    pub answer: String,
    /// manual のとき何回目の RING で応答するか
    pub rings: u32,
    pub connect_timeout_secs: u64,
    /// CONNECT からログイン画面を出すまでの待ち
    pub connect_delay_ms: u64,
    /// キャリア断の検出: dcd / text ("NO CARRIER") / both
    pub carrier: String,
    /// 切断方法: dtr / escape (+++ATH0)
    pub hangup: String,
}

impl Default for ModemConfig {
    fn default() -> Self {
        ModemConfig {
            line: 1,
            path: String::new(),
            baud: 9600,
            flow: "none".into(),
            init: vec!["ATZ".into(), "ATE0V1Q0X4&C1&D2S0=0".into()],
            answer: "manual".into(),
            rings: 1,
            connect_timeout_secs: 60,
            connect_delay_ms: 500,
            carrier: "both".into(),
            hangup: "dtr".into(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("{} を読めません", path.display()))?;
        let mut cfg: Config = toml::from_str(&text).with_context(|| format!("{} の書式が不正です", path.display()))?;
        cfg.bbs.max_lines = cfg.bbs.max_lines.clamp(1, 128);
        for m in &cfg.modem {
            anyhow::ensure!(
                (1..=cfg.bbs.max_lines).contains(&m.line),
                "modem の line は 1〜{} で指定してください: {}",
                cfg.bbs.max_lines,
                m.line
            );
        }
        Ok(cfg)
    }
}
