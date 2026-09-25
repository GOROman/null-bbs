//! NULL BBS: パソコン通信ホスト局

mod app;
mod auth;
mod config;
mod db;
mod hub;
mod line;
mod logbuf;
mod session;
mod term;
mod transfer;
mod tui;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use config::Config;
use db::{repo, Db};

#[derive(Parser)]
#[command(version, about = "NULL BBS: パソコン通信ホスト局 (モデム / TCP、最大 128 回線)")]
struct Args {
    /// 設定ファイル
    #[arg(short, long, global = true, default_value = "null-bbs.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// ホスト局を起動する (既定)
    Run {
        /// 管理画面を出さずに動かす (ログは標準エラーへ)
        #[arg(long)]
        headless: bool,
    },
    /// 会員を追加する (パスワードは標準入力から読む)
    Useradd {
        login: String,
        handle: String,
        /// SYSOP 権限を付ける
        #[arg(long)]
        sysop: bool,
    },
    /// 会員の一覧
    Users,
    /// ボードの管理
    Board {
        #[command(subcommand)]
        cmd: BoardCmd,
    },
}

#[derive(Subcommand)]
enum BoardCmd {
    /// ボードを追加する
    Add {
        name: String,
        title: String,
        #[arg(short, long, default_value = "")]
        description: String,
        /// 書き込みに必要なレベル (10: 会員 / 100: SYSOP)
        #[arg(short, long, default_value_t = repo::LEVEL_MEMBER)]
        write_level: i64,
    },
    /// ボードの一覧
    List,
}

fn read_password() -> Result<String> {
    eprint!("パスワード: ");
    std::io::stderr().flush()?;
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    let s = s.trim_end_matches(['\r', '\n']).to_string();
    if s.chars().count() < 4 {
        bail!("パスワードは 4 文字以上にしてください");
    }
    Ok(s)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let cfg = Config::load(&args.config)?;
    match args.command.unwrap_or(Command::Run { headless: false }) {
        Command::Run { headless } => {
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
            rt.block_on(app::run(cfg, headless))
        }
        Command::Useradd { login, handle, sysop } => {
            let pw = read_password()?;
            let hash = auth::hash(&pw)?;
            let db = Db::open(&cfg.bbs.db)?;
            let level = if sysop { repo::LEVEL_SYSOP } else { repo::LEVEL_MEMBER };
            let id = db.call_blocking(move |c| repo::create_user(c, &login, &handle, &hash, level))?;
            println!("会員を追加しました (id {id})");
            Ok(())
        }
        Command::Users => {
            let db = Db::open(&cfg.bbs.db)?;
            for u in db.call_blocking(|c| repo::list_users(c))? {
                println!(
                    "{:>4} {:<12} {:<16} level {:>3}{}",
                    u.id,
                    u.login,
                    u.handle,
                    u.level,
                    if u.banned { " (停止中)" } else { "" }
                );
            }
            Ok(())
        }
        Command::Board { cmd } => {
            let db = Db::open(&cfg.bbs.db)?;
            match cmd {
                BoardCmd::Add { name, title, description, write_level } => {
                    db.call_blocking(move |c| {
                        repo::add_board(c, &name.to_uppercase(), &title, &description, 0, write_level)
                    })?;
                    println!("ボードを追加しました");
                }
                BoardCmd::List => {
                    for b in db.call_blocking(|c| repo::list_boards(c, repo::LEVEL_SYSOP, 0))? {
                        println!("{:<10} {:<20} 記事 {:>4}  書込レベル {}", b.name, b.title, b.count, b.write_level);
                    }
                }
            }
            Ok(())
        }
    }
}
