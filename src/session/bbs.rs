//! 掲示板

use crate::db::repo::{self, Board, Post, User, LEVEL_SYSOP};
use crate::term::{SResult, SessionError, Term};

use super::util::{fmt_short, fmt_time, pad, split_cmd};
use super::Ctx;

/// 本文の入力。中止したら None
pub async fn edit_body(term: &mut Term<'_>) -> SResult<Option<String>> {
    term.println("本文を入力してください。「.」だけの行で終了、/A で中止、/L で見直し。").await?;
    let mut lines: Vec<String> = Vec::new();
    loop {
        let l = term.input_text(&format!("{:>3}: ", lines.len() + 1), 200).await?;
        match l.trim() {
            "." => break,
            "/A" | "/a" => {
                term.println("中止しました。").await?;
                return Ok(None);
            }
            "/L" | "/l" => {
                let text: Vec<String> = lines.iter().enumerate().map(|(i, l)| format!("{:>3}: {l}", i + 1)).collect();
                term.page(&text).await?;
            }
            _ => {
                lines.push(l);
                if lines.len() >= 300 {
                    term.println("300 行に達したので入力を終わります。").await?;
                    break;
                }
            }
        }
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        term.println("本文が空なので中止しました。").await?;
        return Ok(None);
    }
    Ok(Some(lines.join("\n")))
}

pub async fn run(term: &mut Term<'_>, ctx: &Ctx, user: &User) -> SResult<()> {
    loop {
        ctx.hub.set_place(term.line_no(), "掲示板");
        let (level, uid) = (user.level, user.id);
        let boards = ctx.db.call(move |c| repo::list_boards(c, level, uid)).await?;
        let mut text = vec!["\n No  ボード                  記事  未読".to_string()];
        for (i, b) in boards.iter().enumerate() {
            let unread = if user.is_guest() { "-".to_string() } else { b.unread.to_string() };
            text.push(format!("{:>3}  {} {:>5} {:>5}", i + 1, pad(&b.title, 22), b.count, unread));
        }
        term.page(&text).await?;
        let a = term.input("ボード番号 (Q:戻る G:終了)? ").await?;
        let (cmd, _) = split_cmd(&a);
        match cmd.as_str() {
            "" | "Q" => return Ok(()),
            "G" => return Err(SessionError::Logoff),
            _ => {}
        }
        let chosen = cmd
            .parse::<usize>()
            .ok()
            .and_then(|n| boards.get(n.wrapping_sub(1)))
            .or_else(|| boards.iter().find(|b| b.name.eq_ignore_ascii_case(&cmd)));
        match chosen {
            Some(b) => board(term, ctx, user, b.clone()).await?,
            None => term.println("そのボードはありません。").await?,
        }
    }
}

fn show_post(b: &Board, p: &Post) -> String {
    let re = p.parent_seq.map(|s| format!(" (#{s} へのコメント)")).unwrap_or_default();
    format!(
        "\n#{} [{}] {}  {} ({}){re}\n題名: {}\n{}\n{}\n{}\n",
        p.seq,
        b.title,
        fmt_time(p.created_at),
        p.author_handle,
        p.author_login,
        p.title,
        "-".repeat(60),
        p.body,
        "-".repeat(60)
    )
}

async fn board(term: &mut Term<'_>, ctx: &Ctx, user: &User, b: Board) -> SResult<()> {
    ctx.hub.set_place(term.line_no(), &format!("掲示板 {}", b.title));
    term.println(&format!("\n== {} ==  {}", b.title, b.description)).await?;
    loop {
        let a = term.input(&format!("\n[{}] L:一覧 R n:読む N:未読 W:書く RE n:返信 Q:戻る? ", b.title)).await?;
        let (cmd, arg) = split_cmd(&a);
        let num = arg.parse::<i64>().ok();
        match cmd.as_str() {
            "Q" | "" => return Ok(()),
            "G" => return Err(SessionError::Logoff),
            "?" => {
                term.println("L:一覧 R 番号:読む N:未読を順に読む W:書く RE 番号:返信 D 番号:削除 Q:戻る G:終了").await?
            }
            "L" => {
                let id = b.id;
                let posts = ctx.db.call(move |c| repo::list_posts(c, id)).await?;
                if posts.is_empty() {
                    term.println("記事はまだありません。").await?;
                    continue;
                }
                let lines: Vec<String> = posts
                    .iter()
                    .map(|p| format!("#{:<4} {} {} {}", p.seq, fmt_short(p.created_at), pad(&p.author_handle, 12), p.title))
                    .collect();
                term.page(&lines).await?;
            }
            "R" | "READ" => match num {
                Some(n) => read(term, ctx, user, &b, n).await?,
                None => term.println("R の後に記事番号を入れてください (例: R 3)").await?,
            },
            n if n.parse::<i64>().is_ok() => read(term, ctx, user, &b, n.parse().unwrap()).await?,
            "N" | "NEW" => unread(term, ctx, user, &b).await?,
            "W" | "WRITE" => write(term, ctx, user, &b, None).await?,
            "RE" => match num {
                Some(n) => write(term, ctx, user, &b, Some(n)).await?,
                None => term.println("RE の後に記事番号を入れてください (例: RE 3)").await?,
            },
            "D" | "DEL" => match num {
                Some(n) => delete(term, ctx, user, &b, n).await?,
                None => term.println("D の後に記事番号を入れてください").await?,
            },
            _ => term.println("コマンドが違います (? でヘルプ)").await?,
        }
    }
}

async fn read(term: &mut Term<'_>, ctx: &Ctx, user: &User, b: &Board, seq: i64) -> SResult<()> {
    let id = b.id;
    let post = ctx.db.call(move |c| repo::get_post(c, id, seq)).await?;
    let Some(p) = post else {
        return term.println(&format!("#{seq} はありません。")).await;
    };
    let lines: Vec<String> = show_post(b, &p).lines().map(str::to_string).collect();
    term.page(&lines).await?;
    if !user.is_guest() {
        let uid = user.id;
        ctx.db.call(move |c| repo::mark_read(c, uid, id, seq)).await?;
    }
    Ok(())
}

async fn unread(term: &mut Term<'_>, ctx: &Ctx, user: &User, b: &Board) -> SResult<()> {
    let (id, uid, guest) = (b.id, user.id, user.is_guest());
    let posts = ctx
        .db
        .call(move |c| {
            let mark = if guest { 0 } else { repo::read_mark(c, uid, id)? };
            repo::posts_after(c, id, mark)
        })
        .await?;
    if posts.is_empty() {
        return term.println("未読の記事はありません。").await;
    }
    let total = posts.len();
    for (i, p) in posts.iter().enumerate() {
        let lines: Vec<String> = show_post(b, p).lines().map(str::to_string).collect();
        if !term.page(&lines).await? {
            return Ok(());
        }
        if !guest {
            let seq = p.seq;
            ctx.db.call(move |c| repo::mark_read(c, uid, id, seq)).await?;
        }
        if i + 1 < total {
            let a = term.input(&format!("-- 未読あと {} 件 (Enter:次 Q:やめる) --", total - i - 1)).await?;
            if a.eq_ignore_ascii_case("q") {
                return Ok(());
            }
        }
    }
    term.println("未読の記事はこれで全部です。").await
}

async fn write(term: &mut Term<'_>, ctx: &Ctx, user: &User, b: &Board, reply_to: Option<i64>) -> SResult<()> {
    if user.is_guest() || user.level < b.write_level {
        return term.println("このボードには書き込めません。").await;
    }
    let mut default_title = String::new();
    if let Some(seq) = reply_to {
        let id = b.id;
        match ctx.db.call(move |c| repo::get_post(c, id, seq)).await? {
            Some(p) => {
                default_title =
                    if p.title.starts_with("Re:") { p.title.clone() } else { format!("Re: {}", p.title) };
            }
            None => return term.println(&format!("#{seq} はありません。")).await,
        }
    }
    let prompt = if default_title.is_empty() { "題名: ".to_string() } else { format!("題名 (Enter で「{default_title}」): ") };
    let mut title = term.input_text(&prompt, 40).await?.trim().to_string();
    if title.is_empty() {
        title = default_title;
    }
    if title.is_empty() {
        return term.println("中止しました。").await;
    }
    let Some(body) = edit_body(term).await? else { return Ok(()) };
    if !term.yes_no("書き込みますか").await? {
        return term.println("中止しました。").await;
    }
    let (id, u) = (b.id, user.clone());
    let seq = ctx.db.call(move |c| repo::add_post(c, id, reply_to, &u, &title, &body)).await?;
    let uid = user.id;
    ctx.db.call(move |c| repo::mark_read(c, uid, id, seq)).await?;
    tracing::info!("CH{:02}: {} が {} に #{seq} を書き込み", term.line_no(), user.login, b.name);
    term.println(&format!("#{seq} として書き込みました。")).await
}

async fn delete(term: &mut Term<'_>, ctx: &Ctx, user: &User, b: &Board, seq: i64) -> SResult<()> {
    let id = b.id;
    let Some(p) = ctx.db.call(move |c| repo::get_post(c, id, seq)).await? else {
        return term.println(&format!("#{seq} はありません。")).await;
    };
    if !(p.author_login.eq_ignore_ascii_case(&user.login) || user.level >= LEVEL_SYSOP) {
        return term.println("自分の記事しか削除できません。").await;
    }
    if term.yes_no(&format!("#{seq}「{}」を削除しますか", p.title)).await? {
        ctx.db.call(move |c| repo::delete_post(c, id, seq)).await?;
        term.println("削除しました。").await?;
    }
    Ok(())
}
