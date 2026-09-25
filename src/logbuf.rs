//! システムログ: 管理画面に出すためのリングバッファとログファイルへの出力

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

const MAX_LINES: usize = 1000;

static LOG: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// 直近 `n` 行
pub fn tail(n: usize) -> Vec<String> {
    let log = LOG.lock().unwrap();
    log.iter().skip(log.len().saturating_sub(n)).cloned().collect()
}

#[derive(Clone)]
struct RingMaker {
    file: Option<Arc<Mutex<File>>>,
    stderr: bool,
}

struct RingWriter {
    buf: Vec<u8>,
    maker: RingMaker,
}

impl Write for RingWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for RingWriter {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        if let Some(f) = &self.maker.file {
            let _ = f.lock().unwrap().write_all(&self.buf);
        }
        if self.maker.stderr {
            let _ = io::stderr().write_all(&self.buf);
        }
        let text = String::from_utf8_lossy(&self.buf);
        let mut log = LOG.lock().unwrap();
        for line in text.lines() {
            log.push_back(line.to_string());
        }
        while log.len() > MAX_LINES {
            log.pop_front();
        }
    }
}

impl<'a> MakeWriter<'a> for RingMaker {
    type Writer = RingWriter;
    fn make_writer(&'a self) -> RingWriter {
        RingWriter { buf: Vec::new(), maker: self.clone() }
    }
}

/// ログを初期化する。`stderr` は headless 起動のとき true
pub fn init(file: &Path, stderr: bool) -> anyhow::Result<()> {
    let f = OpenOptions::new().create(true).append(true).open(file)?;
    let maker = RingMaker { file: Some(Arc::new(Mutex::new(f))), stderr };
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(maker)
        .with_ansi(false)
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::ChronoLocal::new("%m/%d %H:%M:%S".into()));
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(layer)
        .init();
    Ok(())
}
