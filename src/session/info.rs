//! WHO / LOG / 登録情報 / 電報

use chrono::Local;

use crate::auth;
use crate::db::repo::{self, User};
use crate::term::{SResult, Term};

use super::util::{fmt_elapsed, fmt_short, fmt_time, pad, split_cmd};
use super::Ctx;

pub async fn who(term: &mut Term<'_>, ctx: &Ctx) -> SResult<()> {
    let who = ctx.hub.who();
    let mut lines = vec![
        format!("\n== 在室者 {}人 ==", who.len()),
        "CH  ID           ハンドル        接続          場所              時間".to_string(),
    ];
    for v in &who {
        let via = match v.kind {
            Some(crate::line::LineKind::Modem) if !v.speed.is_empty() => v.speed.clone(),
            Some(k) => k.label().to_string(),
            None => String::new(),
        };
        let elapsed = v.connected_at.map(|t| (Local::now() - t).num_seconds()).unwrap_or(0);
        lines.push(format!(
            "{:02}  {} {} {} {} {}",
            v.no,
            pad(&v.login, 12),
            pad(&v.handle, 15),
            pad(&via, 13),
            pad(&v.place, 17),
            fmt_elapsed(elapsed)
        ));
    }
    term.page(&lines).await.map(|_| ())
}

pub async fn log(term: &mut Term<'_>, ctx: &Ctx) -> SResult<()> {
    let rows = ctx.db.call(|c| repo::recent_access(c, 20)).await?;
    let mut lines = vec!["\n== 足跡 (最近の 20 件) ==".to_string(), "日時         CH  ハンドル            利用時間  終了".into()];
    for a in rows {
        let (dur, reason) = match a.logout_at {
            Some(out) => (fmt_elapsed(out - a.login_at), a.reason.unwrap_or_default()),
            None => ("接続中".into(), String::new()),
        };
        lines.push(format!(
            "{}  {:02}  {} {:>8}  {}",
            fmt_short(a.login_at),
            a.line_no,
            pad(&format!("{}({})", a.handle, a.login), 19),
            dur,
            reason
        ));
    }
    term.page(&lines).await.map(|_| ())
}

pub async fn profile(term: &mut Term<'_>, ctx: &Ctx, user: &mut User) -> SResult<()> {
    if user.is_guest() {
        return term.println("ゲストは登録情報を変更できません。").await;
    }
    ctx.hub.set_place(term.line_no(), "登録情報");
    loop {
        term.println(&format!(
            "\n== 登録情報 ==\n会員番号 : {}\nID       : {}\nハンドル : {}\n登録日   : {}\nログイン : {} 回\n画面行数 : {}\n自己紹介 : {}\n\n1.ハンドル変更 2.パスワード変更 3.画面行数 4.自己紹介 Q.戻る",
            user.id,
            user.login,
            user.handle,
            fmt_time(user.created_at),
            user.login_count + 1,
            user.screen_rows,
            user.profile
        ))
        .await?;
        let a = term.input("番号? ").await?;
        let id = user.id;
        match a.to_uppercase().as_str() {
            "" | "Q" => return Ok(()),
            "1" => {
                let h = term.input_text("新しいハンドル: ", 20).await?.trim().to_string();
                if !h.is_empty() {
                    let hh = h.clone();
                    ctx.db.call(move |c| repo::set_handle(c, id, &hh)).await?;
                    user.handle = h;
                    ctx.hub.set_user(term.line_no(), user.id, &user.login, &user.handle);
                }
            }
            "2" => {
                let cur = term.password("今のパスワード: ").await?;
                if !auth::verify_async(cur, user.pw_hash.clone()).await {
                    term.println("パスワードが違います。").await?;
                    continue;
                }
                let p1 = term.password("新しいパスワード: ").await?;
                let p2 = term.password("もう一度: ").await?;
                if p1 != p2 || p1.chars().count() < 4 {
                    term.println("一致しないか、4 文字未満です。").await?;
                    continue;
                }
                let hash = auth::hash_async(p1).await?;
                user.pw_hash = hash.clone();
                ctx.db.call(move |c| repo::set_password(c, id, &hash)).await?;
                term.println("変更しました。").await?;
            }
            "3" => {
                if let Ok(n) = term.input("1 画面の行数 (8〜100): ").await?.parse::<i64>() {
                    let n = n.clamp(8, 100);
                    ctx.db.call(move |c| repo::set_screen_rows(c, id, n)).await?;
                    user.screen_rows = n;
                    term.rows = n as usize;
                }
            }
            "4" => {
                let p = term.input_text("自己紹介 (1 行): ", 60).await?;
                let pp = p.clone();
                ctx.db.call(move |c| repo::set_profile(c, id, &pp)).await?;
                user.profile = p;
            }
            _ => {}
        }
    }
}

/// 電報: 「T 相手 本文」。相手は回線番号か ID
pub async fn telegram(term: &mut Term<'_>, ctx: &Ctx, user: &User, arg: &str) -> SResult<()> {
    let (mut target, mut text) = split_cmd(arg);
    if target.is_empty() {
        target = term.input("相手 (回線番号か ID): ").await?;
        if target.is_empty() {
            return Ok(());
        }
    }
    if text.is_empty() {
        text = term.input_text("本文: ", 100).await?;
        if text.trim().is_empty() {
            return Ok(());
        }
    }
    let from = format!("{}(CH{:02})", user.handle, term.line_no());
    match ctx.hub.telegram(&target, &from, text.trim()) {
        Ok(to) => term.println(&format!("{to} に送りました。")).await,
        Err(e) => term.println(&format!("{e}")).await,
    }
}
