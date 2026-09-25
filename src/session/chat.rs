//! チャット

use crate::db::repo::{self, User};
use crate::term::{SResult, SessionError, Term};

use super::util::split_cmd;
use super::Ctx;

const HELP: &str = "/Q:退室 /W:在室者 /R 部屋名:部屋を移動 /L:部屋の一覧 /T 相手 本文:電報 /?:ヘルプ";

fn room_name(s: &str) -> String {
    let s = s.trim().to_uppercase();
    if s.is_empty() { "LOBBY".into() } else { s.chars().take(16).collect() }
}

pub async fn run(term: &mut Term<'_>, ctx: &Ctx, user: &User, arg: &str) -> SResult<()> {
    let no = term.line_no();
    let mut room = room_name(arg);
    loop {
        ctx.hub.chat_join(&room, no, &user.handle);
        ctx.hub.set_place(no, &format!("チャット {room}"));
        let result = in_room(term, ctx, user, &room).await;
        ctx.hub.chat_leave(&room, no, &user.handle);
        match result? {
            Some(next) => room = next,
            None => return Ok(()),
        }
    }
}

/// 部屋を出るまで。移動するなら次の部屋名を返す
async fn in_room(term: &mut Term<'_>, ctx: &Ctx, user: &User, room: &str) -> SResult<Option<String>> {
    let no = term.line_no();
    term.println(&format!("\n*** チャット {room} に入室しました ***\n{HELP}")).await?;
    who(term, ctx, room).await?;
    loop {
        let line = term.input_text("", 200).await?;
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(cmd) = text.strip_prefix('/') {
            let (c, arg) = split_cmd(cmd);
            match c.as_str() {
                "Q" | "E" | "EXIT" => {
                    term.println(&format!("*** {room} から退室しました ***")).await?;
                    return Ok(None);
                }
                "G" => return Err(SessionError::Logoff),
                "W" => who(term, ctx, room).await?,
                "R" if !arg.is_empty() => return Ok(Some(room_name(&arg))),
                "L" => {
                    let rooms = ctx.hub.chat_rooms();
                    let list: Vec<String> = rooms.iter().map(|(r, n)| format!("{r}({n}人)")).collect();
                    term.println(&format!("*** 部屋: {}", list.join(" "))).await?;
                }
                "T" => super::info::telegram(term, ctx, user, &arg).await?,
                _ => term.println(HELP).await?,
            }
            continue;
        }
        ctx.hub.chat_say(room, no, &user.handle, text);
        if ctx.cfg.bbs.chat_log {
            let (r, h, t) = (room.to_string(), user.handle.clone(), text.to_string());
            ctx.db.call(move |c| repo::log_chat(c, &r, &h, no, &t)).await?;
        }
    }
}

async fn who(term: &mut Term<'_>, ctx: &Ctx, room: &str) -> SResult<()> {
    let members: Vec<String> =
        ctx.hub.chat_members(room).iter().map(|(no, h)| format!("{h}(CH{no:02})")).collect();
    term.println(&format!("*** 在室者 {}人: {}", members.len(), members.join(" "))).await
}
