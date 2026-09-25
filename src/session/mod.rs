//! 利用者のセッション (接続からログオフまで)

mod bbs;
mod chat;
mod files;
mod info;
mod login;
mod mail;
mod menu;
mod sysop;
pub mod util;

use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::db::{repo, Db};
use crate::hub::Hub;
use crate::line::Conn;
use crate::term::{SResult, SessionError, Term};

/// セッションが使う共有物
pub struct Ctx {
    pub hub: Arc<Hub>,
    pub db: Db,
    pub cfg: Arc<Config>,
}

/// 1 回の接続を処理する。回線の切断は呼び出し側で行う
pub async fn run(conn: &mut Conn, ctx: &Ctx) {
    let info = conn.info.clone();
    let no = info.no;
    let (notices, monitor) = ctx.hub.attach(info.clone());
    let idle = Duration::from_secs(ctx.cfg.limits.idle_timeout_secs.max(30));
    let mut term = Term::new(conn, notices, monitor, idle);
    let mut access = None;
    let mut who = String::from("(未ログイン)");
    let result = session(&mut term, ctx, &mut access, &mut who).await;
    let reason = match &result {
        Ok(()) | Err(SessionError::Logoff) => "ログオフ",
        Err(e) => e.reason(),
    };
    match &result {
        Ok(()) | Err(SessionError::Logoff) => {
            let bye = util::load_text(&ctx.cfg, "goodbye.txt")
                .unwrap_or_else(|| format!("\nご利用ありがとうございました。{} でした。\n", ctx.cfg.bbs.name));
            let _ = term.print(&bye).await;
        }
        Err(SessionError::Disconnected) => {}
        Err(e) => {
            let _ = term.println(&format!("\n\n*** {e} ***")).await;
        }
    }
    if let Some(id) = access {
        let reason = reason.to_string();
        let _ = ctx.db.call(move |c| repo::log_logout(c, id, &reason)).await;
    }
    tracing::info!("CH{no:02}: {who} 終了 ({reason})");
    ctx.hub.detach(no);
}

async fn session(term: &mut Term<'_>, ctx: &Ctx, access: &mut Option<i64>, who: &mut String) -> SResult<()> {
    let banner = util::load_text(&ctx.cfg, "banner.txt").unwrap_or_else(|| util::default_banner(&ctx.cfg.bbs.name));
    term.print(&banner).await?;
    let mut user = login::login(term, ctx).await?;
    let no = term.line_no();
    let kind = term.info().kind.label().to_string();
    let peer = term.info().peer();
    let u = user.clone();
    let id = ctx.db.call(move |c| {
        repo::record_login(c, u.id)?;
        repo::log_login(c, &u, no, &kind, &peer)
    })
    .await?;
    *access = Some(id);
    *who = format!("{} ({})", user.handle, user.login);
    ctx.hub.set_user(no, user.id, &user.login, &user.handle);
    tracing::info!("CH{no:02}: {who} がログイン");

    let mins = if user.is_guest() { ctx.cfg.limits.guest_session_mins } else { ctx.cfg.limits.max_session_mins };
    if mins > 0 {
        term.deadline = Some(tokio::time::Instant::now() + Duration::from_secs(mins * 60));
    }
    term.rows = user.screen_rows.clamp(8, 100) as usize;

    login::greeting(term, ctx, &user, id).await?;
    menu::main_menu(term, ctx, &mut user).await
}
