//! 管理コンソール (ホスト局のオペレータ画面)

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Local;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::auth;
use crate::db::repo::{self, Board, User};
use crate::hub::{LineState, SlotView};
use crate::logbuf;
use crate::session::util::fmt_elapsed;
use crate::session::Ctx;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Lines,
    Users,
    Boards,
    Monitor(u16),
}

#[derive(Clone)]
enum Action {
    Quit,
    Kick(u16),
    Broadcast,
    ToggleBan(i64),
    SetLevel(i64),
    ResetPassword(i64),
    AddBoardName,
    AddBoardTitle(String),
    DeleteBoard(i64),
}

enum Modal {
    Confirm { text: String, action: Action },
    Input { title: String, value: String, mask: bool, action: Action },
    Help,
}

struct Monitor {
    rx: broadcast::Receiver<Vec<u8>>,
    parser: vt100::Parser,
    closed: bool,
}

struct App {
    ctx: Arc<Ctx>,
    screen: Screen,
    sel: TableState,
    user_sel: TableState,
    board_sel: TableState,
    show_all: bool,
    modal: Option<Modal>,
    users: Vec<User>,
    boards: Vec<Board>,
    message: String,
    monitor: Option<Monitor>,
    quit: bool,
}

fn state_color(s: LineState) -> Color {
    match s {
        LineState::Free | LineState::Idle => Color::DarkGray,
        LineState::Init | LineState::Hangup => Color::Yellow,
        LineState::Ringing | LineState::Connecting => Color::LightMagenta,
        LineState::Login => Color::LightCyan,
        LineState::Online => Color::LightGreen,
        LineState::Error => Color::LightRed,
    }
}

impl App {
    fn lines(&self) -> Vec<SlotView> {
        let all = self.ctx.hub.snapshot();
        if self.show_all {
            all
        } else {
            all.into_iter().filter(|v| v.kind.is_some() || v.state != LineState::Free).collect()
        }
    }

    fn selected_line(&self) -> Option<SlotView> {
        self.lines().get(self.sel.selected()?).cloned()
    }

    fn reload(&mut self) {
        match self.ctx.db.call_blocking(|c| repo::list_users(c)) {
            Ok(u) => self.users = u,
            Err(e) => self.message = format!("{e:#}"),
        }
        match self.ctx.db.call_blocking(|c| repo::list_boards(c, repo::LEVEL_SYSOP, 0)) {
            Ok(b) => self.boards = b,
            Err(e) => self.message = format!("{e:#}"),
        }
    }

    fn selected_user(&self) -> Option<&User> {
        self.users.get(self.user_sel.selected()?)
    }

    fn run_action(&mut self, action: Action, input: String) {
        let db = self.ctx.db.clone();
        let result: Result<String> = match action {
            Action::Quit => {
                self.quit = true;
                Ok(String::new())
            }
            Action::Kick(no) => {
                let ok = self.ctx.hub.kick(no, "SYSOP により切断されました");
                tracing::info!("管理画面から CH{no:02} を切断");
                Ok(if ok { format!("CH{no:02} を切断しました") } else { format!("CH{no:02} は利用中ではありません") })
            }
            Action::Broadcast => {
                if input.trim().is_empty() {
                    Ok(String::new())
                } else {
                    let n = self.ctx.hub.broadcast(input.trim());
                    tracing::info!("全体放送: {}", input.trim());
                    Ok(format!("{n} 人に放送しました"))
                }
            }
            Action::ToggleBan(id) => {
                let banned = self.users.iter().find(|u| u.id == id).is_some_and(|u| u.banned);
                db.call_blocking(move |c| repo::set_banned(c, id, !banned))
                    .map(|_| if banned { "利用停止を解除しました".into() } else { "利用停止にしました".into() })
            }
            Action::SetLevel(id) => match input.trim().parse::<i64>() {
                Ok(level) => {
                    let level = level.clamp(0, 100);
                    db.call_blocking(move |c| repo::set_level(c, id, level)).map(|_| format!("レベルを {level} にしました"))
                }
                Err(_) => Err(anyhow::anyhow!("数字で入力してください")),
            },
            Action::ResetPassword(id) => {
                if input.chars().count() < 4 {
                    Err(anyhow::anyhow!("4 文字以上にしてください"))
                } else {
                    auth::hash(&input)
                        .and_then(|hash| db.call_blocking(move |c| repo::set_password(c, id, &hash)))
                        .map(|_| "パスワードを再設定しました".into())
                }
            }
            Action::AddBoardName => {
                let name = input.trim().to_uppercase();
                if name.is_empty() {
                    Err(anyhow::anyhow!("ボード名が空です"))
                } else {
                    self.modal = Some(Modal::Input {
                        title: format!("ボード {name} の表示名"),
                        value: String::new(),
                        mask: false,
                        action: Action::AddBoardTitle(name),
                    });
                    Ok(String::new())
                }
            }
            Action::AddBoardTitle(name) => {
                let title = input.trim().to_string();
                if title.is_empty() {
                    Err(anyhow::anyhow!("表示名が空です"))
                } else {
                    db.call_blocking(move |c| repo::add_board(c, &name, &title, "", 0, repo::LEVEL_MEMBER))
                        .map(|_| "ボードを追加しました".into())
                }
            }
            Action::DeleteBoard(id) => db.call_blocking(move |c| repo::delete_board(c, id)).map(|_| "ボードを削除しました".into()),
        };
        match result {
            Ok(m) if !m.is_empty() => self.message = m,
            Ok(_) => {}
            Err(e) => self.message = format!("エラー: {e:#}"),
        }
        self.reload();
    }

    fn open_monitor(&mut self, no: u16) {
        match self.ctx.hub.monitor(no) {
            Some(rx) => {
                self.monitor = Some(Monitor { rx, parser: vt100::Parser::new(24, 80, 0), closed: false });
                self.screen = Screen::Monitor(no);
            }
            None => self.message = format!("CH{no:02} は接続していません"),
        }
    }

    fn pump_monitor(&mut self) {
        if let Some(m) = self.monitor.as_mut() {
            loop {
                match m.rx.try_recv() {
                    Ok(b) => m.parser.process(&b),
                    Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                    Err(broadcast::error::TryRecvError::Empty) => break,
                    Err(broadcast::error::TryRecvError::Closed) => {
                        m.closed = true;
                        break;
                    }
                }
            }
        }
    }

    fn key(&mut self, k: KeyEvent) {
        if let Some(modal) = self.modal.take() {
            match modal {
                Modal::Help => {}
                Modal::Confirm { text, action } => match k.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.run_action(action, String::new()),
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                    _ => self.modal = Some(Modal::Confirm { text, action }),
                },
                Modal::Input { title, mut value, mask, action } => match k.code {
                    KeyCode::Enter => self.run_action(action, value),
                    KeyCode::Esc => {}
                    KeyCode::Backspace => {
                        value.pop();
                        self.modal = Some(Modal::Input { title, value, mask, action });
                    }
                    KeyCode::Char(c) => {
                        value.push(c);
                        self.modal = Some(Modal::Input { title, value, mask, action });
                    }
                    _ => self.modal = Some(Modal::Input { title, value, mask, action }),
                },
            }
            return;
        }
        let input = |title: &str, action: Action| Modal::Input { title: title.into(), value: String::new(), mask: false, action };
        let c = match k.code {
            KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
            _ => None,
        };
        match self.screen {
            Screen::Lines => match (k.code, c) {
                (_, Some('?')) => self.modal = Some(Modal::Help),
                (_, Some('q')) => {
                    self.modal = Some(Modal::Confirm {
                        text: "NULL BBS を終了しますか? (接続中の利用者は切断されます)".into(),
                        action: Action::Quit,
                    })
                }
                (KeyCode::Up, _) => self.sel.select_previous(),
                (KeyCode::Down, _) => self.sel.select_next(),
                (KeyCode::PageUp, _) => self.sel.scroll_up_by(10),
                (KeyCode::PageDown, _) => self.sel.scroll_down_by(10),
                (_, Some('a')) => self.show_all = !self.show_all,
                (KeyCode::Enter, _) | (_, Some('m')) => {
                    if let Some(v) = self.selected_line() {
                        self.open_monitor(v.no);
                    }
                }
                (_, Some('k')) => {
                    if let Some(v) = self.selected_line().filter(|v| v.user_id.is_some() || v.state == LineState::Login) {
                        self.modal = Some(Modal::Confirm { text: format!("CH{:02} を切断しますか?", v.no), action: Action::Kick(v.no) });
                    }
                }
                (_, Some('b')) => self.modal = Some(input("全体放送する文", Action::Broadcast)),
                (_, Some('u')) => {
                    self.reload();
                    self.screen = Screen::Users;
                }
                (_, Some('o')) => {
                    self.reload();
                    self.screen = Screen::Boards;
                }
                _ => {}
            },
            Screen::Monitor(no) => match (k.code, c) {
                (KeyCode::Esc, _) | (_, Some('q')) => {
                    self.screen = Screen::Lines;
                    self.monitor = None;
                }
                (_, Some('k')) => {
                    self.modal = Some(Modal::Confirm { text: format!("CH{no:02} を切断しますか?"), action: Action::Kick(no) })
                }
                (_, Some('b')) => self.modal = Some(input("全体放送する文", Action::Broadcast)),
                _ => {}
            },
            Screen::Users => match (k.code, c) {
                (KeyCode::Esc, _) | (_, Some('q')) => self.screen = Screen::Lines,
                (KeyCode::Up, _) => self.user_sel.select_previous(),
                (KeyCode::Down, _) => self.user_sel.select_next(),
                (_, Some('b')) => {
                    if let Some(u) = self.selected_user() {
                        let t = if u.banned { "利用停止を解除" } else { "利用停止に" };
                        self.modal = Some(Modal::Confirm {
                            text: format!("{} ({}) を{t}しますか?", u.handle, u.login),
                            action: Action::ToggleBan(u.id),
                        });
                    }
                }
                (_, Some('l')) => {
                    if let Some(u) = self.selected_user() {
                        self.modal = Some(input(
                            &format!("{} の新しいレベル (0:ゲスト 10:会員 50:サブ 100:SYSOP)", u.login),
                            Action::SetLevel(u.id),
                        ));
                    }
                }
                (_, Some('p')) => {
                    if let Some(u) = self.selected_user() {
                        self.modal = Some(Modal::Input {
                            title: format!("{} の新しいパスワード", u.login),
                            value: String::new(),
                            mask: true,
                            action: Action::ResetPassword(u.id),
                        });
                    }
                }
                _ => {}
            },
            Screen::Boards => match (k.code, c) {
                (KeyCode::Esc, _) | (_, Some('q')) => self.screen = Screen::Lines,
                (KeyCode::Up, _) => self.board_sel.select_previous(),
                (KeyCode::Down, _) => self.board_sel.select_next(),
                (_, Some('a')) => self.modal = Some(input("新しいボード名 (英数字)", Action::AddBoardName)),
                (_, Some('d')) => {
                    if let Some(b) = self.board_sel.selected().and_then(|i| self.boards.get(i)) {
                        self.modal = Some(Modal::Confirm {
                            text: format!("ボード「{}」を削除しますか?", b.title),
                            action: Action::DeleteBoard(b.id),
                        });
                    }
                }
                _ => {}
            },
        }
    }
}

pub fn run(ctx: Arc<Ctx>, stop: CancellationToken) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App {
        ctx,
        screen: Screen::Lines,
        sel: TableState::default().with_selected(Some(0)),
        user_sel: TableState::default().with_selected(Some(0)),
        board_sel: TableState::default().with_selected(Some(0)),
        show_all: false,
        modal: None,
        users: Vec::new(),
        boards: Vec::new(),
        message: "?:ヘルプ".into(),
        monitor: None,
        quit: false,
    };
    app.reload();
    let result = (|| -> Result<()> {
        let mut last = Instant::now() - Duration::from_secs(1);
        while !app.quit && !stop.is_cancelled() {
            app.pump_monitor();
            if last.elapsed() >= Duration::from_millis(250) {
                terminal.draw(|f| draw(f, &mut app))?;
                last = Instant::now();
            }
            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind != KeyEventKind::Release {
                        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
                            app.modal = Some(Modal::Confirm { text: "NULL BBS を終了しますか?".into(), action: Action::Quit });
                        } else {
                            app.key(k);
                        }
                        terminal.draw(|f| draw(f, &mut app))?;
                        last = Instant::now();
                    }
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

// ---------------------------------------------------------------- 描画

fn human(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1}M", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{:.1}K", n as f64 / 1024.0)
    } else {
        n.to_string()
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, log, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(6),
        Constraint::Length(if matches!(app.screen, Screen::Monitor(_)) { 0 } else { 9 }),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let snapshot = app.ctx.hub.snapshot();
    let online = snapshot.iter().filter(|v| v.user_id.is_some()).count();
    let busy = snapshot
        .iter()
        .filter(|v| !matches!(v.state, LineState::Free | LineState::Idle | LineState::Init))
        .count();
    let head = format!(
        " {}  稼働 {}  利用中 {online}人  使用回線 {busy}/{}  着信 {}件  {}",
        app.ctx.hub.bbs_name,
        fmt_elapsed(app.ctx.hub.started.elapsed().as_secs() as i64),
        app.ctx.hub.max_lines(),
        app.ctx.hub.calls.load(std::sync::atomic::Ordering::Relaxed),
        Local::now().format("%Y/%m/%d %H:%M:%S")
    );
    f.render_widget(
        Paragraph::new(head).style(Style::new().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD)),
        header,
    );

    match app.screen {
        Screen::Lines => draw_lines(f, app, body),
        Screen::Users => draw_users(f, app, body),
        Screen::Boards => draw_boards(f, app, body),
        Screen::Monitor(no) => draw_monitor(f, app, body, no),
    }

    if log.height > 0 {
        let lines: Vec<Line> = logbuf::tail(log.height.saturating_sub(2) as usize).into_iter().map(Line::raw).collect();
        f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" システムログ ")), log);
    }

    let keys = match app.screen {
        Screen::Lines => "↑↓:選択 Enter:モニタ K:切断 B:全体放送 U:会員 O:ボード A:全回線 ?:ヘルプ Q:終了",
        Screen::Users => "↑↓:選択 B:利用停止/解除 L:レベル P:パスワード再設定 Esc:戻る",
        Screen::Boards => "↑↓:選択 A:追加 D:削除 Esc:戻る",
        Screen::Monitor(_) => "K:この回線を切断 B:全体放送 Esc:戻る",
    };
    let foot = Line::from(vec![
        Span::styled(format!(" {keys} "), Style::new().fg(Color::Black).bg(Color::Gray)),
        Span::styled(format!(" {}", app.message), Style::new().fg(Color::Yellow)),
    ]);
    f.render_widget(Paragraph::new(foot), footer);

    if let Some(m) = &app.modal {
        draw_modal(f, m);
    }
}

fn header_style() -> Style {
    Style::new().add_modifier(Modifier::BOLD).fg(Color::Cyan)
}

fn highlight() -> Style {
    Style::new().bg(Color::Rgb(40, 60, 110))
}

fn draw_lines(f: &mut Frame, app: &mut App, area: Rect) {
    let lines = app.lines();
    let now = Local::now();
    let rows: Vec<Row> = lines
        .iter()
        .map(|v| {
            let kind = v.kind.map(|k| k.label()).unwrap_or("-");
            let port = if v.port.is_empty() { v.peer.clone() } else { v.port.clone() };
            let time = v.connected_at.map(|t| fmt_elapsed((now - t).num_seconds())).unwrap_or_default();
            let state = if v.note.is_empty() { v.state.label().to_string() } else { format!("{} {}", v.state.label(), v.note) };
            Row::new(vec![
                Cell::from(format!("{:02}", v.no)),
                Cell::from(kind),
                Cell::from(port),
                Cell::from(state).style(Style::new().fg(state_color(v.state))),
                Cell::from(v.login.clone()),
                Cell::from(v.handle.clone()),
                Cell::from(v.speed.clone()),
                Cell::from(v.place.clone()),
                Cell::from(time),
                Cell::from(human(v.rx)),
                Cell::from(human(v.tx)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(3),
        Constraint::Length(6),
        Constraint::Length(24),
        Constraint::Length(14),
        Constraint::Length(12),
        Constraint::Length(14),
        Constraint::Length(16),
        Constraint::Min(14),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(7),
    ];
    let title = if app.show_all { " 回線 (全回線) " } else { " 回線 (使用中とモデム回線。A で全回線) " };
    let table = Table::new(rows, widths)
        .header(Row::new(["CH", "種別", "ポート/接続元", "状態", "ID", "ハンドル", "速度", "場所", "時間", "RX", "TX"]).style(header_style()))
        .row_highlight_style(highlight())
        .block(Block::default().borders(Borders::ALL).title(title));
    // 行が 0 のときに描くと選択が外れるので、行ができたら先頭を選び直す
    match app.sel.selected() {
        None if !lines.is_empty() => app.sel.select(Some(0)),
        Some(s) if s >= lines.len() && !lines.is_empty() => app.sel.select(Some(lines.len() - 1)),
        _ => {}
    }
    f.render_stateful_widget(table, area, &mut app.sel);
}

fn draw_users(f: &mut Frame, app: &mut App, area: Rect) {
    let rows: Vec<Row> = app
        .users
        .iter()
        .map(|u| {
            let last = u.last_login_at.map(crate::session::util::fmt_time).unwrap_or_default();
            let style = if u.banned { Style::new().fg(Color::LightRed) } else { Style::new() };
            Row::new(vec![
                u.id.to_string(),
                u.login.clone(),
                u.handle.clone(),
                u.level.to_string(),
                if u.banned { "停止中".into() } else { String::new() },
                u.login_count.to_string(),
                last,
            ])
            .style(style)
        })
        .collect();
    let widths = [
        Constraint::Length(5),
        Constraint::Length(14),
        Constraint::Length(18),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Min(18),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(["ID", "ログイン", "ハンドル", "レベル", "状態", "回数", "最終ログイン"]).style(header_style()))
        .row_highlight_style(highlight())
        .block(Block::default().borders(Borders::ALL).title(format!(" 会員 {}人 ", app.users.len())));
    if app.user_sel.selected().is_none() && !app.users.is_empty() {
        app.user_sel.select(Some(0));
    }
    f.render_stateful_widget(table, area, &mut app.user_sel);
}

fn draw_boards(f: &mut Frame, app: &mut App, area: Rect) {
    let rows: Vec<Row> = app
        .boards
        .iter()
        .map(|b| {
            Row::new(vec![
                b.name.clone(),
                b.title.clone(),
                b.count.to_string(),
                b.read_level.to_string(),
                b.write_level.to_string(),
                b.description.clone(),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(12),
        Constraint::Length(20),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Min(20),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(["名前", "表示名", "記事", "読レベル", "書レベル", "説明"]).style(header_style()))
        .row_highlight_style(highlight())
        .block(Block::default().borders(Borders::ALL).title(" ボード "));
    if app.board_sel.selected().is_none() && !app.boards.is_empty() {
        app.board_sel.select(Some(0));
    }
    f.render_stateful_widget(table, area, &mut app.board_sel);
}

fn draw_monitor(f: &mut Frame, app: &mut App, area: Rect, no: u16) {
    let Some(m) = app.monitor.as_mut() else { return };
    let title = format!(" CH{no:02} モニタ{} ", if m.closed { " (切断されました)" } else { "" });
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let (rows, cols) = (inner.height.min(100), inner.width.min(200));
    if m.parser.screen().size() != (rows, cols) && rows > 0 && cols > 0 {
        m.parser.set_size(rows, cols);
    }
    render_screen(m.parser.screen(), inner, f.buffer_mut());
}

fn vt_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// vt100 の画面をそのまま描く (null-term と同じ)
fn render_screen(screen: &vt100::Screen, area: Rect, buf: &mut Buffer) {
    for row in 0..area.height {
        for col in 0..area.width {
            let Some(cell) = screen.cell(row, col) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::new().fg(vt_color(cell.fgcolor())).bg(vt_color(cell.bgcolor()));
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.inverse() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let contents = cell.contents();
            let sym = if contents.is_empty() { " " } else { contents.as_str() };
            let (x, y) = (area.x + col, area.y + row);
            if cell.is_wide() && col + 1 >= area.width {
                buf[(x, y)].set_symbol(" ").set_style(style);
            } else {
                buf.set_stringn(x, y, sym, (area.width - col) as usize, style);
            }
        }
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn draw_modal(f: &mut Frame, m: &Modal) {
    let style = Style::new().bg(Color::Black).fg(Color::White);
    match m {
        Modal::Confirm { text, .. } => {
            let area = centered(f.area(), 70, 6);
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(format!("\n {text}\n\n Y:はい  N:いいえ"))
                    .block(Block::default().borders(Borders::ALL).title(" 確認 "))
                    .style(style),
                area,
            );
        }
        Modal::Input { title, value, mask, .. } => {
            let area = centered(f.area(), 70, 5);
            f.render_widget(Clear, area);
            let shown = if *mask { "*".repeat(value.chars().count()) } else { value.clone() };
            f.render_widget(
                Paragraph::new(format!("\n {shown}_\n Enter:決定 Esc:取消"))
                    .block(Block::default().borders(Borders::ALL).title(format!(" {title} ")))
                    .style(style),
                area,
            );
        }
        Modal::Help => {
            let text = [
                "",
                " 回線画面",
                "   ↑↓ / PgUp PgDn   回線を選ぶ",
                "   Enter / M         選んだ回線の画面をモニタ",
                "   K                 選んだ回線を切断",
                "   B                 全体放送",
                "   U                 会員管理 (利用停止・レベル・パスワード)",
                "   O                 ボード管理 (追加・削除)",
                "   A                 全回線 / 使用中の回線だけ を切り替え",
                "   Q                 終了",
                "",
                " 何かキーを押すと閉じます",
            ];
            let area = centered(f.area(), 60, text.len() as u16 + 2);
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(text.join("\n")).block(Block::default().borders(Borders::ALL).title(" ヘルプ ")).style(style),
                area,
            );
        }
    }
}
