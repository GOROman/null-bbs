//! メインメニュー

use crate::db::repo::{User, LEVEL_SYSOP};
use crate::term::{SResult, SessionError, Term};

use super::util::split_cmd;
use super::{bbs, chat, files, info, mail, sysop, Ctx};

fn menu_text(ctx: &Ctx, user: &User) -> String {
    let mut s = format!(
        "\n== {} メインメニュー ==\n 1.掲示板 BBS    2.メール MAIL   3.チャット CHAT\n 4.在室者 WHO    5.足跡 LOG      6.登録情報 PROF\n 7.ファイル FILE  T.電報 TEL\n",
        ctx.cfg.bbs.name
    );
    if user.level >= LEVEL_SYSOP {
        s += " 9.SYSOP SYS\n";
    }
    s += " 0.終了 BYE\n";
    s
}

pub async fn main_menu(term: &mut Term<'_>, ctx: &Ctx, user: &mut User) -> SResult<()> {
    term.print(&menu_text(ctx, user)).await?;
    let no = term.line_no();
    loop {
        ctx.hub.set_place(no, "メニュー");
        let line = term.input("\nコマンド (?:メニュー)? ").await?;
        let (cmd, arg) = split_cmd(&line);
        let r = match cmd.as_str() {
            "" => continue,
            "?" | "H" | "HELP" | "MENU" => term.print(&menu_text(ctx, user)).await,
            "1" | "BBS" | "B" => bbs::run(term, ctx, user).await,
            "2" | "MAIL" | "M" => mail::run(term, ctx, user).await,
            "3" | "CHAT" | "C" => chat::run(term, ctx, user, &arg).await,
            "4" | "WHO" | "W" => info::who(term, ctx).await,
            "5" | "LOG" | "L" => info::log(term, ctx).await,
            "6" | "PROF" | "P" => info::profile(term, ctx, user).await,
            "7" | "FILE" | "F" => files::run(term, ctx, user).await,
            "T" | "TEL" => info::telegram(term, ctx, user, &arg).await,
            "9" | "SYS" | "SYSOP" if user.level >= LEVEL_SYSOP => sysop::run(term, ctx, user).await,
            "0" | "BYE" | "G" | "OFF" | "Q" | "QUIT" => return Ok(()),
            _ => term.println("コマンドが違います。? でメニューを表示します。").await,
        };
        match r {
            Err(SessionError::Logoff) => return Ok(()),
            other => other?,
        }
    }
}
