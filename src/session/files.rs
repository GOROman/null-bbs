//! ファイルライブラリ (XMODEM / XMODEM-1K / YMODEM で送受信)

use std::path::{Path, PathBuf};

use crate::db::repo::{self, FileEntry, User, LEVEL_SYSOP};
use crate::term::{SResult, SessionError, Term};
use crate::transfer::{Outcome, Protocol, Transfer};

use super::util::{fmt_short, pad, split_cmd};
use super::Ctx;

fn human(n: i64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1}M", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{:.1}K", n as f64 / 1024.0)
    } else {
        format!("{n}B")
    }
}

/// パスの区切りや制御文字を取り除いたファイル名
fn safe_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next()?.trim();
    let base: String = base.chars().filter(|c| !c.is_control()).collect();
    (!base.is_empty() && base != "." && base != ".." && !base.starts_with('.')).then_some(base)
}

/// files_dir 内で重複しないファイル名
fn unique_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..).map(|i| format!("{stem}_{i}{ext}")).find(|n| !dir.join(n).exists()).unwrap()
}

pub async fn run(term: &mut Term<'_>, ctx: &Ctx, user: &User) -> SResult<()> {
    ctx.hub.set_place(term.line_no(), "ファイル");
    std::fs::create_dir_all(&ctx.cfg.bbs.files_dir).map_err(anyhow::Error::from)?;
    let mut show = true;
    loop {
        let files = ctx.db.call(|c| repo::list_files(c)).await?;
        if show {
            list(term, &files).await?;
            show = false;
        }
        let a = term.input("\n[ファイル] L:一覧 D n:ダウンロード U:アップロード I n:説明 Q:戻る? ").await?;
        let (cmd, arg) = split_cmd(&a);
        let pick = |s: &str| s.parse::<usize>().ok().and_then(|n| files.get(n.wrapping_sub(1))).cloned();
        match cmd.as_str() {
            "" | "Q" => return Ok(()),
            "G" => return Err(SessionError::Logoff),
            "L" => show = true,
            "U" | "UP" => upload(term, ctx, user).await?,
            "D" | "DOWN" => match pick(&arg) {
                Some(f) => download(term, ctx, &f).await?,
                None => term.println("D の後にファイル番号を入れてください (例: D 1)").await?,
            },
            n if n.parse::<usize>().is_ok() => match pick(n) {
                Some(f) => download(term, ctx, &f).await?,
                None => term.println("その番号はありません").await?,
            },
            "I" | "INFO" => match pick(&arg) {
                Some(f) => {
                    term.println(&format!(
                        "\n{}  {} バイト\n登録: {} {} ({})  ダウンロード {} 回\n説明: {}",
                        f.name,
                        f.size,
                        super::util::fmt_time(f.created_at),
                        f.uploader_handle,
                        f.uploader_login,
                        f.downloads,
                        f.description
                    ))
                    .await?
                }
                None => term.println("I の後にファイル番号を入れてください").await?,
            },
            "DEL" => match pick(&arg) {
                Some(f) if f.uploader_login.eq_ignore_ascii_case(&user.login) || user.level >= LEVEL_SYSOP => {
                    if term.yes_no(&format!("{} を削除しますか", f.name)).await? {
                        let _ = std::fs::remove_file(ctx.cfg.bbs.files_dir.join(&f.name));
                        let id = f.id;
                        ctx.db.call(move |c| repo::delete_file(c, id)).await?;
                        term.println("削除しました。").await?;
                    }
                }
                Some(_) => term.println("自分が登録したファイルしか削除できません。").await?,
                None => term.println("DEL の後にファイル番号を入れてください").await?,
            },
            "?" => term.println("L:一覧 D 番号:ダウンロード U:アップロード I 番号:説明 DEL 番号:削除 Q:戻る").await?,
            _ => term.println("コマンドが違います (? でヘルプ)").await?,
        }
    }
}

async fn list(term: &mut Term<'_>, files: &[FileEntry]) -> SResult<()> {
    if files.is_empty() {
        return term.println("\nファイルはまだありません。U でアップロードできます。").await;
    }
    let mut lines = vec!["\n No  ファイル名             サイズ  日付         登録者       説明".to_string()];
    for (i, f) in files.iter().enumerate() {
        lines.push(format!(
            "{:>3}  {} {:>7}  {} {} {}",
            i + 1,
            pad(&f.name, 20),
            human(f.size),
            fmt_short(f.created_at),
            pad(&f.uploader_handle, 12),
            f.description
        ));
    }
    term.page(&lines).await.map(|_| ())
}

async fn choose_protocol(term: &mut Term<'_>) -> SResult<Option<Protocol>> {
    let a = term.input("プロトコル 1.XMODEM 2.XMODEM-1K 3.YMODEM (Enter=3 Q=中止)? ").await?;
    Ok(match a.to_uppercase().as_str() {
        "1" | "X" | "XMODEM" => Some(Protocol::Xmodem),
        "2" | "1K" | "XMODEM-1K" => Some(Protocol::Xmodem1k),
        "" | "3" | "Y" | "YMODEM" => Some(Protocol::Ymodem),
        _ => None,
    })
}

async fn download(term: &mut Term<'_>, ctx: &Ctx, f: &FileEntry) -> SResult<()> {
    let path = ctx.cfg.bbs.files_dir.join(&f.name);
    if !path.is_file() {
        return term.println("ファイルが見つかりません。SYSOP にお知らせください。").await;
    }
    let Some(proto) = choose_protocol(term).await? else { return Ok(()) };
    term.println(&format!(
        "{} で {} ({} バイト) を送ります。通信ソフトで受信を始めてください。\n(60 秒待ちます。中止は Ctrl-X を数回)",
        proto.label(),
        f.name,
        f.size
    ))
    .await?;
    ctx.hub.set_place(term.line_no(), &format!("DL {}", f.name));
    let mut t = Transfer::send(proto, vec![path], std::time::Instant::now())?;
    let outcome = term.transfer(&mut t, Vec::new(), None).await?;
    ctx.hub.set_place(term.line_no(), "ファイル");
    match outcome {
        Outcome::Done(_) => {
            let id = f.id;
            ctx.db.call(move |c| repo::count_download(c, id)).await?;
            tracing::info!("CH{:02}: {} をダウンロード ({})", term.line_no(), f.name, proto.label());
            term.println("\n送信が完了しました。").await
        }
        Outcome::Failed(m) => term.println(&format!("\n送信できませんでした: {m}")).await,
        Outcome::Running => Ok(()),
    }
}

async fn upload(term: &mut Term<'_>, ctx: &Ctx, user: &User) -> SResult<()> {
    if user.is_guest() {
        return term.println("ゲストはアップロードできません。NEW で会員登録してください。").await;
    }
    let Some(proto) = choose_protocol(term).await? else { return Ok(()) };
    // 受信はいったん回線ごとの作業ディレクトリに置く
    let work = ctx.cfg.bbs.files_dir.join(".incoming").join(format!("CH{:02}", term.line_no()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(anyhow::Error::from)?;
    let dest: PathBuf = if proto == Protocol::Ymodem {
        work.clone()
    } else {
        let name = term.input_text("ファイル名 (XMODEM は名前が送られないので入力してください): ", 40).await?;
        match safe_name(&name) {
            Some(n) => work.join(n),
            None => return term.println("中止しました。").await,
        }
    };
    let limit = ctx.cfg.bbs.max_upload_kb * 1024;
    term.println(&format!(
        "{} で受信します。通信ソフトから送信を始めてください (上限 {} KB)。\n(60 秒待ちます。中止は Ctrl-X を数回)",
        proto.label(),
        ctx.cfg.bbs.max_upload_kb
    ))
    .await?;
    ctx.hub.set_place(term.line_no(), "UP");
    let (mut t, first) = Transfer::recv(proto, dest, std::time::Instant::now())?;
    let outcome = term.transfer(&mut t, first, Some(limit)).await;
    ctx.hub.set_place(term.line_no(), "ファイル");
    let result = register_uploads(term, ctx, user, &work, outcome).await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// 受信したファイルに説明を付けてライブラリに登録する
async fn register_uploads(
    term: &mut Term<'_>,
    ctx: &Ctx,
    user: &User,
    work: &Path,
    outcome: SResult<Outcome>,
) -> SResult<()> {
    match outcome? {
        Outcome::Done(_) => {}
        Outcome::Failed(m) => return term.println(&format!("\n受信できませんでした: {m}")).await,
        Outcome::Running => return Ok(()),
    }
    let mut received: Vec<PathBuf> = std::fs::read_dir(work)
        .map_err(anyhow::Error::from)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    received.sort();
    term.println(&format!("\n{} 個のファイルを受信しました。", received.len())).await?;
    let dir = ctx.cfg.bbs.files_dir.clone();
    for p in received {
        let orig = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(orig) = safe_name(&orig) else { continue };
        let size = std::fs::metadata(&p).map(|m| m.len() as i64).unwrap_or(0);
        let desc = term.input_text(&format!("{orig} ({size} バイト) の説明: "), 60).await?;
        let name = unique_name(&dir, &orig);
        std::fs::rename(&p, dir.join(&name)).map_err(anyhow::Error::from)?;
        let (n, d, u) = (name.clone(), desc.trim().to_string(), user.clone());
        ctx.db.call(move |c| repo::add_file(c, &n, size, &d, &u)).await?;
        tracing::info!("CH{:02}: {} が {name} をアップロード ({size} バイト)", term.line_no(), user.login);
        term.println(&format!("{name} として登録しました。")).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(safe_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(safe_name("C:\\a\\b.txt").as_deref(), Some("b.txt"));
        assert_eq!(safe_name(".hidden"), None);
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "x").unwrap();
        assert_eq!(unique_name(d.path(), "a.txt"), "a_1.txt");
        assert_eq!(unique_name(d.path(), "b.txt"), "b.txt");
    }
}
