//! シリアルポートを Conn として扱う (読み書きはそれぞれ OS スレッド)
//!
//! ポートを開く処理・書き込み・DTR 操作は null-term の channel.rs と同じ考え方。

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serialport::{FlowControl, SerialPort};

use super::{Conn, InEvent, LineInfo, OutCmd};
use crate::config::ModemConfig;

/// オンライン中 (セッション中) だけキャリア断を監視する
pub struct CarrierWatch {
    pub online: Arc<AtomicBool>,
}

fn flow(s: &str) -> FlowControl {
    match s.to_ascii_lowercase().as_str() {
        "hardware" | "hw" | "rts" => FlowControl::Hardware,
        "software" | "sw" | "xon" => FlowControl::Software,
        _ => FlowControl::None,
    }
}

#[cfg(unix)]
fn open_raw(path: &str) -> serialport::Result<Box<dyn SerialPort>> {
    use std::os::fd::{FromRawFd, IntoRawFd};
    let file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let mut port = unsafe { serialport::TTYPort::from_raw_fd(file.into_raw_fd()) };
    port.set_timeout(Duration::from_millis(50))?;
    Ok(Box::new(port))
}

#[cfg(not(unix))]
fn open_raw(path: &str) -> serialport::Result<Box<dyn SerialPort>> {
    Err(serialport::Error::new(serialport::ErrorKind::NoDevice, path))
}

fn open_port(cfg: &ModemConfig) -> Result<Box<dyn SerialPort>> {
    let r = serialport::new(&cfg.path, cfg.baud)
        .flow_control(flow(&cfg.flow))
        .timeout(Duration::from_millis(50))
        .open();
    // pty など速度設定を受け付けないものは素の fd として開く (テスト用)
    match r {
        Ok(p) => Ok(p),
        Err(e) => open_raw(&cfg.path).map_err(|_| e).with_context(|| format!("{} を開けません", cfg.path)),
    }
}

/// タイムアウトしても待ち続けて全部書く。10 秒進まなければエラー
fn write_all_patient(port: &mut dyn SerialPort, mut data: &[u8], abort: &AtomicBool) -> std::io::Result<usize> {
    let total = data.len();
    let mut last = Instant::now();
    while !data.is_empty() {
        if abort.load(Ordering::Relaxed) {
            break;
        }
        match port.write(data) {
            Ok(n) if n > 0 => {
                data = &data[n..];
                last = Instant::now();
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
            Err(e) => return Err(e),
        }
        if last.elapsed() > Duration::from_secs(10) {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "送信が進みません"));
        }
    }
    port.flush()?;
    Ok(total - data.len())
}

/// ポートを開いて Conn を返す
pub fn open(cfg: &ModemConfig, info: Arc<LineInfo>) -> Result<(Conn, CarrierWatch)> {
    let mut writer = open_port(cfg)?;
    let _ = writer.write_data_terminal_ready(true);
    let mut reader = writer.try_clone()?;
    let (conn, in_tx, mut out_rx) = Conn::pair(info.clone());
    let online = Arc::new(AtomicBool::new(false));
    let use_dcd = matches!(cfg.carrier.as_str(), "dcd" | "both");
    let use_text = matches!(cfg.carrier.as_str(), "text" | "both");
    let no = info.no;

    let rinfo = info.clone();
    let ronline = online.clone();
    thread::Builder::new().name(format!("serial-rx-{no}")).spawn(move || {
        let mut buf = [0u8; 1024];
        let mut tail: Vec<u8> = Vec::new();
        let mut reported = false;
        let mut last_dcd = Instant::now();
        loop {
            let is_online = ronline.load(Ordering::Relaxed);
            if !is_online {
                reported = false;
                tail.clear();
            }
            match reader.read(&mut buf) {
                Ok(n) if n > 0 => {
                    rinfo.add_rx(n);
                    if is_online && use_text && !reported {
                        // 受信の流れから「NO CARRIER」を探す (行をまたいでもよいよう末尾を残す)
                        tail.extend_from_slice(&buf[..n]);
                        if tail.windows(10).any(|w| w == b"NO CARRIER") {
                            reported = true;
                            let _ = in_tx.blocking_send(InEvent::CarrierLost);
                        }
                        let keep = tail.len().saturating_sub(16);
                        tail.drain(..keep);
                    }
                    if in_tx.blocking_send(InEvent::Data(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
                Err(e) => {
                    tracing::warn!("CH{no:02}: シリアル読み込みエラー: {e}");
                    let _ = in_tx.blocking_send(InEvent::Closed);
                    break;
                }
            }
            if is_online && use_dcd && !reported && last_dcd.elapsed() > Duration::from_millis(200) {
                last_dcd = Instant::now();
                if let Ok(false) = reader.read_carrier_detect() {
                    reported = true;
                    let _ = in_tx.blocking_send(InEvent::CarrierLost);
                }
            }
            if in_tx.is_closed() {
                break;
            }
        }
    })?;

    let hangup = cfg.hangup.clone();
    thread::Builder::new().name(format!("serial-tx-{no}")).spawn(move || {
        while let Some(cmd) = out_rx.blocking_recv() {
            match cmd {
                OutCmd::Write(data) => {
                    if info.abort.load(Ordering::Relaxed) {
                        continue;
                    }
                    match write_all_patient(writer.as_mut(), &data, &info.abort) {
                        Ok(n) => info.add_tx(n),
                        Err(e) => tracing::warn!("CH{no:02}: シリアル書き込みエラー: {e}"),
                    }
                }
                OutCmd::Hangup => {
                    info.abort.store(false, Ordering::Relaxed);
                    if hangup == "escape" {
                        tracing::info!("CH{no:02}: → +++ / ATH0 (回線を切ります)");
                        thread::sleep(Duration::from_millis(1200));
                        let _ = writer.write_all(b"+++");
                        let _ = writer.flush();
                        thread::sleep(Duration::from_millis(1200));
                        let _ = writer.write_all(b"ATH0\r");
                        let _ = writer.flush();
                    } else {
                        // DTR を落とすと &D2 のモデムは回線を切る
                        tracing::info!("CH{no:02}: DTR OFF (回線を切ります)");
                        let _ = writer.write_data_terminal_ready(false);
                        thread::sleep(Duration::from_millis(600));
                        let _ = writer.write_data_terminal_ready(true);
                    }
                }
            }
        }
    })?;
    Ok((conn, CarrierWatch { online }))
}
