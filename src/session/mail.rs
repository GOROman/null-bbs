//! メール (BBS 内のユーザー間)

use crate::db::repo::{self, Mail, User};
use crate::term::{SResult, SessionError, Term};

use super::bbs::edit_body;
use super::util::{fmt_short, fmt_time, pad, split_cmd};
use super::Ctx;

pub async fn run(term: &mut Term<'_>, ctx: &Ctx, user: &User) -> SResult<()> {
    if user.is_guest() {
        return term.println("ゲストはメールを使えません。NEW で会員登録してください。").await;
    }
    ctx.hub.set_place(term.line_no(), "メール");
    let mut show_list = true;
    loop {
        let uid = user.id;
        let inbox = ctx.db.call(move |c| repo::inbox(c, uid)).await?;
        if show_list {
            list(term, &inbox).await?;
            show_list = false;
        }
        let a = term.input("\n[メール] L:一覧 R n:読む S:送信 RE n:返信 D n:削除 Q:戻る? ").await?;
        let (cmd, arg) = split_cmd(&a);
        let pick = arg.parse::<usize>().ok().and_then(|n| inbox.get(n.wrapping_sub(1)));
        match cmd.as_str() {
            "" | "Q" => return Ok(()),
            "G" => return Err(SessionError::Logoff),
            "L" => show_list = true,
            "S" | "SEND" => send(term, ctx, user, &arg, None).await?,
            "R" | "READ" => match pick {
                Some(m) => read(term, ctx, m).await?,
                None => term.println("R の後に番号を入れてください").await?,
            },
            n if n.parse::<usize>().is_ok() => match inbox.get(n.parse::<usize>().unwrap().wrapping_sub(1)) {
                Some(m) => read(term, ctx, m).await?,
                None => term.println("その番号はありません").await?,
            },
            "RE" => match pick {
                Some(m) => {
                    let subject = if m.subject.starts_with("Re:") { m.subject.clone() } else { format!("Re: {}", m.subject) };
                    send(term, ctx, user, &m.from_login.clone(), Some(subject)).await?
                }
                None => term.println("RE の後に番号を入れてください").await?,
            },
            "D" | "DEL" => match pick {
                Some(m) => {
                    if term.yes_no(&format!("「{}」を削除しますか", m.subject)).await? {
                        let id = m.id;
                        ctx.db.call(move |c| repo::delete_mail(c, id, uid)).await?;
                        term.println("削除しました。").await?;
                    }
                }
                None => term.println("D の後に番号を入れてください").await?,
            },
            _ => term.println("コマンドが違います").await?,
        }
    }
}

async fn list(term: &mut Term<'_>, inbox: &[Mail]) -> SResult<()> {
    if inbox.is_empty() {
        return term.println("\nメールはありません。").await;
    }
    let mut lines = vec!["\n No    日付         差出人        件名".to_string()];
    for (i, m) in inbox.iter().enumerate() {
        let mark = if m.read { ' ' } else { '*' };
        lines.push(format!("{mark}{:>3}  {}  {} {}", i + 1, fmt_short(m.created_at), pad(&m.from_handle, 12), m.subject));
    }
    lines.push("(* は未読)".into());
    term.page(&lines).await.map(|_| ())
}

async fn read(term: &mut Term<'_>, ctx: &Ctx, m: &Mail) -> SResult<()> {
    let text = format!(
        "\n差出人: {} ({})\n日付  : {}\n件名  : {}\n{}\n{}\n{}",
        m.from_handle,
        m.from_login,
        fmt_time(m.created_at),
        m.subject,
        "-".repeat(60),
        m.body,
        "-".repeat(60)
    );
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    term.page(&lines).await?;
    let id = m.id;
    ctx.db.call(move |c| repo::mark_mail_read(c, id)).await?;
    Ok(())
}

async fn send(term: &mut Term<'_>, ctx: &Ctx, user: &User, to: &str, subject: Option<String>) -> SResult<()> {
    let to = if to.is_empty() { term.input("宛先の ID: ").await? } else { to.to_string() };
    if to.is_empty() {
        return Ok(());
    }
    let t = to.clone();
    let Some(dest) = ctx.db.call(move |c| repo::find_user(c, &t)).await?.filter(|u| !u.is_guest()) else {
        return term.println(&format!("{to} という会員はいません。")).await;
    };
    let subject = match subject {
        Some(s) => s,
        None => {
            let s = term.input_text("件名: ", 40).await?.trim().to_string();
            if s.is_empty() {
                return term.println("中止しました。").await;
            }
            s
        }
    };
    term.println(&format!("宛先: {} ({})  件名: {subject}", dest.handle, dest.login)).await?;
    let Some(body) = edit_body(term).await? else { return Ok(()) };
    if !term.yes_no("送信しますか").await? {
        return term.println("中止しました。").await;
    }
    let (from, to_id) = (user.id, dest.id);
    ctx.db.call(move |c| repo::send_mail(c, from, to_id, &subject, &body)).await?;
    // 相手が接続中なら知らせる
    let _ = ctx.hub.telegram(&dest.login, "メール", &format!("{} さんから新しいメールが届きました", user.handle));
    term.println("送信しました。").await
}
