//! ログイン・新規登録・ログイン後のあいさつ

use crate::auth;
use crate::db::repo::{self, User, LEVEL_MEMBER, LEVEL_SYSOP};
use crate::term::{SResult, SessionError, Term};

use super::util::fmt_time;
use super::Ctx;

pub async fn login(term: &mut Term<'_>, ctx: &Ctx) -> SResult<User> {
    let mut failures = 0;
    loop {
        let id = term.input("ID を入力してください (新規登録=NEW ゲスト=GUEST): ").await?;
        if id.is_empty() {
            continue;
        }
        match id.to_uppercase().as_str() {
            "NEW" => {
                if let Some(u) = register(term, ctx).await? {
                    return Ok(u);
                }
                continue;
            }
            "GUEST" if ctx.cfg.bbs.allow_guest => {
                let u = ctx.db.call(|c| repo::find_user(c, "GUEST")).await?;
                if let Some(u) = u {
                    term.println("ゲストとしてログインします (掲示板の閲覧とチャットができます)").await?;
                    return Ok(u);
                }
            }
            _ => {}
        }
        let pw = term.password("パスワード: ").await?;
        let login_id = id.clone();
        let user = ctx.db.call(move |c| repo::find_user(c, &login_id)).await?;
        let ok = match &user {
            Some(u) if u.level > 0 => auth::verify_async(pw, u.pw_hash.clone()).await,
            _ => false,
        };
        match user {
            Some(u) if ok => {
                if u.banned {
                    term.println("この ID は利用停止中です。SYSOP にお問い合わせください。").await?;
                    return Err(SessionError::Logoff);
                }
                return Ok(u);
            }
            _ => {
                failures += 1;
                tracing::warn!("CH{:02}: ログイン失敗 ID={id}", term.line_no());
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                term.println("ID かパスワードが違います。").await?;
                if failures >= ctx.cfg.limits.login_attempts.max(1) {
                    term.println("規定回数を超えたので切断します。").await?;
                    return Err(SessionError::Logoff);
                }
            }
        }
    }
}

fn valid_login(s: &str) -> bool {
    (3..=12).contains(&s.len())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && !["NEW", "GUEST", "SYSOP", "ALL"].contains(&s.to_uppercase().as_str())
}

/// 新規登録。やめたら None
async fn register(term: &mut Term<'_>, ctx: &Ctx) -> SResult<Option<User>> {
    term.println("\n== 新規登録 ==  (途中でやめるときは Q)").await?;
    let login = loop {
        let id = term.input("希望する ID (英字で始まる英数字 3〜12 文字): ").await?;
        if id.eq_ignore_ascii_case("q") {
            return Ok(None);
        }
        if !valid_login(&id) {
            term.println("その ID は使えません。").await?;
            continue;
        }
        let check = id.clone();
        if ctx.db.call(move |c| repo::find_user(c, &check)).await?.is_some() {
            term.println("その ID はすでに使われています。").await?;
            continue;
        }
        break id;
    };
    let handle = loop {
        let h = term.input_text("ハンドル名 (全角 10 文字まで): ", 20).await?;
        let h = h.trim().to_string();
        if h.eq_ignore_ascii_case("q") {
            return Ok(None);
        }
        if h.is_empty() || super::util::width(&h) > 20 {
            term.println("1〜20 桁 (全角 10 文字) で入力してください。").await?;
            continue;
        }
        break h;
    };
    let pw = loop {
        let p1 = term.password("パスワード (4 文字以上): ").await?;
        if p1.chars().count() < 4 {
            term.println("4 文字以上にしてください。").await?;
            continue;
        }
        let p2 = term.password("もう一度: ").await?;
        if p1 != p2 {
            term.println("一致しません。").await?;
            continue;
        }
        break p1;
    };
    if !term.yes_no(&format!("ID {login} / ハンドル {handle} で登録します。よろしいですか")).await? {
        return Ok(None);
    }
    let hash = auth::hash_async(pw).await?;
    let (l, h) = (login.clone(), handle.clone());
    let user = ctx
        .db
        .call(move |c| {
            // 最初に登録した人を SYSOP にする
            let members: i64 = c.query_row("SELECT count(*) FROM users WHERE level > 0", [], |r| r.get(0))?;
            let level = if members == 0 { LEVEL_SYSOP } else { LEVEL_MEMBER };
            let id = repo::create_user(c, &l, &h, &hash, level)?;
            Ok(repo::get_user(c, id)?.unwrap())
        })
        .await?;
    tracing::info!("新規登録: {} ({})", user.handle, user.login);
    term.println(&format!("登録しました。ようこそ {} さん!", user.handle)).await?;
    if user.level >= LEVEL_SYSOP {
        term.println("(最初の登録者なので SYSOP 権限を付けました)").await?;
    }
    Ok(Some(user))
}

pub async fn greeting(term: &mut Term<'_>, ctx: &Ctx, user: &User, access_id: i64) -> SResult<()> {
    let u = user.clone();
    let (prev, unread_mail, boards) = ctx
        .db
        .call(move |c| {
            Ok((
                repo::previous_login(c, u.id, access_id)?,
                repo::unread_mail_count(c, u.id)?,
                repo::list_boards(c, u.level, u.id)?,
            ))
        })
        .await?;
    let online = ctx.hub.who().len();
    let mut msg = format!("\nこんにちは {} さん。現在 {online} 人が接続中です。\n", user.handle);
    if !user.is_guest() {
        if let Some(p) = prev {
            msg += &format!("前回のログイン: {} (CH{:02})\n", fmt_time(p.login_at), p.line_no);
        }
        if unread_mail > 0 {
            msg += &format!("未読メールが {unread_mail} 通あります。\n");
        }
        let news: Vec<String> =
            boards.iter().filter(|b| b.unread > 0).map(|b| format!("{}({})", b.title, b.unread)).collect();
        if !news.is_empty() {
            msg += &format!("新着記事: {}\n", news.join(" "));
        }
    }
    term.print(&msg).await
}
