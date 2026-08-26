use ratatui::{
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
