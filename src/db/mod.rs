//! SQLite を専用スレッドで扱うアクター
//!
//! `Db::call(|conn| ...)` でクロージャを DB スレッドに送り、結果を待つ。

pub mod repo;

use std::path::Path;
use std::sync::mpsc;
use std::thread;

use anyhow::{anyhow, Result};
use rusqlite::Connection;
use tokio::sync::oneshot;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
pub struct Db {
    tx: mpsc::Sender<Job>,
}

const MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/001_init.sql"),
    include_str!("../../migrations/002_files.sql"),
];

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        Self::start(Connection::open(path)?)
    }

    pub fn open_memory() -> Result<Db> {
        Self::start(Connection::open_in_memory()?)
    }

    fn start(mut conn: Connection) -> Result<Db> {
        setup(&mut conn)?;
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new().name("db".into()).spawn(move || {
            for job in rx {
                job(&mut conn);
            }
        })?;
        Ok(Db { tx })
    }

    /// DB スレッドで `f` を実行する (async)
    pub async fn call<R, F>(&self, f: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<R> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .map_err(|_| anyhow!("DB スレッドが停止しています"))?;
        rx.await.map_err(|_| anyhow!("DB スレッドが応答しません"))?
    }

    /// DB スレッドで `f` を実行する (同期版。管理画面スレッド用)
    pub fn call_blocking<R, F>(&self, f: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<R> + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .map_err(|_| anyhow!("DB スレッドが停止しています"))?;
        rx.recv().map_err(|_| anyhow!("DB スレッドが応答しません"))?
    }
}

fn setup(conn: &mut Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as i64 + 1)?;
        tx.commit()?;
    }
    repo::ensure_defaults(conn)?;
    Ok(())
}
