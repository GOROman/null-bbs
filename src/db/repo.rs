//! テーブルごとの読み書き (DB スレッド上で呼ばれる同期関数)

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};

pub const LEVEL_GUEST: i64 = 0;
pub const LEVEL_MEMBER: i64 = 10;
pub const LEVEL_SYSOP: i64 = 100;

/// 会員番号: 0 はゲスト、1 は SYSOP、2〜9 は予約、会員は 10 から
pub const GUEST_ID: i64 = 0;
pub const SYSOP_ID: i64 = 1;
pub const FIRST_MEMBER_ID: i64 = 10;

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// ゲスト用ユーザーと最初のボードを用意する
pub fn ensure_defaults(c: &Connection) -> Result<()> {
    if find_user(c, "GUEST")?.is_none() {
        // pw_hash "!" はどのパスワードとも一致しない
        c.execute(
            "INSERT INTO users (id, login, handle, pw_hash, level, created_at) VALUES (?1, 'GUEST', 'ゲスト', '!', 0, ?2)",
            params![GUEST_ID, now()],
        )?;
    }
    let boards: i64 = c.query_row("SELECT count(*) FROM boards", [], |r| r.get(0))?;
    if boards == 0 {
        add_board(c, "INFO", "お知らせ", "SYSOP からのお知らせ", 0, LEVEL_SYSOP)?;
        add_board(c, "FREE", "雑談", "なんでも自由に書き込んでください", 0, LEVEL_MEMBER)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ users

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub login: String,
    pub handle: String,
    pub pw_hash: String,
    pub level: i64,
    pub banned: bool,
    pub screen_rows: i64,
    pub profile: String,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
    pub login_count: i64,
}

impl User {
    pub fn is_guest(&self) -> bool {
        self.level <= LEVEL_GUEST
    }
}

const USER_COLS: &str =
    "id, login, handle, pw_hash, level, banned, screen_rows, profile, created_at, last_login_at, login_count";

fn user_row(r: &Row) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        login: r.get(1)?,
        handle: r.get(2)?,
        pw_hash: r.get(3)?,
        level: r.get(4)?,
        banned: r.get::<_, i64>(5)? != 0,
        screen_rows: r.get(6)?,
        profile: r.get(7)?,
        created_at: r.get(8)?,
        last_login_at: r.get(9)?,
        login_count: r.get(10)?,
    })
}

pub fn find_user(c: &Connection, login: &str) -> Result<Option<User>> {
    Ok(c.query_row(&format!("SELECT {USER_COLS} FROM users WHERE login = ?1"), [login], user_row)
        .optional()?)
}

pub fn get_user(c: &Connection, id: i64) -> Result<Option<User>> {
    Ok(c.query_row(&format!("SELECT {USER_COLS} FROM users WHERE id = ?1"), [id], user_row).optional()?)
}

pub fn list_users(c: &Connection) -> Result<Vec<User>> {
    let mut st = c.prepare(&format!("SELECT {USER_COLS} FROM users ORDER BY id"))?;
    let rows = st.query_map([], user_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn create_user(c: &Connection, login: &str, handle: &str, pw_hash: &str, level: i64) -> Result<i64> {
    if find_user(c, login)?.is_some() {
        bail!("ID {login} はすでに使われています");
    }
    // SYSOP は 1 番が空いていれば 1 番。それ以外は 10 番以降の続き番号
    let id = if level >= LEVEL_SYSOP && get_user(c, SYSOP_ID)?.is_none() {
        SYSOP_ID
    } else {
        c.query_row("SELECT max(coalesce(max(id), 0), ?1 - 1) + 1 FROM users", [FIRST_MEMBER_ID], |r| r.get(0))?
    };
    c.execute(
        "INSERT INTO users (id, login, handle, pw_hash, level, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, login, handle, pw_hash, level, now()],
    )?;
    Ok(id)
}

/// 既存の会員番号を「ゲスト 0 / 最初の SYSOP 1 / 他は 10〜」に振り直す (マイグレーション)
pub fn renumber_users(c: &Connection) -> Result<()> {
    let users: Vec<(i64, String, i64)> = {
        let mut st = c.prepare("SELECT id, login, level FROM users ORDER BY id")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let sysop = users.iter().filter(|u| u.2 >= LEVEL_SYSOP && !u.1.eq_ignore_ascii_case("GUEST")).map(|u| u.0).min();
    let mut next = FIRST_MEMBER_ID;
    let map: Vec<(i64, i64)> = users
        .iter()
        .map(|(id, login, _)| {
            let new = if login.eq_ignore_ascii_case("GUEST") {
                GUEST_ID
            } else if Some(*id) == sysop {
                SYSOP_ID
            } else {
                next += 1;
                next - 1
            };
            (*id, new)
        })
        .collect();
    let refs = [
        ("users", "id"),
        ("posts", "author_id"),
        ("read_marks", "user_id"),
        ("mail", "from_id"),
        ("mail", "to_id"),
        ("access_log", "user_id"),
        ("files", "uploader_id"),
    ];
    // 番号がぶつからないよう、いったん負の仮番号 (-新番号 - 1000) にしてから戻す
    for (table, col) in refs {
        for (old, new) in &map {
            c.execute(&format!("UPDATE {table} SET {col} = ?2 WHERE {col} = ?1"), params![old, -new - 1000])?;
        }
        c.execute(&format!("UPDATE {table} SET {col} = -{col} - 1000 WHERE {col} <= -1000"), [])?;
    }
    Ok(())
}

pub fn record_login(c: &Connection, id: i64) -> Result<()> {
    c.execute(
        "UPDATE users SET last_login_at = ?2, login_count = login_count + 1 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

pub fn set_handle(c: &Connection, id: i64, handle: &str) -> Result<()> {
    c.execute("UPDATE users SET handle = ?2 WHERE id = ?1", params![id, handle])?;
    Ok(())
}

pub fn set_password(c: &Connection, id: i64, pw_hash: &str) -> Result<()> {
    c.execute("UPDATE users SET pw_hash = ?2 WHERE id = ?1", params![id, pw_hash])?;
    Ok(())
}

pub fn set_screen_rows(c: &Connection, id: i64, rows: i64) -> Result<()> {
    c.execute("UPDATE users SET screen_rows = ?2 WHERE id = ?1", params![id, rows])?;
    Ok(())
}

pub fn set_profile(c: &Connection, id: i64, profile: &str) -> Result<()> {
    c.execute("UPDATE users SET profile = ?2 WHERE id = ?1", params![id, profile])?;
    Ok(())
}

pub fn set_level(c: &Connection, id: i64, level: i64) -> Result<()> {
    c.execute("UPDATE users SET level = ?2 WHERE id = ?1", params![id, level])?;
    Ok(())
}

pub fn set_banned(c: &Connection, id: i64, banned: bool) -> Result<()> {
    c.execute("UPDATE users SET banned = ?2 WHERE id = ?1", params![id, banned as i64])?;
    Ok(())
}

// ------------------------------------------------------------------ boards

#[derive(Debug, Clone)]
pub struct Board {
    pub id: i64,
    pub name: String,
    pub title: String,
    pub description: String,
    pub read_level: i64,
    pub write_level: i64,
    /// 記事数
    pub count: i64,
    /// 未読数 (ログイン中のユーザーから見て)
    pub unread: i64,
    pub last_seq: i64,
}

pub fn add_board(c: &Connection, name: &str, title: &str, desc: &str, read_level: i64, write_level: i64) -> Result<i64> {
    let exists: Option<i64> = c
        .query_row("SELECT id FROM boards WHERE name = ?1 AND deleted = 0", [name], |r| r.get(0))
        .optional()?;
    if exists.is_some() {
        bail!("ボード {name} はすでにあります");
    }
    let order: i64 = c.query_row("SELECT coalesce(max(sort_order), 0) + 1 FROM boards", [], |r| r.get(0))?;
    c.execute(
        "INSERT INTO boards (name, title, description, read_level, write_level, sort_order, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![name, title, desc, read_level, write_level, order, now()],
    )?;
    Ok(c.last_insert_rowid())
}

pub fn delete_board(c: &Connection, id: i64) -> Result<()> {
    c.execute("UPDATE boards SET deleted = 1, name = name || '#' || id WHERE id = ?1", [id])?;
    Ok(())
}

/// `level` で読めるボードの一覧 (未読数は `user_id` の既読位置から)
pub fn list_boards(c: &Connection, level: i64, user_id: i64) -> Result<Vec<Board>> {
    let mut st = c.prepare(
        "SELECT b.id, b.name, b.title, b.description, b.read_level, b.write_level,
                (SELECT count(*) FROM posts p WHERE p.board_id = b.id AND p.deleted = 0),
                (SELECT count(*) FROM posts p WHERE p.board_id = b.id AND p.deleted = 0
                   AND p.seq > coalesce((SELECT last_read_seq FROM read_marks m
                                          WHERE m.user_id = ?2 AND m.board_id = b.id), 0)),
                b.next_seq - 1
         FROM boards b WHERE b.deleted = 0 AND b.read_level <= ?1 ORDER BY b.sort_order, b.id",
    )?;
    let rows = st
        .query_map(params![level, user_id], |r| {
            Ok(Board {
                id: r.get(0)?,
                name: r.get(1)?,
                title: r.get(2)?,
                description: r.get(3)?,
                read_level: r.get(4)?,
                write_level: r.get(5)?,
                count: r.get(6)?,
                unread: r.get(7)?,
                last_seq: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

// ------------------------------------------------------------------ posts

#[derive(Debug, Clone)]
pub struct Post {
    pub seq: i64,
    pub parent_seq: Option<i64>,
    pub author_login: String,
    pub author_handle: String,
    pub title: String,
    pub body: String,
    pub created_at: i64,
}

fn post_row(r: &Row) -> rusqlite::Result<Post> {
    Ok(Post {
        seq: r.get(0)?,
        parent_seq: r.get(1)?,
        author_login: r.get(2)?,
        author_handle: r.get(3)?,
        title: r.get(4)?,
        body: r.get(5)?,
        created_at: r.get(6)?,
    })
}

const POST_COLS: &str = "seq, parent_seq, author_login, author_handle, title, body, created_at";

pub fn list_posts(c: &Connection, board_id: i64) -> Result<Vec<Post>> {
    let mut st = c.prepare(&format!(
        "SELECT {POST_COLS} FROM posts WHERE board_id = ?1 AND deleted = 0 ORDER BY seq"
    ))?;
    let rows = st.query_map([board_id], post_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn get_post(c: &Connection, board_id: i64, seq: i64) -> Result<Option<Post>> {
    Ok(c.query_row(
        &format!("SELECT {POST_COLS} FROM posts WHERE board_id = ?1 AND seq = ?2 AND deleted = 0"),
        params![board_id, seq],
        post_row,
    )
    .optional()?)
}

/// `after` より後の記事 (未読の順読み用)
pub fn posts_after(c: &Connection, board_id: i64, after: i64) -> Result<Vec<Post>> {
    let mut st = c.prepare(&format!(
        "SELECT {POST_COLS} FROM posts WHERE board_id = ?1 AND seq > ?2 AND deleted = 0 ORDER BY seq"
    ))?;
    let rows = st.query_map(params![board_id, after], post_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn add_post(
    c: &mut Connection,
    board_id: i64,
    parent_seq: Option<i64>,
    author: &User,
    title: &str,
    body: &str,
) -> Result<i64> {
    let tx = c.transaction()?;
    let seq: i64 = tx.query_row("SELECT next_seq FROM boards WHERE id = ?1", [board_id], |r| r.get(0))?;
    tx.execute("UPDATE boards SET next_seq = next_seq + 1 WHERE id = ?1", [board_id])?;
    tx.execute(
        "INSERT INTO posts (board_id, seq, parent_seq, author_id, author_login, author_handle, title, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![board_id, seq, parent_seq, author.id, author.login, author.handle, title, body, now()],
    )?;
    tx.commit()?;
    Ok(seq)
}

pub fn delete_post(c: &Connection, board_id: i64, seq: i64) -> Result<()> {
    c.execute("UPDATE posts SET deleted = 1 WHERE board_id = ?1 AND seq = ?2", params![board_id, seq])?;
    Ok(())
}

pub fn read_mark(c: &Connection, user_id: i64, board_id: i64) -> Result<i64> {
    Ok(c.query_row(
        "SELECT last_read_seq FROM read_marks WHERE user_id = ?1 AND board_id = ?2",
        params![user_id, board_id],
        |r| r.get(0),
    )
    .optional()?
    .unwrap_or(0))
}

/// 既読位置を進める (戻しはしない)
pub fn mark_read(c: &Connection, user_id: i64, board_id: i64, seq: i64) -> Result<()> {
    c.execute(
        "INSERT INTO read_marks (user_id, board_id, last_read_seq) VALUES (?1, ?2, ?3)
         ON CONFLICT (user_id, board_id) DO UPDATE SET last_read_seq = max(last_read_seq, excluded.last_read_seq)",
        params![user_id, board_id, seq],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ mail

#[derive(Debug, Clone)]
pub struct Mail {
    pub id: i64,
    pub from_login: String,
    pub from_handle: String,
    pub subject: String,
    pub body: String,
    pub created_at: i64,
    pub read: bool,
}

pub fn inbox(c: &Connection, user_id: i64) -> Result<Vec<Mail>> {
    let mut st = c.prepare(
        "SELECT m.id, u.login, u.handle, m.subject, m.body, m.created_at, m.read_at IS NOT NULL
         FROM mail m JOIN users u ON u.id = m.from_id
         WHERE m.to_id = ?1 AND m.del_by_to = 0 ORDER BY m.id",
    )?;
    let rows = st
        .query_map([user_id], |r| {
            Ok(Mail {
                id: r.get(0)?,
                from_login: r.get(1)?,
                from_handle: r.get(2)?,
                subject: r.get(3)?,
                body: r.get(4)?,
                created_at: r.get(5)?,
                read: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn unread_mail_count(c: &Connection, user_id: i64) -> Result<i64> {
    Ok(c.query_row(
        "SELECT count(*) FROM mail WHERE to_id = ?1 AND del_by_to = 0 AND read_at IS NULL",
        [user_id],
        |r| r.get(0),
    )?)
}

pub fn send_mail(c: &Connection, from_id: i64, to_id: i64, subject: &str, body: &str) -> Result<i64> {
    c.execute(
        "INSERT INTO mail (from_id, to_id, subject, body, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![from_id, to_id, subject, body, now()],
    )?;
    Ok(c.last_insert_rowid())
}

pub fn mark_mail_read(c: &Connection, id: i64) -> Result<()> {
    c.execute("UPDATE mail SET read_at = ?2 WHERE id = ?1 AND read_at IS NULL", params![id, now()])?;
    Ok(())
}

pub fn delete_mail(c: &Connection, id: i64, user_id: i64) -> Result<()> {
    c.execute("UPDATE mail SET del_by_to = 1 WHERE id = ?1 AND to_id = ?2", params![id, user_id])?;
    Ok(())
}

// ------------------------------------------------------------------ access_log

#[derive(Debug, Clone)]
pub struct Access {
    pub login: String,
    pub handle: String,
    pub line_no: i64,
    pub line_kind: String,
    pub peer: String,
    pub login_at: i64,
    pub logout_at: Option<i64>,
    pub reason: Option<String>,
}

pub fn log_login(c: &Connection, user: &User, line_no: u16, kind: &str, peer: &str) -> Result<i64> {
    c.execute(
        "INSERT INTO access_log (user_id, login, handle, line_no, line_kind, peer, login_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![user.id, user.login, user.handle, line_no, kind, peer, now()],
    )?;
    Ok(c.last_insert_rowid())
}

pub fn log_logout(c: &Connection, id: i64, reason: &str) -> Result<()> {
    c.execute("UPDATE access_log SET logout_at = ?2, reason = ?3 WHERE id = ?1", params![id, now(), reason])?;
    Ok(())
}

fn access_row(r: &Row) -> rusqlite::Result<Access> {
    Ok(Access {
        login: r.get(0)?,
        handle: r.get(1)?,
        line_no: r.get(2)?,
        line_kind: r.get(3)?,
        peer: r.get(4)?,
        login_at: r.get(5)?,
        logout_at: r.get(6)?,
        reason: r.get(7)?,
    })
}

const ACCESS_COLS: &str = "login, handle, line_no, line_kind, peer, login_at, logout_at, reason";

pub fn recent_access(c: &Connection, n: i64) -> Result<Vec<Access>> {
    let mut st = c.prepare(&format!("SELECT {ACCESS_COLS} FROM access_log ORDER BY id DESC LIMIT ?1"))?;
    let rows = st.query_map([n], access_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// 今回 (`current`) より前の最後のログイン
pub fn previous_login(c: &Connection, user_id: i64, current: i64) -> Result<Option<Access>> {
    Ok(c.query_row(
        &format!("SELECT {ACCESS_COLS} FROM access_log WHERE user_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT 1"),
        params![user_id, current],
        access_row,
    )
    .optional()?)
}

pub fn log_chat(c: &Connection, room: &str, handle: &str, line_no: u16, text: &str) -> Result<()> {
    c.execute(
        "INSERT INTO chat_log (room, handle, line_no, text, at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![room, handle, line_no, text, now()],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ files

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub id: i64,
    pub name: String,
    pub size: i64,
    pub description: String,
    pub uploader_login: String,
    pub uploader_handle: String,
    pub created_at: i64,
    pub downloads: i64,
}

pub fn list_files(c: &Connection) -> Result<Vec<FileEntry>> {
    let mut st = c.prepare(
        "SELECT id, name, size, description, uploader_login, uploader_handle, created_at, downloads
         FROM files WHERE deleted = 0 ORDER BY id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(FileEntry {
                id: r.get(0)?,
                name: r.get(1)?,
                size: r.get(2)?,
                description: r.get(3)?,
                uploader_login: r.get(4)?,
                uploader_handle: r.get(5)?,
                created_at: r.get(6)?,
                downloads: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub fn add_file(c: &Connection, name: &str, size: i64, desc: &str, user: &User) -> Result<i64> {
    c.execute(
        "INSERT INTO files (name, size, description, uploader_id, uploader_login, uploader_handle, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![name, size, desc, user.id, user.login, user.handle, now()],
    )?;
    Ok(c.last_insert_rowid())
}

pub fn count_download(c: &Connection, id: i64) -> Result<()> {
    c.execute("UPDATE files SET downloads = downloads + 1 WHERE id = ?1", [id])?;
    Ok(())
}

pub fn delete_file(c: &Connection, id: i64) -> Result<()> {
    c.execute("UPDATE files SET deleted = 1, name = name || '#deleted' || id WHERE id = ?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../../migrations/001_init.sql")).unwrap();
        c.execute_batch(include_str!("../../migrations/002_files.sql")).unwrap();
        ensure_defaults(&mut c).unwrap();
        c
    }

    #[test]
    fn posts_and_unread() {
        let mut c = db();
        let a = create_user(&c, "alice", "アリス", "x", LEVEL_MEMBER).unwrap();
        let b = create_user(&c, "bob", "ボブ", "x", LEVEL_MEMBER).unwrap();
        assert!(create_user(&c, "ALICE", "x", "x", 10).is_err(), "ID は大文字小文字を区別しない");
        let alice = get_user(&c, a).unwrap().unwrap();
        let free = list_boards(&c, 10, a).unwrap().into_iter().find(|b| b.name == "FREE").unwrap();
        assert_eq!(add_post(&mut c, free.id, None, &alice, "はじめまして", "よろしく").unwrap(), 1);
        assert_eq!(add_post(&mut c, free.id, Some(1), &alice, "Re: はじめまして", "続き").unwrap(), 2);
        let bb = list_boards(&c, 10, b).unwrap().into_iter().find(|x| x.id == free.id).unwrap();
        assert_eq!((bb.count, bb.unread), (2, 2));
        mark_read(&c, b, free.id, 1).unwrap();
        mark_read(&c, b, free.id, 0).unwrap(); // 戻らない
        assert_eq!(read_mark(&c, b, free.id).unwrap(), 1);
        assert_eq!(posts_after(&c, free.id, 1).unwrap().len(), 1);
    }

    #[test]
    fn user_numbers() {
        let c = db();
        assert_eq!(find_user(&c, "GUEST").unwrap().unwrap().id, GUEST_ID);
        assert_eq!(create_user(&c, "alice", "a", "x", LEVEL_MEMBER).unwrap(), 10);
        assert_eq!(create_user(&c, "root", "r", "x", LEVEL_SYSOP).unwrap(), SYSOP_ID);
        assert_eq!(create_user(&c, "bob", "b", "x", LEVEL_MEMBER).unwrap(), 11);
        assert_eq!(create_user(&c, "sub", "s", "x", LEVEL_SYSOP).unwrap(), 12, "1 番が埋まっていたら続き番号");
    }

    #[test]
    fn renumber_old_db() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../../migrations/001_init.sql")).unwrap();
        c.execute_batch(include_str!("../../migrations/002_files.sql")).unwrap();
        // 旧方式: ゲスト 1、会員 2, 3 (3 が SYSOP)
        for (login, level) in [("GUEST", 0), ("alice", 10), ("root", 100)] {
            c.execute("INSERT INTO users (login, handle, pw_hash, level, created_at) VALUES (?1, ?1, 'x', ?2, 0)", params![login, level]).unwrap();
        }
        add_board(&c, "B", "b", "", 0, 10).unwrap();
        let alice = find_user(&c, "alice").unwrap().unwrap();
        add_post(&mut c, 1, None, &alice, "t", "b").unwrap();
        send_mail(&c, 2, 3, "s", "b").unwrap();
        c.pragma_update(None, "foreign_keys", "OFF").unwrap(); // 本番のマイグレーションと同じ
        renumber_users(&c).unwrap();
        assert_eq!(find_user(&c, "GUEST").unwrap().unwrap().id, 0);
        assert_eq!(find_user(&c, "root").unwrap().unwrap().id, 1);
        assert_eq!(find_user(&c, "alice").unwrap().unwrap().id, 10);
        let author: i64 = c.query_row("SELECT author_id FROM posts", [], |r| r.get(0)).unwrap();
        let (from, to): (i64, i64) = c.query_row("SELECT from_id, to_id FROM mail", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((author, from, to), (10, 10, 1));
    }

    #[test]
    fn mail_flow() {
        let c = db();
        let a = create_user(&c, "alice", "アリス", "x", 10).unwrap();
        let b = create_user(&c, "bob", "ボブ", "x", 10).unwrap();
        let id = send_mail(&c, a, b, "こんにちは", "本文").unwrap();
        assert_eq!(unread_mail_count(&c, b).unwrap(), 1);
        mark_mail_read(&c, id).unwrap();
        assert_eq!(unread_mail_count(&c, b).unwrap(), 0);
        delete_mail(&c, id, b).unwrap();
        assert!(inbox(&c, b).unwrap().is_empty());
    }
}
