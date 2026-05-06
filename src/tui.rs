use crate::{
    highlight::{
        TriggerState, dispatch_tui_triggers, evaluate_lines, filter_new_triggers,
        write_manual_screenshot,
    },
    model::{BindingAction, KeyBinding, Rgb, Runtime, ScreenLine},
    support::{exit_with_status, spawn_direct},
    transform::{split_cells_by_display_width, transform_line},
};
use anyhow::{Result, anyhow};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    execute,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
#[cfg(unix)]
use signal_hook::{consts::signal::SIGWINCH, iterator::Signals};
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Read, Write},
    sync::mpsc,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const ALT_SCREEN_ENTER_SEQUENCES: &[&[u8]] = &[b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];
const LIVE_MOUSE_ENABLE_SEQUENCES: &[&[u8]] = &[
    b"\x1b[?1000h",
    b"\x1b[?1002h",
    b"\x1b[?1003h",
    b"\x1b[?1005h",
    b"\x1b[?1006h",
    b"\x1b[?1007h",
    b"\x1b[?1015h",
];
const LIVE_MOUSE_DISABLE_SEQUENCES: &[&[u8]] = &[
    b"\x1b[?1000l",
    b"\x1b[?1002l",
    b"\x1b[?1003l",
    b"\x1b[?1005l",
    b"\x1b[?1006l",
    b"\x1b[?1007l",
    b"\x1b[?1015l",
];
const LIVE_INPUT_MODE_SEQUENCES: &[&[u8]] = &[
    b"\x1b[?1h",
    b"\x1b=",
    b"\x1b[?1l",
    b"\x1b>",
    b"\x1b[?2004h",
    b"\x1b[?2004l",
];
const KEY_BIND_PENDING_TIMEOUT: Duration = Duration::from_millis(35);

pub(crate) fn run(rt: Runtime) -> Result<()> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return spawn_direct(&rt.command);
    }

    let (cols, rows) = size().unwrap_or((80, 24));
    let mut session = PtySession::spawn(&rt.command, rows, cols)?;
    let initial = capture_initial_state(&rt, &mut session.reader, rows, cols)?;

    let _guard = TerminalGuard::enter()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        Clear(ClearType::All),
        MoveTo(0, 0)
    )?;

    let passthrough = Arc::new(Mutex::new(LivePassthrough::default()));
    if !initial.captured.is_empty() {
        passthrough
            .lock()
            .expect("passthrough mutex should lock")
            .feed(&mut stdout, &initial.captured)?;
    }

    draw_screen(
        &mut stdout,
        &initial.cells,
        &initial.highlights.colors,
        &initial.state,
    )?;

    let writer = Arc::new(Mutex::new(session.writer));
    let (control_tx, control_rx) = mpsc::channel();
    let _input_handle = spawn_input_forwarder(writer.clone(), rt.key_bindings.clone(), control_tx);
    let reader_rx = spawn_reader(session.reader);

    let mut trigger_state = initial.trigger_state;
    let mut current_cells = initial.cells;
    let mut current_highlight_colors = initial.highlights.colors;
    let mut current_state = initial.state;
    let mut parser = initial.parser;

    loop {
        while let Ok(ControlEvent::ManualScreenshot) = control_rx.try_recv() {
            let svg = screen_to_svg(&current_cells, Some(&current_highlight_colors));
            write_manual_screenshot(&rt.screenshot_dir, rt.screenshot_prefix.as_deref(), &svg)?;
            flash_screen(
                &mut stdout,
                &current_cells,
                &current_highlight_colors,
                &current_state,
            )?;
        }

        match reader_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ReaderEvent::Chunk(chunk)) => {
                passthrough
                    .lock()
                    .expect("passthrough mutex should lock")
                    .feed(&mut stdout, &chunk)?;
                parser.process(&chunk);
                let snapshot = snapshot_from_parser(&parser, &rt);
                current_cells = snapshot.cells;
                current_state = snapshot.state;
                let evaluation = snapshot.highlights;
                let new_triggers =
                    filter_new_triggers(&mut trigger_state, evaluation.triggers.clone());
                if !new_triggers.is_empty() {
                    let svg = screen_to_svg(&current_cells, Some(&evaluation.colors));
                    let capture_paths = dispatch_tui_triggers(
                        &rt.screenshot_dir,
                        rt.screenshot_prefix.as_deref(),
                        &new_triggers,
                        &svg,
                    )?;
                    if !capture_paths.is_empty() {
                        flash_screen(
                            &mut stdout,
                            &current_cells,
                            &evaluation.colors,
                            &current_state,
                        )?;
                    }
                }
                current_highlight_colors = evaluation.colors.clone();
                draw_screen(
                    &mut stdout,
                    &current_cells,
                    &evaluation.colors,
                    &current_state,
                )?;
            }
            Ok(ReaderEvent::Eof) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let status = session.child.wait()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    exit_with_status(status.exit_code() as i32);
}

struct InitialState {
    parser: vt100::Parser,
    captured: Vec<u8>,
    cells: Vec<Vec<StyledCell>>,
    state: TerminalState,
    highlights: crate::highlight::HighlightEvaluation,
    trigger_state: TriggerState,
}

fn capture_initial_state(
    rt: &Runtime,
    reader: &mut Box<dyn Read + Send>,
    rows: u16,
    cols: u16,
) -> Result<InitialState> {
    let mut parser = vt100::Parser::new(rows, cols, 0);
    let deadline = Instant::now() + Duration::from_millis(rt.startup_capture_ms);
    let mut buf = [0u8; 8192];
    let mut captured = Vec::new();
    while Instant::now() < deadline {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                parser.process(&buf[..n]);
                captured.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if is_transient_read_error(&e) => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(_) => break,
        }
    }

    let snapshot = snapshot_from_parser(&parser, rt);
    let mut trigger_state = TriggerState::default();
    let new_triggers =
        filter_new_triggers(&mut trigger_state, snapshot.highlights.triggers.clone());
    if !new_triggers.is_empty() {
        let svg = screen_to_svg(&snapshot.cells, Some(&snapshot.highlights.colors));
        let capture_paths = dispatch_tui_triggers(
            &rt.screenshot_dir,
            rt.screenshot_prefix.as_deref(),
            &new_triggers,
            &svg,
        )?;
        if !capture_paths.is_empty() {
            // No flash here because the TUI is not visible yet.
        }
    }

    Ok(InitialState {
        parser,
        captured,
        cells: snapshot.cells,
        state: snapshot.state,
        highlights: snapshot.highlights,
        trigger_state,
    })
}

struct ScreenSnapshot {
    cells: Vec<Vec<StyledCell>>,
    state: TerminalState,
    highlights: crate::highlight::HighlightEvaluation,
}

fn snapshot_from_parser(parser: &vt100::Parser, rt: &Runtime) -> ScreenSnapshot {
    let rows = parser.screen().size().0;
    let cols = parser.screen().size().1;
    let mut cells = collect_screen(parser.screen(), rows, cols);
    apply_transforms_to_cells(&mut cells, &rt.output_transforms);
    let state = collect_terminal_state(parser.screen(), rows, cols);
    let highlights = evaluate_lines(&screen_lines(&cells), &rt.highlight_rules);
    ScreenSnapshot {
        cells,
        state,
        highlights,
    }
}

struct PtySession {
    _pty: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    _resize_watcher: ResizeWatcher,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}

impl PtySession {
    fn spawn(command: &[OsString], rows: u16, cols: u16) -> Result<Self> {
        if command.is_empty() {
            return Err(anyhow!("no command specified"));
        }
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new(&command[0]);
        for arg in &command[1..] {
            cmd.arg(arg);
        }
        cmd.env("TERM", preferred_term());
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let master = pair.master;
        let reader = master.try_clone_reader()?;
        let writer = master.take_writer()?;
        let pty = Arc::new(Mutex::new(master));
        let resize_watcher = ResizeWatcher::spawn(pty.clone());
        Ok(Self {
            _pty: pty,
            _resize_watcher: resize_watcher,
            child,
            reader,
            writer,
        })
    }
}

fn preferred_term() -> String {
    match std::env::var("TERM") {
        Ok(term) if !term.is_empty() && term != "dumb" => term,
        _ => "xterm-256color".to_string(),
    }
}

#[derive(Debug)]
enum ControlEvent {
    ManualScreenshot,
}

enum ReaderEvent {
    Chunk(Vec<u8>),
    Eof,
}

fn spawn_input_forwarder(
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    key_bindings: Vec<KeyBinding>,
    control_tx: mpsc::Sender<ControlEvent>,
) -> thread::JoinHandle<io::Result<()>> {
    thread::spawn(move || -> io::Result<()> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();

        let _reader_handle = thread::spawn(move || -> io::Result<()> {
            let mut stdin = io::stdin();
            let mut buf = [0u8; 1024];
            loop {
                let n = stdin.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Ok(())
        });

        let mut pending = Vec::new();
        loop {
            match rx.recv_timeout(KEY_BIND_PENDING_TIMEOUT) {
                Ok(chunk) => {
                    pending.extend_from_slice(&chunk);
                    process_pending_input(
                        &mut pending,
                        &key_bindings,
                        &writer,
                        &control_tx,
                        false,
                    )?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    process_pending_input(&mut pending, &key_bindings, &writer, &control_tx, true)?;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    process_pending_input(&mut pending, &key_bindings, &writer, &control_tx, true)?;
                    break;
                }
            }
        }
        Ok(())
    })
}

fn process_pending_input(
    pending: &mut Vec<u8>,
    key_bindings: &[KeyBinding],
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
    control_tx: &mpsc::Sender<ControlEvent>,
    flush_partial: bool,
) -> io::Result<()> {
    loop {
        if pending.is_empty() {
            return Ok(());
        }

        if let Some(binding) = key_bindings
            .iter()
            .find(|binding| pending.starts_with(&binding.trigger.bytes))
        {
            let consumed = binding.trigger.bytes.len();
            match &binding.action {
                BindingAction::Screenshot => {
                    let _ = control_tx.send(ControlEvent::ManualScreenshot);
                }
                BindingAction::Send(bytes) => {
                    let mut w = writer.lock().expect("writer mutex should lock");
                    w.write_all(bytes)?;
                    w.flush()?;
                }
            }
            pending.drain(..consumed);
            continue;
        }

        let could_extend = key_bindings
            .iter()
            .any(|binding| binding.trigger.bytes.starts_with(&pending[..]));
        if could_extend && !flush_partial {
            return Ok(());
        }

        let byte = pending.remove(0);
        let mut w = writer.lock().expect("writer mutex should lock");
        w.write_all(&[byte])?;
        w.flush()?;
    }
}

fn spawn_reader(mut reader: Box<dyn Read + Send>) -> mpsc::Receiver<ReaderEvent> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(ReaderEvent::Eof);
                    break;
                }
                Ok(n) => {
                    if tx.send(ReaderEvent::Chunk(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if is_transient_read_error(&e) => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(_) => {
                    let _ = tx.send(ReaderEvent::Eof);
                    break;
                }
            }
        }
    });
    rx
}

fn is_transient_read_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

struct TerminalGuard {
    raw_mode_enabled: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), Hide)?;
        Ok(Self {
            raw_mode_enabled: true,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, crossterm::style::ResetColor);
        if self.raw_mode_enabled {
            let _ = disable_raw_mode();
        }
    }
}

struct ResizeWatcher {
    #[cfg(unix)]
    handle: signal_hook::iterator::Handle,
    join: Option<thread::JoinHandle<()>>,
}

impl ResizeWatcher {
    fn spawn(pty: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>) -> Self {
        #[cfg(unix)]
        {
            let mut signals = Signals::new([SIGWINCH]).expect("failed to register SIGWINCH");
            let handle = signals.handle();
            let join = thread::spawn(move || {
                for _ in signals.forever() {
                    if let Ok((cols, rows)) = size() {
                        let _ = pty.lock().expect("pty mutex should lock").resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                }
            });
            Self {
                handle,
                join: Some(join),
            }
        }

        #[cfg(not(unix))]
        {
            let _ = pty;
            Self { join: None }
        }
    }
}

impl Drop for ResizeWatcher {
    fn drop(&mut self) {
        #[cfg(unix)]
        self.handle.close();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StyledCell {
    text: String,
    fg: DisplayColor,
    bg: DisplayColor,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    wide_continuation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayColor {
    Default,
    Indexed(u8),
    Rgb(Rgb),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalState {
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
}

fn collect_terminal_state(screen: &vt100::Screen, rows: u16, cols: u16) -> TerminalState {
    let (cursor_row, cursor_col) = screen.cursor_position();
    TerminalState {
        cursor_row: cursor_row.min(rows.saturating_sub(1)),
        cursor_col: cursor_col.min(cols.saturating_sub(1)),
        cursor_visible: !screen.hide_cursor(),
    }
}

fn collect_screen(screen: &vt100::Screen, rows: u16, cols: u16) -> Vec<Vec<StyledCell>> {
    let mut result = Vec::new();
    for row in 0..rows {
        let mut out_row = Vec::new();
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                out_row.push(StyledCell {
                    text: if cell.has_contents() {
                        cell.contents().to_string()
                    } else {
                        " ".to_string()
                    },
                    fg: display_vt_color(cell.fgcolor()),
                    bg: display_vt_color(cell.bgcolor()),
                    bold: cell.bold(),
                    dim: cell.dim(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                    inverse: cell.inverse(),
                    wide_continuation: cell.is_wide_continuation(),
                });
            } else {
                out_row.push(StyledCell::blank());
            }
        }
        result.push(out_row);
    }
    result
}

fn screen_lines(cells: &[Vec<StyledCell>]) -> Vec<ScreenLine> {
    cells.iter().map(|row| screen_line_from_row(row)).collect()
}

fn screen_line_from_row(row: &[StyledCell]) -> ScreenLine {
    let mut text = String::new();
    let mut char_spans = Vec::new();
    let mut cell_col = 0usize;

    for (idx, cell) in row.iter().enumerate() {
        if cell.wide_continuation {
            continue;
        }

        let cell_width = row
            .get(idx + 1)
            .is_some_and(|next| next.wide_continuation)
            .then_some(2)
            .unwrap_or(1);
        let mut char_col = cell_col;
        for ch in cell.text.chars() {
            let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            let end = if width == 0 {
                char_col
            } else {
                (char_col + width).min(cell_col + cell_width)
            };
            char_spans.push((char_col, end));
            if width > 0 {
                char_col = end;
            }
        }

        text.push_str(&cell.text);
        cell_col += cell_width;
    }

    while text.ends_with(' ') {
        text.pop();
        let _ = char_spans.pop();
    }

    ScreenLine {
        text,
        char_spans,
        cell_count: cell_col,
    }
}

fn screen_to_svg(
    cells: &[Vec<StyledCell>],
    highlight_colors: Option<&[Vec<Option<Rgb>>]>,
) -> String {
    let cell_width = 9usize;
    let cell_height = 18usize;
    let width = cells.first().map(|row| row.len()).unwrap_or(0) * cell_width;
    let height = cells.len() * cell_height;
    let mut svg = String::new();
    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">"#
    ));
    svg.push_str(r##"<rect width="100%" height="100%" fill="#101010"/>"##);
    svg.push_str(r#"<g font-family="monospace" font-size="14" dominant-baseline="hanging">"#);
    for (row_idx, row) in cells.iter().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            if cell.wide_continuation {
                continue;
            }
            let x = col_idx * cell_width;
            let y = row_idx * cell_height;
            let bg = highlight_colors
                .and_then(|rows| rows.get(row_idx))
                .and_then(|row_colors| row_colors.get(col_idx))
                .copied()
                .flatten()
                .map(rgb_hex)
                .unwrap_or_else(|| display_bg_hex(cell.bg));
            if bg != "#00000000" && bg != "#000000" {
                svg.push_str(&format!(
                    r#"<rect x="{x}" y="{y}" width="{cell_width}" height="{cell_height}" fill="{bg}"/>"#
                ));
            }
            let fill = display_color_hex(cell.fg);
            let text = svg_escape(&cell.text);
            svg.push_str(&format!(
                r#"<text x="{x}" y="{y}" fill="{fill}">{text}</text>"#
            ));
        }
    }
    svg.push_str("</g></svg>");
    svg
}

fn draw_screen(
    out: &mut io::Stdout,
    cells: &[Vec<StyledCell>],
    highlight_colors: &[Vec<Option<Rgb>>],
    state: &TerminalState,
) -> Result<()> {
    execute!(out, MoveTo(0, 0), Clear(ClearType::All))?;
    let mut style_state = DrawStyle::default();
    for (row_idx, row) in cells.iter().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            if cell.wide_continuation {
                continue;
            }
            let highlight_bg = highlight_colors
                .get(row_idx)
                .and_then(|colors| colors.get(col_idx))
                .copied()
                .flatten();
            write_cell(out, cell, highlight_bg, &mut style_state)?;
        }
        if row_idx + 1 < cells.len() {
            write!(out, "\r\n")?;
        }
    }

    write!(out, "\x1b[0m")?;
    if state.cursor_visible {
        execute!(out, Show, MoveTo(state.cursor_col, state.cursor_row))?;
    } else {
        execute!(out, Hide)?;
    }
    out.flush()?;
    Ok(())
}

fn flash_screen(
    out: &mut io::Stdout,
    cells: &[Vec<StyledCell>],
    highlight_colors: &[Vec<Option<Rgb>>],
    state: &TerminalState,
) -> Result<()> {
    execute!(out, MoveTo(0, 0), Clear(ClearType::All))?;
    for (row_idx, row) in cells.iter().enumerate() {
        write!(out, "\x1b[38;2;16;16;16m\x1b[48;2;245;245;245m")?;
        for cell in row {
            if cell.wide_continuation {
                continue;
            }
            write!(out, "{}", cell.text)?;
        }
        write!(out, "\x1b[0m")?;
        if row_idx + 1 < cells.len() {
            write!(out, "\r\n")?;
        }
    }
    out.flush()?;
    thread::sleep(Duration::from_millis(60));
    draw_screen(out, cells, highlight_colors, state)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DrawStyle {
    fg: Option<DisplayColor>,
    bg: Option<DisplayColor>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

fn write_cell(
    out: &mut io::Stdout,
    cell: &StyledCell,
    highlight_bg: Option<Rgb>,
    style_state: &mut DrawStyle,
) -> Result<()> {
    let (fg, bg) = resolved_colors(cell, highlight_bg);
    let target = DrawStyle {
        fg: Some(fg),
        bg: Some(bg),
        bold: cell.bold,
        dim: cell.dim,
        italic: cell.italic,
        underline: cell.underline,
    };
    if *style_state != target {
        write!(out, "\x1b[0m")?;
        write_color(out, target.fg.expect("fg should exist"), true)?;
        write_color(out, target.bg.expect("bg should exist"), false)?;
        if target.bold {
            write!(out, "\x1b[1m")?;
        }
        if target.dim {
            write!(out, "\x1b[2m")?;
        }
        if target.italic {
            write!(out, "\x1b[3m")?;
        }
        if target.underline {
            write!(out, "\x1b[4m")?;
        }
        *style_state = target;
    }
    write!(out, "{}", cell.text)?;
    Ok(())
}

fn resolved_colors(cell: &StyledCell, highlight_bg: Option<Rgb>) -> (DisplayColor, DisplayColor) {
    let mut fg = cell.fg;
    let mut bg = highlight_bg.map(DisplayColor::Rgb).unwrap_or(cell.bg);
    if cell.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    (fg, bg)
}

fn write_color(out: &mut io::Stdout, color: DisplayColor, foreground: bool) -> io::Result<()> {
    match color {
        DisplayColor::Default => write!(out, "\x1b[{}m", if foreground { 39 } else { 49 }),
        DisplayColor::Indexed(idx) => {
            write!(out, "\x1b[{};5;{}m", if foreground { 38 } else { 48 }, idx)
        }
        DisplayColor::Rgb(Rgb(r, g, b)) => write!(
            out,
            "\x1b[{};2;{};{};{}m",
            if foreground { 38 } else { 48 },
            r,
            g,
            b
        ),
    }
}

fn display_vt_color(color: vt100::Color) -> DisplayColor {
    match color {
        vt100::Color::Default => DisplayColor::Default,
        vt100::Color::Idx(idx) => DisplayColor::Indexed(idx),
        vt100::Color::Rgb(r, g, b) => DisplayColor::Rgb(Rgb(r, g, b)),
    }
}

impl StyledCell {
    fn blank() -> Self {
        Self {
            text: " ".to_string(),
            fg: DisplayColor::Default,
            bg: DisplayColor::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            wide_continuation: false,
        }
    }
}

fn apply_transforms_to_cells(
    cells: &mut [Vec<StyledCell>],
    rules: &[crate::model::OutputTransformRule],
) {
    if rules.is_empty() {
        return;
    }

    for row in cells.iter_mut() {
        let visible_indexes = row
            .iter()
            .enumerate()
            .filter_map(|(idx, cell)| (!cell.wide_continuation).then_some(idx))
            .collect::<Vec<_>>();
        let visible_widths = visible_indexes
            .iter()
            .map(|&idx| {
                row.get(idx + 1)
                    .is_some_and(|next| next.wide_continuation)
                    .then_some(2)
                    .unwrap_or(1)
            })
            .collect::<Vec<_>>();
        let plain = visible_indexes
            .iter()
            .map(|&idx| row[idx].text.as_str())
            .collect::<String>();
        let transformed = transform_line(&plain, rules);
        let transformed_cells = split_cells_by_display_width(&transformed, &visible_widths);
        for (cell_idx, text) in visible_indexes.into_iter().zip(transformed_cells) {
            row[cell_idx].text = text;
        }
    }
}

#[derive(Default)]
struct LivePassthrough {
    pending: Vec<u8>,
}

impl LivePassthrough {
    fn feed<W: Write>(&mut self, out: &mut W, bytes: &[u8]) -> io::Result<()> {
        self.pending.extend_from_slice(bytes);
        let keep = longest_passthrough_suffix(&self.pending);
        let split_at = self.pending.len().saturating_sub(keep);
        let scan = self.pending[..split_at].to_vec();
        self.pending = self.pending[split_at..].to_vec();

        let mut wrote_any = false;
        let mut i = 0;
        while i < scan.len() {
            if let Some(pattern) = passthrough_match(&scan[i..]) {
                out.write_all(pattern)?;
                wrote_any = true;
                i += pattern.len();
            } else {
                i += 1;
            }
        }
        if wrote_any {
            out.flush()?;
        }
        Ok(())
    }
}

fn passthrough_match(bytes: &[u8]) -> Option<&'static [u8]> {
    for pattern in ALT_SCREEN_ENTER_SEQUENCES
        .iter()
        .chain(LIVE_MOUSE_ENABLE_SEQUENCES.iter())
        .chain(LIVE_MOUSE_DISABLE_SEQUENCES.iter())
        .chain(LIVE_INPUT_MODE_SEQUENCES.iter())
    {
        if bytes.starts_with(pattern) {
            return Some(pattern);
        }
    }
    None
}

fn longest_passthrough_suffix(bytes: &[u8]) -> usize {
    let patterns = ALT_SCREEN_ENTER_SEQUENCES
        .iter()
        .chain(LIVE_MOUSE_ENABLE_SEQUENCES.iter())
        .chain(LIVE_MOUSE_DISABLE_SEQUENCES.iter())
        .chain(LIVE_INPUT_MODE_SEQUENCES.iter());
    let max_len = patterns.clone().map(|p| p.len()).max().unwrap_or(0);
    let max_suffix = bytes.len().min(max_len.saturating_sub(1));
    for len in (1..=max_suffix).rev() {
        let suffix = &bytes[bytes.len() - len..];
        if patterns.clone().any(|pattern| pattern.starts_with(suffix)) {
            return len;
        }
    }
    0
}

fn display_color_hex(color: DisplayColor) -> String {
    match color {
        DisplayColor::Default => "#d0d0d0".to_string(),
        DisplayColor::Indexed(idx) => rgb_hex(indexed_to_rgb(idx)),
        DisplayColor::Rgb(rgb) => rgb_hex(rgb),
    }
}

fn display_bg_hex(color: DisplayColor) -> String {
    match color {
        DisplayColor::Default => "#00000000".to_string(),
        DisplayColor::Indexed(idx) => rgb_hex(indexed_to_rgb(idx)),
        DisplayColor::Rgb(rgb) => rgb_hex(rgb),
    }
}

fn indexed_to_rgb(idx: u8) -> Rgb {
    match idx {
        0 => Rgb(0, 0, 0),
        1 => Rgb(205, 49, 49),
        2 => Rgb(13, 188, 121),
        3 => Rgb(229, 229, 16),
        4 => Rgb(36, 114, 200),
        5 => Rgb(188, 63, 188),
        6 => Rgb(17, 168, 205),
        7 => Rgb(229, 229, 229),
        8 => Rgb(102, 102, 102),
        9 => Rgb(241, 76, 76),
        10 => Rgb(35, 209, 139),
        11 => Rgb(245, 245, 67),
        12 => Rgb(59, 142, 234),
        13 => Rgb(214, 112, 214),
        14 => Rgb(41, 184, 219),
        _ => Rgb(255, 255, 255),
    }
}

fn rgb_hex(rgb: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
}

fn svg_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_line_from_row_tracks_wide_character_columns() {
        let row = vec![
            StyledCell {
                text: "界".to_string(),
                fg: DisplayColor::Default,
                bg: DisplayColor::Default,
                bold: false,
                dim: false,
                italic: false,
                underline: false,
                inverse: false,
                wide_continuation: false,
            },
            StyledCell {
                text: String::new(),
                fg: DisplayColor::Default,
                bg: DisplayColor::Default,
                bold: false,
                dim: false,
                italic: false,
                underline: false,
                inverse: false,
                wide_continuation: true,
            },
            StyledCell {
                text: "B".to_string(),
                fg: DisplayColor::Default,
                bg: DisplayColor::Default,
                bold: false,
                dim: false,
                italic: false,
                underline: false,
                inverse: false,
                wide_continuation: false,
            },
        ];

        let line = screen_line_from_row(&row);
        assert_eq!(line.text, "界B");
        assert_eq!(line.char_spans, vec![(0, 2), (2, 3)]);
        assert_eq!(line.cell_count, 3);
    }
}
