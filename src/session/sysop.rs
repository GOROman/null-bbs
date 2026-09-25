//! SYSOP メニュー (回線からの管理。主な管理は管理コンソールで行う)

use crate::db::repo::{self, User};
use crate::term::{SResult, Term};

use super::util::split_cmd;
use super::Ctx;

pub async fn run(term: &mut Term<'_>, ctx: &Ctx, user: &User) -> SResult<()> {
    ctx.hub.set_place(term.line_no(), "SYSOP");
    loop {
        let a = term.input("\n[SYSOP] B:全体放送 K n:回線切断 A:ボード追加 Q:戻る? ").await?;
        let (cmd, arg) = split_cmd(&a);
        match cmd.as_str() {
            "" | "Q" => return Ok(()),
            "B" => {
                let text = if arg.is_empty() { term.input_text("放送する文: ", 100).await? } else { arg };
                if !text.trim().is_empty() {
                    let n = ctx.hub.broadcast(text.trim());
                    tracing::info!("{} が全体放送: {}", user.login, text.trim());
                    term.println(&format!("{n} 人に送りました。")).await?;
                }
            }
            "K" => match arg.parse::<u16>() {
                Ok(no) if no != term.line_no() => {
                    let ok = ctx.hub.kick(no, "SYSOP により切断されました");
                    term.println(if ok { "切断しました。" } else { "その回線は使われていません。" }).await?;
                }
                _ => term.println("K の後に回線番号を入れてください (自分は切れません)").await?,
            },
            "A" => {
                let name = term.input("ボード名 (英数字): ").await?.to_uppercase();
                let title = term.input_text("表示名: ", 20).await?.trim().to_string();
                if name.is_empty() || title.is_empty() {
                    continue;
                }
                let desc = term.input_text("説明: ", 60).await?;
                let r = ctx.db.call(move |c| repo::add_board(c, &name, &title, desc.trim(), 0, repo::LEVEL_MEMBER)).await;
                match r {
                    Ok(_) => term.println("追加しました。").await?,
                    Err(e) => term.println(&format!("{e:#}")).await?,
                }
            }
            _ => term.println("コマンドが違います").await?,
        }
    }
}
