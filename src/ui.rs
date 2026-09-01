//! iwtui's entire look in one file: the color palette, shared chrome
//! (dialogs with drop shadows, buttons, signal helpers) and every screen
//! renderer, including the IWTUI banner on the main menu.
//!
//! The nmtui rules this file enforces:
//! * Colors are *exact RGB values*, never named ANSI colors — terminal
//!   themes cannot make "cyan" render yellowish.
//! * The blue fills the whole screen, not just the dialog.
//! * Focus surrounds the text (black on cyan behind the glyphs), never a
//!   bare border; text inputs show focus with the real terminal cursor
//!   inside the field.
//! * Dialogs cast a drop shadow.
//!
//! Every screen renderer returns `Option<Position>` ("where the cursor
//! should be, if anywhere"); the topmost visible screen's answer wins and
//! is applied once at the end of `draw`, so overlays (help, errors, agent
//! popups) never leak a cursor.

use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{
    ActivateButton, AddHiddenButton, AddHiddenForm, AgentDialog, App, EditForm, EditFormButton,
    EditListButton, Focus, HostnameButton, RootAuth, Screen, MAIN_ITEMS,
};
use crate::system;

// ── palette ──────────────────────────────────────────────────────
// Vivid takes on the classic newt colors — a touch brighter than the
// vintage hues so they don't look muddy on modern backlit panels.

/// Screen AND dialog background: vivid newt blue.
const SCREEN_BLUE: Color = Color::Rgb(0x1a, 0x1b, 0x26);
const DIALOG_BG: Color = Color::Rgb(0x3b, 0x42, 0x61);

/// Bright periwinkle outline for dialogs — barely there, nmtui-style.
const BORDER_BLUE: Color = Color::Rgb(0x56, 0x5f, 0x89);

/// Vivid cyan for the focus highlight, hardcoded so it can never look yellow.
const FOCUS_CYAN: Color = Color::Rgb(0x7d, 0xcf, 0xff);

/// nmtui's red `< Button >` labels when not focused — brighter variant.
const BUTTON_RED: Color = Color::Rgb(0xf7, 0x76, 0x8e);

/// Dimmed hint/status text on the blue background.
const DIM: Color = Color::Rgb(0x56, 0x5f, 0x89);

/// Inline error text on the blue background.
const ERROR_RED: Color = Color::Rgb(0xf7, 0x76, 0x8e);

/// Deep navy drop shadow — reads as depth on the blue, softer than black.
const SHADOW: Color = Color::Rgb(0x0f, 0x0f, 0x14);

/// Whole-screen background (painted over the full terminal every frame).
fn s_norm() -> Style {
    Style::default().bg(SCREEN_BLUE)
}

/// Dialog body: white text on the newt blue.
fn s_dlg() -> Style {
    Style::default().fg(Color::White).bg(SCREEN_BLUE)
}

fn s_border() -> Style {
    Style::default().fg(BORDER_BLUE).bg(SCREEN_BLUE)
}

/// Dialog title: bold white on blue (sits on the border row).
fn s_title() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(SCREEN_BLUE)
        .add_modifier(Modifier::BOLD)
}

/// Focused element: bold BLACK ON CYAN behind the text — classic nmtui
/// selection. This is the only "highlight" in the UI and it always fills
/// behind the glyphs, never a bare border.
fn s_focus() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(FOCUS_CYAN)
        .add_modifier(Modifier::BOLD)
}

/// Selected but not focused (e.g. list row while a button has focus):
/// bold white on blue.
fn s_sel() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(SCREEN_BLUE)
        .add_modifier(Modifier::BOLD)
}

/// Unfocused button label: the nmtui red on blue.
fn s_btn() -> Style {
    Style::default().fg(BUTTON_RED).bg(SCREEN_BLUE)
}

/// Text input interior: black on white (the entry itself, never the border).
fn s_input() -> Style {
    Style::default().fg(Color::Black).bg(Color::White)
}

/// Dimmed hint text under inputs / in the status bar.
fn s_hint() -> Style {
    Style::default().fg(DIM).bg(SCREEN_BLUE)
}

/// Inline error text (e.g. hostname validation) on the blue background.
fn s_error() -> Style {
    Style::default()
        .fg(ERROR_RED)
        .bg(SCREEN_BLUE)
        .add_modifier(Modifier::BOLD)
}

/// The IWTUI banner glyphs on the main menu.
fn s_banner() -> Style {
    Style::default()
        .fg(FOCUS_CYAN)
        .bg(SCREEN_BLUE)
        .add_modifier(Modifier::BOLD)
}

/// Status bar. Active (a live message): bold black on cyan. Passive
/// (hostname / device / connection info): white on blue.
fn s_status(active: bool) -> Style {
    if active {
        Style::default()
            .fg(Color::Black)
            .bg(FOCUS_CYAN)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Rgb(0xc0, 0xca, 0xf5))
            .bg(SCREEN_BLUE)
    }
}

// ── shared chrome ────────────────────────────────────────────────

/// Centered dialog area capped to `w` x `h` (or the terminal size, whichever
/// is smaller), leaving one line at the bottom for the status bar.
fn dlg_sized(area: Rect, w: u16, h: u16) -> Rect {
    let avail = area.height.saturating_sub(1);
    let w = w.min(area.width);
    let h = h.min(avail);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (avail - h) / 2,
        width: w,
        height: h,
    }
}

/// Standard 80x24 dialog area used by most screens.
fn dlg_area(area: Rect) -> Rect {
    dlg_sized(area, 92, 28)
}

/// Centered popup area of the requested size, clamped to the terminal.
fn popup(w: u16, h: u16, area: Rect) -> Rect {
    Rect {
        x: area.x + (area.width - w.min(area.width)) / 2,
        y: area.y + (area.height - h.min(area.height)) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}

fn shadow(f: &mut Frame, area: Rect) {
    let sh = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width,
        height: area.height,
    };
    f.render_widget(Block::default().style(Style::default().bg(SHADOW)), sh);
}

/// Bordered dialog with title (and a drop shadow). Does NOT clear the
/// background (the whole screen is redrawn every frame anyway; the area
/// behind a primary dialog is the same newt blue).
fn dialog(f: &mut Frame, area: Rect, title: &str) -> Rect {
    shadow(f, area);
    let block = bordered_block(title);
    let inner = block.inner(area);
    f.render_widget(
        Block::default().style(Style::default().bg(DIALOG_BG)),
        inner,
    );
    f.render_widget(
        Block::default().style(Style::default().bg(DIALOG_BG)),
        inner,
    );
    f.render_widget(block, area);
    inner
}

/// The one dialog-block style used everywhere: periwinkle border on the
/// newt blue, bold white title. Focus is NEVER shown on the border —
/// highlights fill behind text instead (s_focus).
fn bordered_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_style(s_title())
        .style(s_border())
}

/// Bordered popup that clears whatever is behind it (used for overlays
/// rendered on top of another screen: agent dialogs, errors, help).
fn popup_box(f: &mut Frame, area: Rect, title: &str) -> Rect {
    f.render_widget(Clear, area);
    shadow(f, area);
    let block = bordered_block(title);
    let inner = block.inner(area);
    f.render_widget(
        Block::default().style(Style::default().bg(DIALOG_BG)),
        inner,
    );
    f.render_widget(
        Block::default().style(Style::default().bg(DIALOG_BG)),
        inner,
    );
    f.render_widget(block, area);
    inner
}

// ── signal & text helpers ────────────────────────────────────────
// Input to the signal helpers is plain dBm (already converted from iwd's
// centi-dBm in the iwd module).

/// 4-slot visual bar, e.g. `▆▆▆ `
fn sig_bar(dbm: i16) -> String {
    let bars = match dbm {
        r if r >= -50 => 4,
        r if r >= -65 => 3,
        r if r >= -75 => 2,
        r if r >= -85 => 1,
        _ => 0,
    };
    (0..4).map(|i| if i < bars { '▆' } else { ' ' }).collect()
}

/// Rough 0–100 percentage: -100 dBm -> 0%, -50 dBm -> 100%.
fn sig_pct(dbm: i16) -> u32 {
    (dbm + 100).clamp(0, 50) as u32 * 2
}

/// Mobile‑style signal bars using Unicode block characters.
fn mobile_sig_bar(dbm: i16) -> String {
    let bars = match dbm {
        r if r >= -50 => 4,
        r if r >= -65 => 3,
        r if r >= -75 => 2,
        r if r >= -85 => 1,
        _ => 0,
    };
    let chars = ['▂', '▄', '▆', '█'];
    (0..4)
        .map(|i| if i < bars { chars[i] } else { ' ' })
        .collect()
}

/// Truncate to `max` chars (char-boundary safe) with an ellipsis.
fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// Right-aligned row of `< Button >` labels, nmtui style. The focused
/// button gets black-on-cyan filling BEHIND its label; the rest are red.
fn btn_row<B: Copy + PartialEq>(
    f: &mut Frame,
    area: Rect,
    buttons: &[B],
    current: B,
    focus: Focus,
    label_fn: impl Fn(B) -> &'static str,
) {
    let labels: Vec<String> = buttons
        .iter()
        .map(|b| format!("< {} >", label_fn(*b)))
        .collect();
    if labels.is_empty() {
        return;
    }
    let total: u16 = labels.iter().map(|l| l.len() as u16).sum::<u16>() + labels.len() as u16 - 1;
    let mut x = if total >= area.width {
        area.x
    } else {
        area.x + area.width - total
    };
    for (i, label) in labels.iter().enumerate() {
        let is_cur = buttons[i] == current;
        let style = if is_cur && focus == Focus::Buttons {
            s_focus()
        } else if is_cur {
            s_sel()
        } else {
            s_btn()
        };
        f.render_widget(
            Paragraph::new(label.as_str()).style(style),
            Rect {
                x,
                y: area.y,
                width: label.len() as u16,
                height: 1,
            },
        );
        x += label.len() as u16 + 1;
    }
}

/// Render a bordered one-line input. The border NEVER changes with focus;
/// the caller shows focus via the returned cursor position.
fn input_field(f: &mut Frame, area: Rect, title: &str, text: &str) -> Position {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_style(s_title())
        .style(s_border());
    f.render_widget(
        Paragraph::new(text.to_string())
            .style(s_input())
            .block(block),
        area,
    );
    // Cursor sits right after the last character (inside the border).
    Position::new(
        area.x + 1 + text.chars().count() as u16,
        area.y + (area.height / 2),
    )
}

// ── top-level dispatch ───────────────────────────────────────────

pub fn draw(f: &mut Frame, app: &App) {
    // Fill the WHOLE screen with the nmtui blue (colors cover everything,
    // not just the dialog).
    f.render_widget(Block::default().style(s_norm()), f.area());

    let mut cursor: Option<Position> = None;
    match &app.screen {
        // The error screen renders over its saved previous screen.
        Screen::Error(msg) => {
            if let Some(prev) = &app.error_prev_screen {
                // Ignore the covered screen's cursor: the error popup is on
                // top and takes input.
                let _ = draw_screen(f, app, prev);
            }
            draw_error(f, msg);
        }
        other => cursor = draw_screen(f, app, other),
    }
    if app.show_help {
        cursor = None; // the help overlay swallows input
        draw_help(f);
    }
    draw_status(f, app);

    if let Some(pos) = cursor {
        f.set_cursor_position(pos);
    }
}

/// Render any screen (also used to draw the saved screen behind popups).
/// Returns the cursor position requested by the topmost text field.
fn draw_screen(f: &mut Frame, app: &App, screen: &Screen) -> Option<Position> {
    match screen {
        Screen::Main(idx) => draw_main(f, *idx),
        Screen::Activate {
            list_idx,
            button,
            focus,
        } => draw_activate(f, app, *list_idx, *button, *focus),
        Screen::EditList {
            list_idx,
            button,
            focus,
        } => draw_edit_list(f, app, *list_idx, *button, *focus),
        Screen::EditForm(form) => draw_edit_form(f, app, form),
        Screen::AddHidden(form) => draw_add_hidden(f, form),
        Screen::SetHostname {
            input,
            button,
            focus,
        } => draw_hostname(f, input, *button, *focus),
        Screen::RootAuth(auth) => draw_root_auth(f, auth),
        Screen::AgentDialog(d) => {
            // The covered screen's cursor never applies: the agent dialog
            // owns the input while it is open.
            let _ = draw_screen(f, app, d.prev_screen_ref());
            draw_agent(f, d, app.show_password)
        }
        Screen::Error(_) => None, // handled by draw()
    }
}

fn draw_status(f: &mut Frame, app: &App) {
    let area = Rect {
        x: 0,
        y: f.area().height.saturating_sub(1),
        width: f.area().width,
        height: 1,
    };

    let mut left = format!(" {}", app.hostname);
    if let Some(dev) = &app.device_name {
        left.push_str(&format!(" | {dev}"));
    }
    if !app.wifi_powered {
        left.push_str(" | Wi-Fi off");
    }

    let right = app.status_message.clone().unwrap_or_else(|| {
        match app.networks.iter().find(|n| n.connected) {
            Some(n) => format!(
                "{}: {} {}",
                app.station_state,
                trunc(&n.name, 24),
                sig_bar(n.signal_dbm)
            ),
            None => app.station_state.clone(),
        }
    });

    f.render_widget(Paragraph::new(left.as_str()).style(s_status(false)), area);

    let left_w = left.chars().count() as u16;
    let rw = right.chars().count() as u16;
    if rw < area.width.saturating_sub(left_w + 2) {
        let ra = Rect {
            x: area.x + area.width - rw,
            y: area.y,
            width: rw,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(right.as_str()).style(s_status(app.status_message.is_some())),
            ra,
        );
    }
}

// ── screens ──────────────────────────────────────────────────────

/// The IWTUI banner, shown on the main menu (exactly as specified —
/// ANSI-shadow style block glyphs).
const BANNER: [&str; 6] = [
    "██╗██╗    ██╗████████╗██╗   ██╗██╗",
    "██║██║    ██║╚══██╔══╝██║   ██║██║",
    "██║██║ █╗ ██║   ██║   ██║   ██║██║",
    "██║██║███╗██║   ██║   ██║   ██║██║",
    "██║╚███╔███╔╝   ██║   ╚██████╔╝██║",
    "╚═╝ ╚══╝╚══╝    ╚═╝    ╚═════╝ ╚═╝",
];

pub fn draw_main(f: &mut Frame, idx: usize) -> Option<Position> {
    let full = f.area();

    // Big enough for the banner (6 rows) + a blank line + a compact menu?
    // On tiny terminals we skip the banner and just show the menu.
    let banner_w = BANNER[0].chars().count() as u16;
    let mut menu_area = full;
    if full.height >= 18 && full.width >= banner_w + 2 {
        let bx = full.x + (full.width - banner_w) / 2;
        let by = full.y + 1;
        for (i, line) in BANNER.iter().enumerate() {
            f.render_widget(
                Paragraph::new(*line).style(s_banner()),
                Rect {
                    x: bx,
                    y: by + i as u16,
                    width: banner_w,
                    height: 1,
                },
            );
        }
        menu_area = Rect {
            x: full.x,
            y: by + BANNER.len() as u16 + 1,
            width: full.width,
            height: full.height.saturating_sub(BANNER.len() as u16 + 2),
        };
    }

    let inner = dialog(
        f,
        dlg_sized(menu_area, 60, 14),
        "iwtui",
    );
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(inner);
    let li: Vec<ListItem> = MAIN_ITEMS
        .iter()
        .map(|t| ListItem::new(format!(" {t}")))
        .collect();
    let mut st = ListState::default();
    st.select(Some(idx));
    let list = List::new(li).style(s_dlg()).highlight_style(s_focus());
    f.render_stateful_widget(list, chunks[1], &mut st);
    None
}

// ── activate a connection ────────────────────────────────────────

// ── saved networks ───────────────────────────────────────────────

fn draw_edit_list(
    f: &mut Frame,
    app: &App,
    idx: usize,
    btn: EditListButton,
    focus: Focus,
) -> Option<Position> {
    let area = dlg_area(f.area());
    let inner = dialog(f, area, "Edit a connection");
    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(3),    // list
        Constraint::Length(1), // spacer
        Constraint::Length(1), // buttons
        Constraint::Length(1), // spacer
    ])
    .split(inner);

    let w = inner.width as usize;
    let name_w = w.saturating_sub(17).clamp(8, 44);
    let header = format!(" {:<nw$} {:<6} {:<6}", "Name", "Sec", "Auto", nw = name_w);
    f.render_widget(Paragraph::new(header).style(s_title()), chunks[0]);

    if app.known_networks.is_empty() {
        f.render_widget(
            Paragraph::new(" No saved networks.").style(s_dlg()),
            chunks[1],
        );
    } else {
        let li: Vec<ListItem> = app
            .known_networks
            .iter()
            .map(|n| {
                // '*' marks hidden networks.
                let name_disp = if n.hidden {
                    format!("{}*", trunc(&n.name, name_w.saturating_sub(2)))
                } else {
                    trunc(&n.name, name_w.saturating_sub(1))
                };
                ListItem::new(format!(
                    " {:<nw$} {:<6} {:<6}",
                    name_disp,
                    n.security_type,
                    if n.auto_connect { "yes" } else { "no" },
                    nw = name_w
                ))
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(idx));
        let hl = if focus == Focus::List {
            s_focus()
        } else {
            s_sel()
        };
        let list = List::new(li).style(s_dlg()).highlight_style(hl);
        f.render_stateful_widget(list, chunks[1], &mut state);
    }
    btn_row(f, chunks[3], &EditListButton::ALL, btn, focus, |b| {
        b.label()
    });
    None
}

// ── edit form ────────────────────────────────────────────────────

fn draw_edit_form(f: &mut Frame, app: &App, form: &EditForm) -> Option<Position> {
    let area = dlg_area(f.area());
    let known = app
        .known_index_by_path(&form.net_path)
        .and_then(|i| app.known_networks.get(i));
    let name = known.map(|n| n.name.as_str()).unwrap_or("Unknown");
    let sec = known.map(|n| n.security_type.as_str()).unwrap_or("");
    let last = known
        .and_then(|n| n.last_connected.as_deref())
        .unwrap_or("");

    let inner = dialog(f, area, &format!("Edit {name}"));
    let chunks = Layout::vertical([
        Constraint::Length(1), // blank
        Constraint::Length(1), // name
        Constraint::Length(1), // security
        Constraint::Length(1), // last connected
        Constraint::Length(1), // blank
        Constraint::Length(1), // checkbox
        Constraint::Min(1),    // spacer
        Constraint::Length(1), // buttons
        Constraint::Length(1), // blank
    ])
    .split(inner);

    f.render_widget(
        Paragraph::new(format!(" Name:     {name}")).style(s_dlg()),
        chunks[1],
    );
    f.render_widget(
        Paragraph::new(format!(" Security: {sec}")).style(s_dlg()),
        chunks[2],
    );
    let last_line = if last.is_empty() {
        String::new()
    } else {
        format!(
            " Last used: {}",
            trunc(last, (inner.width as usize).saturating_sub(14))
        )
    };
    f.render_widget(Paragraph::new(last_line).style(s_dlg()), chunks[3]);
    let chk = if form.auto_connect { "[x]" } else { "[ ]" };
    // Focus fills BEHIND the checkbox label — never a border.
    let chk_s = if form.focus == Focus::List {
        s_focus()
    } else {
        s_dlg()
    };
    f.render_widget(
        Paragraph::new(format!(" {chk} Auto-connect")).style(chk_s),
        chunks[5],
    );
    btn_row(
        f,
        chunks[7],
        &EditFormButton::ALL,
        form.button,
        form.focus,
        |b| b.label(),
    );
    None
}

// ── add hidden network ───────────────────────────────────────────

fn draw_add_hidden(f: &mut Frame, form: &AddHiddenForm) -> Option<Position> {
    let area = dlg_area(f.area());
    let inner = dialog(f, area, "Add hidden network");
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    // Focus = cursor inside the field; border stays neutral.
    let cursor = input_field(f, chunks[1], "SSID", &form.ssid);
    btn_row(
        f,
        chunks[3],
        &AddHiddenButton::ALL,
        form.button,
        form.focus,
        |b| b.label(),
    );
    (form.focus == Focus::List).then_some(cursor)
}

// ── hostname ─────────────────────────────────────────────────────

fn draw_hostname(
    f: &mut Frame,
    input: &str,
    btn: HostnameButton,
    focus: Focus,
) -> Option<Position> {
    let area = dlg_area(f.area());
    let inner = dialog(f, area, "Set system hostname");
    let chunks = Layout::vertical([
        Constraint::Length(1), // blank
        Constraint::Length(3), // hostname field
        Constraint::Length(1), // live validation / hint
        Constraint::Length(1), // spacer
        Constraint::Length(1), // buttons
        Constraint::Length(1), // blank
    ])
    .split(inner);

    let cursor = input_field(f, chunks[1], "Hostname", input);

    // Live validation while typing: red inline error, calm hint otherwise.
    let (line, style) = if input.trim().is_empty() {
        (
            " Allowed: letters, digits, '-' and '.' (max 63 characters)".to_string(),
            s_hint(),
        )
    } else {
        match system::validate_hostname(input) {
            Ok(()) => (" Valid hostname".to_string(), s_hint()),
            Err(msg) => (format!(" {msg}"), s_error()),
        }
    };
    f.render_widget(Paragraph::new(line).style(style), chunks[2]);

    btn_row(f, chunks[4], &HostnameButton::ALL, btn, focus, |b| {
        b.label()
    });
    (focus == Focus::List).then_some(cursor)
}

// ── root password (hostname escalation) ─────────────────────────

fn draw_root_auth(f: &mut Frame, auth: &RootAuth) -> Option<Position> {
    let area = popup(64, 11, f.area());
    let inner = popup_box(f, area, "Root password required");
    let chunks = Layout::vertical([
        Constraint::Length(1), // explanation
        Constraint::Length(1), // spacer
        Constraint::Length(3), // password field
        Constraint::Length(1), // spacer
        Constraint::Length(1), // message line
        Constraint::Length(1), // spacer
        Constraint::Length(1), // buttons
    ])
    .split(inner);

    let what = if auth.busy {
        format!(
            "Setting hostname '{}' requires authentication...",
            auth.pending_hostname
        )
    } else {
        format!(
            "Setting hostname '{}' requires root privileges.",
            auth.pending_hostname
        )
    };
    f.render_widget(Paragraph::new(what).style(s_dlg()), chunks[0]);

    // While busy the field is emptied visually (nothing to type into).
    let masked = if auth.busy {
        String::new()
    } else {
        "*".repeat(auth.password.chars().count())
    };
    let cursor = input_field(f, chunks[2], " Password (Ctrl+R shows) ", &masked);

    let (msg_line, msg_style) = if auth.busy {
        (" Authenticating...".to_string(), s_hint())
    } else {
        match &auth.message {
            Some(m) => (format!(" {m}"), s_error()),
            None => (String::new(), s_hint()),
        }
    };
    f.render_widget(Paragraph::new(msg_line).style(msg_style), chunks[4]);

    btn_row(
        f,
        chunks[6],
        &crate::app::AuthButton::ALL,
        auth.button,
        auth.focus,
        |b| b.label(),
    );

    let want_cursor = auth.focus == Focus::List && !auth.busy;
    want_cursor.then_some(cursor)
}

// ── agent credential dialog ──────────────────────────────────────

fn draw_agent(f: &mut Frame, d: &AgentDialog, show_password: bool) -> Option<Position> {
    let prompt = d.prompt();
    let (pass, user, editing_user, has_user) = match d {
        AgentDialog::Passphrase { pass, .. } => (pass.clone(), String::new(), false, false),
        AgentDialog::UserPassword {
            user,
            pass,
            editing_user,
            ..
        } => (pass.clone(), user.clone(), *editing_user, true),
        AgentDialog::UserNameAndPassword {
            user,
            pass,
            editing_user,
            ..
        } => (pass.clone(), user.clone(), *editing_user, true),
        AgentDialog::PrivateKeyPassphrase { pass, .. } => {
            (pass.clone(), String::new(), false, false)
        }
    };
    let masked = if show_password {
        pass.clone()
    } else {
        "*".repeat(pass.chars().count())
    };

    let h = if has_user { 11 } else { 9 };
    let area = popup(64, h, f.area());
    let inner = popup_box(f, area, "Authentication required");

    if has_user {
        let ch = Layout::vertical([
            Constraint::Length(1), // prompt
            Constraint::Length(1), // spacer
            Constraint::Length(3), // user field
            Constraint::Length(1), // spacer
            Constraint::Length(3), // password field
        ])
        .split(inner);
        f.render_widget(Paragraph::new(prompt).style(s_dlg()), ch[0]);
        let user_cursor = input_field(f, ch[2], "User", &user);
        let pass_cursor = input_field(f, ch[4], " Password (Ctrl+R shows) ", &masked);
        let cursor = if editing_user {
            user_cursor
        } else {
            pass_cursor
        };
        Some(cursor)
    } else {
        let ch = Layout::vertical([
            Constraint::Length(1), // prompt
            Constraint::Length(1), // spacer
            Constraint::Length(3), // password field
        ])
        .split(inner);
        f.render_widget(Paragraph::new(prompt).style(s_dlg()), ch[0]);
        let cursor = input_field(f, ch[2], " Password (Ctrl+R shows) ", &masked);
        Some(cursor)
    }
}

// ── error popup ──────────────────────────────────────────────────

fn draw_error(f: &mut Frame, msg: &str) {
    let full = f.area();
    let lines = msg.lines().count().max(1) as u16;
    let h = (lines + 4).min(full.height);
    let max_line = msg.lines().map(|l| l.chars().count()).max().unwrap_or(20) as u16;
    let w = (max_line + 8).clamp(40, 70).min(full.width);
    let area = popup(w, h, full);
    let inner = popup_box(f, area, "Error");
    f.render_widget(
        Paragraph::new(msg)
            .style(s_dlg())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        inner,
    );
}

// ── help overlay ─────────────────────────────────────────────────

fn draw_activate(
    f: &mut Frame,
    app: &App,
    idx: usize,
    btn: ActivateButton,
    focus: Focus,
) -> Option<Position> {
    // Larger nmtui‑style dialog, same dark theme as other screens.
    let area = dlg_sized(f.area(), 88, 28);
    let inner = dialog(f, area, "Activate a connection");
    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(6),    // list
        Constraint::Length(1), // spacer
        Constraint::Length(1), // buttons
        Constraint::Length(1), // spacer
    ])
    .split(inner);

    // Header: Name | Sig (mobile bars only, no percentage)
    let name_w = (inner.width as usize).saturating_sub(14).clamp(8, 52);
    let header = format!(
        " {:<name_w$} {:<6} {:>4}",
        "Name",
        "Sig",
        "%",
        name_w = name_w
    );
    f.render_widget(Paragraph::new(header).style(s_title()), chunks[0]);

    if app.networks.is_empty() {
        let msg = if !app.wifi_powered {
            " Wi-Fi is off."
        } else if app.station_state == "scanning" {
            " Scanning..."
        } else {
            " No networks found."
        };
        f.render_widget(Paragraph::new(msg).style(s_dlg()), chunks[1]);
    } else {
        let known: std::collections::HashSet<&str> =
            app.known_networks.iter().map(|k| k.name.as_str()).collect();

        let li: Vec<ListItem> = app
            .networks
            .iter()
            .map(|n| {
                let prefix = if n.connected {
                    "> "
                } else if known.contains(n.name.as_str()) {
                    "* "
                } else {
                    "  "
                };
                let raw_name = format!("{}{}", prefix, n.name);
                let name_padded = format!("{:<name_w$}", trunc(&raw_name, name_w), name_w = name_w);
                let bar = sig_bar(n.signal_dbm);
                let pct = format!("{:3}%", sig_pct(n.signal_dbm));
                ListItem::new(format!(" {name_padded} {bar:<6} {pct:>4}"))
            })
            .collect();

        let mut state = ListState::default();
        state.select(Some(idx));
        let hl = if focus == Focus::List {
            s_focus()
        } else {
            s_sel()
        };
        let list = List::new(li).style(s_dlg()).highlight_style(hl);
        f.render_stateful_widget(list, chunks[1], &mut state);
    }

    let connected = app.networks.get(idx).map(|n| n.connected).unwrap_or(false);
    btn_row(f, chunks[3], &ActivateButton::ALL, btn, focus, move |b| {
        b.label_for(connected)
    });
    None
}

// ── help overlay ─────────────────────────────────────────────────

fn draw_help(f: &mut Frame) {
    let text = "\
Global
  Up/Down or j/k ...... move in list
  Left/Right or h/l ... move between buttons
  Tab ................. list <-> buttons
  Enter ............... activate
  Esc or q ............ back / quit
  ? ................... toggle this help

Activate a connection
  r ................... rescan
  n ................... connect to hidden network
  p ................... toggle Wi-Fi power
  Enter on network .... connect / disconnect

Saved networks
  Enter ............... edit auto-connect
  Delete .............. forget
  Add ................. connect to hidden network

Set system hostname
  Enter ............... apply (asks for the root password
                        via sudo when not running as root)

Password dialogs
  Ctrl+R .............. show/hide password
  Tab ................. switch user/password
  Esc ................. cancel";
    let lines = text.lines().count() as u16;
    let h = (lines + 2).min(f.area().height);
    let area = popup(60, h, f.area());
    let inner = popup_box(f, area, "iwtui help — Esc closes");
    f.render_widget(
        Paragraph::new(text)
            .style(s_dlg())
            .wrap(Wrap { trim: false }),
        inner,
    );
}use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap,
    },
};

use crate::app::{
    ActivateButton, AddHiddenButton, AddHiddenForm, AgentDialog, App, EditForm, EditFormButton, EditListButton, Focus,
    HostnameButton, Screen,
};

const BG: Color = Color::Blue;
const FG: Color = Color::White;
const HL_BG: Color = Color::Cyan;
const HL_FG: Color = Color::Black;
const HEADER_FG: Color = Color::White;

fn bg_style() -> Style {
    Style::default().bg(BG).fg(FG)
}

fn hl_style() -> Style {
    Style::default().bg(HL_BG).fg(HL_FG).add_modifier(Modifier::BOLD)
}

fn header_style() -> Style {
    Style::default().bg(BG).fg(HEADER_FG).add_modifier(Modifier::BOLD)
}

fn input_style() -> Style {
    Style::default().bg(Color::Black).fg(Color::White)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let size = f.size();
    f.render_widget(Block::default().style(bg_style()), size);

    let dialog = match &app.screen {
        Screen::Main(idx) => {
            draw_main_menu(f, *idx);
            None
        }
        Screen::Activate { list_idx, button, focus } => {
            draw_activate(f, app, *list_idx, *button, *focus);
            None
        }
        Screen::EditList { list_idx, button, focus } => {
            draw_edit_list(f, app, *list_idx, *button, *focus);
            None
        }
        Screen::EditForm(form) => {
            draw_edit_form(f, app, form);
            None
        }
        Screen::AddHidden(form) => {
            draw_add_hidden(f, form);
            None
        }
        Screen::SetHostname { input, button, focus } => {
            draw_hostname(f, input, *button, *focus);
            None
        }
        Screen::AgentDialog(dialog) => Some(draw_agent_dialog(f, dialog)),
        Screen::Error(msg) => Some(draw_error(f, msg)),
    };

    draw_status_bar(f, size, app);

    if let Some(area) = dialog {
        f.render_widget(Clear, area);
    }
}

fn centered(r: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(r.width);
    let h = height.min(r.height);
    let x = r.x + (r.width.saturating_sub(w)) / 2;
    let y = r.y + (r.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn dialog_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .title_top(Line::from(format!(" {} ", title)))
        .style(bg_style())
}

fn draw_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let bar_area = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
    let text = match &app.screen {
        Screen::Main(_) => " Esc: Quit    Arrow keys / Enter: navigate ".to_string(),
        Screen::Activate { focus, .. } => match focus {
            Focus::List => " Tab: buttons    Enter: connect    Esc: back ".to_string(),
            Focus::Buttons => " Tab: list    ←/→: button    Enter: activate ".to_string(),
        },
        Screen::EditList { focus, .. } => match focus {
            Focus::List => " Tab: buttons    Enter: edit    Esc: back ".to_string(),
            Focus::Buttons => " Tab: list    ←/→: button    Enter: activate ".to_string(),
        },
        Screen::EditForm(form) => match form.focus {
            Focus::List => " Space: toggle auto-connect    Tab: buttons ".to_string(),
            Focus::Buttons => " Tab: form    ←/→: button    Enter: activate ".to_string(),
        },
        Screen::AddHidden(form) => match form.focus {
            Focus::List => " Type SSID    Tab: buttons ".to_string(),
            Focus::Buttons => " Tab: input    ←/→: button    Enter: activate ".to_string(),
        },
        Screen::SetHostname { focus, .. } => match focus {
            Focus::List => " Type hostname    Tab: buttons ".to_string(),
            Focus::Buttons => " Tab: input    ←/→: button    Enter: activate ".to_string(),
        },
        Screen::AgentDialog(d) => match d {
            AgentDialog::Passphrase { .. } | AgentDialog::PrivateKeyPassphrase { .. } => {
                " Enter: submit    Esc: cancel ".to_string()
            }
            AgentDialog::UserPassword { .. } | AgentDialog::UserNameAndPassword { .. } => {
                " Tab/↑↓: switch field    Enter: submit    Esc: cancel ".to_string()
            }
        },
        Screen::Error(_) => " Press Enter or Esc to dismiss ".to_string(),
    };
    let style = if app.status_message.is_some() {
        Style::default().bg(Color::DarkGray).fg(Color::Yellow)
    } else {
        Style::default().bg(Color::Black).fg(Color::Gray)
    };
    f.render_widget(Paragraph::new(text).style(style), bar_area);
}

fn draw_main_menu(f: &mut Frame, idx: usize) {
    let area = centered(f.size(), 70, 18);
    let block = dialog_block("IWD TUI");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Min(0),
                Constraint::Length(2),
            ]
            .as_ref(),
        )
        .split(inner);

    let title = Paragraph::new("IWD TUI")
        .style(header_style())
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[1]);

    let items = ["Edit a connection", "Activate a connection", "Set system hostname", "Quit"];
    let rows: Vec<Row> = items
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let cell = Cell::from(format!("  {}  ", label));
            let row = Row::new(vec![cell]);
            if i == idx {
                row.style(hl_style())
            } else {
                row.style(bg_style())
            }
        })
        .collect();

    let table = Table::new(rows, [Constraint::Percentage(100)])
        .style(bg_style())
        .highlight_style(hl_style());
    let mut state = TableState::default();
    state.select(Some(idx));
    f.render_stateful_widget(table, chunks[2], &mut state);

    let hint = Paragraph::new("Use arrows to navigate, Enter to select, Esc to quit.")
        .style(Style::default().bg(BG).fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(hint, chunks[3]);
}

fn draw_activate(
    f: &mut Frame,
    app: &App,
    list_idx: usize,
    button: ActivateButton,
    focus: Focus,
) {
    let area = centered(f.size(), 80, 22);
    let title = format!("Activate a connection ({})", app.device_name);
    let block = dialog_block(&title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Min(0),
                Constraint::Length(2),
            ]
            .as_ref(),
        )
        .split(inner);

    let header = Row::new(vec![
        Cell::from("  SSID").style(header_style()),
        Cell::from("TYPE").style(header_style()),
        Cell::from("SIGNAL").style(header_style()),
        Cell::from("STATUS").style(header_style()),
    ])
    .height(1)
    .style(header_style());

    let rows: Vec<Row> = app
        .networks
        .iter()
        .map(|n| {
            let mark = if n.connected { "★ " } else { "  " };
            let ssid = truncate(&n.name, 30);
            let status = if n.connected { "Active" } else { "" };
            Row::new(vec![
                Cell::from(format!("{}{}", mark, ssid)),
                Cell::from(n.security_type.clone()),
                Cell::from(format!("{}", n.signal_strength)),
                Cell::from(status.to_string()),
            ])
            .style(bg_style())
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(50),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
            Constraint::Percentage(20),
        ],
    )
    .header(header)
    .style(bg_style())
    .highlight_style(hl_style());

    let mut state = TableState::default();
    state.select(if app.networks.is_empty() { None } else { Some(list_idx) });
    f.render_stateful_widget(table, chunks[0], &mut state);

    if app.networks.is_empty() {
        let hint = Paragraph::new("No networks visible. Press 'Rescan' or wait.")
            .style(Style::default().bg(BG).fg(Color::Gray))
            .alignment(Alignment::Center);
        let center = centered(chunks[0], 60, 3);
        f.render_widget(hint, center);
    }

    let buttons: Vec<(ActivateButton, &str)> = ActivateButton::ALL
        .iter()
        .map(|b| (*b, b.label()))
        .collect();
    draw_button_row(f, chunks[1], &buttons, button, focus == Focus::Buttons, |b| {
        format!(" {} ", b)
    });
}

fn draw_edit_list(
    f: &mut Frame,
    app: &App,
    list_idx: usize,
    button: EditListButton,
    focus: Focus,
) {
    let area = centered(f.size(), 80, 22);
    let block = dialog_block("Edit a connection");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)].as_ref())
        .split(inner);

    let header = Row::new(vec![
        Cell::from("  NAME").style(header_style()),
        Cell::from("TYPE").style(header_style()),
    ])
    .height(1);

    let rows: Vec<Row> = app
        .known_networks
        .iter()
        .map(|n| {
            Row::new(vec![
                Cell::from(format!("  {}", truncate(&n.name, 40))),
                Cell::from(n.security_type.clone()),
            ])
            .style(bg_style())
        })
        .collect();

    let table = Table::new(rows, [Constraint::Percentage(60), Constraint::Percentage(40)])
        .header(header)
        .style(bg_style())
        .highlight_style(hl_style());

    let mut state = TableState::default();
    state.select(if app.known_networks.is_empty() { None } else { Some(list_idx) });
    f.render_stateful_widget(table, chunks[0], &mut state);

    if app.known_networks.is_empty() {
        let hint = Paragraph::new("No saved networks. Connect to one first.")
            .style(Style::default().bg(BG).fg(Color::Gray))
            .alignment(Alignment::Center);
        let center = centered(chunks[0], 60, 3);
        f.render_widget(hint, center);
    }

    let buttons: Vec<(EditListButton, &str)> = EditListButton::ALL
        .iter()
        .map(|b| (*b, b.label()))
        .collect();
    draw_button_row(f, chunks[1], &buttons, button, focus == Focus::Buttons, |b| {
        format!(" {} ", b)
    });
}

fn draw_edit_form(f: &mut Frame, app: &App, form: &EditForm) {
    let area = centered(f.size(), 72, 18);
    let title = match app.known_networks.get(form.net_idx) {
        Some(n) => format!("Editing '{}'", n.name),
        None => "Editing".to_string(),
    };
    let block = dialog_block(&title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)].as_ref())
        .split(inner);

    let net = app.known_networks.get(form.net_idx);

    let last_conn = net
        .and_then(|n| n.last_connected)
        .map(epoch_to_utc_string)
        .unwrap_or_else(|| "never".to_string());

    let auto_mark = if form.auto_connect { "[✓]" } else { "[ ]" };
    let hidden_mark = match net {
        Some(n) if n.hidden => "[✓]",
        _ => "[ ]",
    };

    let rows = vec![
        Row::new(vec![Cell::from("  SSID"), Cell::from(net.map(|n| n.name.clone()).unwrap_or_default())]),
        Row::new(vec![
            Cell::from("  Type"),
            Cell::from(net.map(|n| n.security_type.clone()).unwrap_or_default()),
        ]),
        Row::new(vec![
            Cell::from("  Auto-connect"),
            Cell::from(auto_mark),
        ]),
        Row::new(vec![Cell::from("  Hidden"), Cell::from(hidden_mark)]),
        Row::new(vec![Cell::from("  Last connected"), Cell::from(last_conn)]),
    ];

    let table = Table::new(
        rows,
        [Constraint::Length(20), Constraint::Min(0)],
    )
    .style(bg_style())
    .highlight_style(hl_style());

    let mut state = TableState::default();
    if form.focus == Focus::List {
        state.select(Some(2));
    } else {
        state.select(None);
    }
    f.render_stateful_widget(table, chunks[0], &mut state);

    let buttons: Vec<(EditFormButton, &str)> = EditFormButton::ALL
        .iter()
        .map(|b| (*b, b.label()))
        .collect();
    draw_button_row(f, chunks[1], &buttons, form.button, form.focus == Focus::Buttons, |b| {
        format!(" {} ", b)
    });
}

fn draw_add_hidden(f: &mut Frame, form: &AddHiddenForm) {
    let area = centered(f.size(), 70, 16);
    let block = dialog_block("Add Hidden Network");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(2),
            ]
            .as_ref(),
        )
        .split(inner);

    let label = Paragraph::new("SSID")
        .style(header_style());
    f.render_widget(label, chunks[0]);

    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .style(if form.focus == Focus::List {
            Style::default().bg(BG).fg(FG).add_modifier(Modifier::BOLD)
        } else {
            bg_style()
        });
    let input_p = Paragraph::new(form.ssid.as_str())
        .style(input_style())
        .block(input_block);
    f.render_widget(input_p, chunks[1]);

    let hint = Paragraph::new("Enter the SSID of the hidden network. You will be prompted for a password if required.")
        .style(Style::default().bg(BG).fg(Color::Gray));
    f.render_widget(hint, chunks[2]);

    let buttons: Vec<(AddHiddenButton, &str)> = AddHiddenButton::ALL
        .iter()
        .map(|b| (*b, b.label()))
        .collect();
    draw_button_row(f, chunks[4], &buttons, form.button, form.focus == Focus::Buttons, |b| {
        format!(" {} ", b)
    });
}

fn draw_hostname(
    f: &mut Frame,
    input: &str,
    button: HostnameButton,
    focus: Focus,
) {
    let area = centered(f.size(), 70, 16);
    let block = dialog_block("Set system hostname");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(2),
            ]
            .as_ref(),
        )
        .split(inner);

    let label = Paragraph::new("Hostname")
        .style(header_style());
    f.render_widget(label, chunks[0]);

    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .style(if focus == Focus::List {
            Style::default().bg(BG).fg(FG).add_modifier(Modifier::BOLD)
        } else {
            bg_style()
        });
    let input_p = Paragraph::new(input)
        .style(input_style())
        .block(input_block);
    f.render_widget(input_p, chunks[1]);

    let hint = Paragraph::new("Only a-z A-Z 0-9 - . allowed, max 63 bytes.")
        .style(Style::default().bg(BG).fg(Color::Gray));
    f.render_widget(hint, chunks[2]);

    let buttons: Vec<(HostnameButton, &str)> = HostnameButton::ALL
        .iter()
        .map(|b| (*b, b.label()))
        .collect();
    draw_button_row(f, chunks[4], &buttons, button, focus == Focus::Buttons, |b| {
        format!(" {} ", b)
    });
}

fn draw_agent_dialog(f: &mut Frame, dialog: &AgentDialog) -> Rect {
    let area = centered(f.size(), 64, 16);
    let block = dialog_block("Authentication Required");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Min(0),
            ]
            .as_ref(),
        )
        .split(inner);

    let prompt = Paragraph::new(dialog.prompt())
        .style(bg_style())
        .wrap(Wrap { trim: true });
    f.render_widget(prompt, chunks[0]);

    match dialog {
        AgentDialog::Passphrase { pass, .. } | AgentDialog::PrivateKeyPassphrase { pass, .. } => {
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .title_top(Line::from(" Password "))
                .style(Style::default().bg(BG).fg(FG).add_modifier(Modifier::BOLD));
            let p = Paragraph::new(masked(pass))
                .style(input_style())
                .block(input_block);
            f.render_widget(p, chunks[2]);

            let help = match dialog {
                AgentDialog::Passphrase { .. } => "Enter the WPA/WPA2 passphrase.",
                AgentDialog::PrivateKeyPassphrase { .. } => "Enter the private key passphrase (EAP-TLS).",
                _ => "",
            };
            f.render_widget(
                Paragraph::new(help).style(Style::default().bg(BG).fg(Color::Gray)),
                chunks[5],
            );
        }
        AgentDialog::UserPassword { user, pass, editing_user, .. }
        | AgentDialog::UserNameAndPassword { user, pass, editing_user, .. } => {
            let user_block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .title_top(Line::from(" Username "))
                .style(if *editing_user {
                    Style::default().bg(BG).fg(FG).add_modifier(Modifier::BOLD)
                } else {
                    bg_style()
                });
            f.render_widget(
                Paragraph::new(user.clone())
                    .style(input_style())
                    .block(user_block),
                chunks[2],
            );

            let pass_block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .title_top(Line::from(" Password "))
                .style(if !*editing_user {
                    Style::default().bg(BG).fg(FG).add_modifier(Modifier::BOLD)
                } else {
                    bg_style()
                });
            f.render_widget(
                Paragraph::new(masked(pass))
                    .style(input_style())
                    .block(pass_block),
                chunks[4],
            );

            let help = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Tab or Up/Down to switch fields. Enter to submit, Esc to cancel.",
                    Style::default().fg(Color::Gray),
                )),
            ];
            f.render_widget(
                Paragraph::new(help).style(bg_style()),
                chunks[5],
            );
        }
    }

    area
}

fn draw_error(f: &mut Frame, msg: &str) -> Rect {
    let area = centered(f.size(), 70, 12);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .title_top(Line::from(" Error "))
        .style(Style::default().bg(Color::Red).fg(Color::White));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let p = Paragraph::new(msg)
        .style(Style::default().bg(Color::Red).fg(Color::White))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    let center = centered(inner, 90, 60);
    f.render_widget(p, center);
    area
}

fn draw_button_row<T, F>(
    f: &mut Frame,
    area: Rect,
    buttons: &[(T, &str)],
    selected: T,
    focused: bool,
    label_fmt: F,
) where
    T: PartialEq + Copy,
    F: Fn(&str) -> String,
{
    let n = buttons.len() as u16;
    let widths = vec![Constraint::Percentage(100 / n); buttons.len()];
    let cells = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(widths)
        .split(area);

    for (i, (b, label)) in buttons.iter().enumerate() {
        let is_sel = *b == selected;
        let style = if is_sel && focused {
            hl_style()
        } else if is_sel {
            Style::default().bg(BG).fg(FG).add_modifier(Modifier::BOLD)
        } else {
            bg_style()
        };
        let text = format!("< {} >", label_fmt(label));
        let p = Paragraph::new(text).style(style).alignment(Alignment::Center);
        f.render_widget(p, cells[i]);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(3)).collect();
        t.push_str("...");
        t
    }
}

fn masked(s: &str) -> String {
    "•".repeat(s.chars().count())
}

fn epoch_to_utc_string(epoch: u64) -> String {
    let secs = epoch as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let h = (rem / 3600) as u32;
    let mi = ((rem % 3600) / 60) as u32;
    let s = (rem % 60) as u32;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}
