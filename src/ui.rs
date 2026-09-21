//! Rendering - deliberately styled after nmtui (newt):
//!
//! * centered dialogs whose title sits on the border as `┤ Title ├`
//! * `<Button>` labels, highlighted when focused
//! * boxed `┌ OK ┐` buttons in message dialogs
//! * newt-style `↑ ▒ ▮ ▒ ↓` scrollbars on lists
//! * a vertical button column beside lists (as in "Activate/Edit a
//!   connection"), with Left/Right travel between list and buttons
//! * every stacked window dims the ones below it

use ratatui::{
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Title, Wrap},
    Frame,
};

use crate::app::{App, Focus, Overlay, Screen, DETAIL_BUTTONS, MENU_ENTRIES, WIFI_BUTTONS};
use crate::iwd::NetworkEntry;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    match &app.screen {
        Screen::Menu { selected } => draw_menu(f, area, *selected),
        Screen::Wifi { .. } => draw_wifi(f, area, app),
    }
    draw_overlays(f, area, app);
}

// ---------------------------------------------------------------- helpers

fn selected_style() -> Style {
    Style::default().bg(Color::Blue).fg(Color::White)
}

fn dim_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Draws a bordered dialog with the nmtui-style centered `┤ Title ├`
/// and returns the inner area.
fn dialog(f: &mut Frame, area: Rect, title: &str) -> Rect {
    f.render_widget(
        Block::bordered().title(
            Title::from(Line::from(format!("┤ {title} ├"))).alignment(Alignment::Center),
        ),
        area,
    );
    area.inner(Margin::new(1, 1))
}

fn draw_shadow(f: &mut Frame, rect: Rect) {
    let shadow = Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(1),
        height: rect.height.saturating_sub(1),
    };
    f.render_widget(Clear, shadow);
    f.buffer_mut().set_style(shadow, Style::default().bg(Color::Black));
}

/// nmtui-style `<Button>` row, centered.
fn draw_button_row(f: &mut Frame, area: Rect, buttons: &[(&str, bool)]) {
    let mut spans: Vec<Span> = Vec::new();
    for (index, (label, focused)) in buttons.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        let style = if *focused { selected_style() } else { Style::default() };
        spans.push(Span::styled(format!("<{label}>"), style));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
        area,
    );
}

/// nmtui-style `<Cancel> <OK>` row, right-aligned (form dialogs).
fn draw_button_row_right(f: &mut Frame, area: Rect, buttons: &[(&str, bool)]) {
    let mut spans: Vec<Span> = Vec::new();
    for (index, (label, focused)) in buttons.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        let style = if *focused { selected_style() } else { Style::default() };
        spans.push(Span::styled(format!("<{label}>"), style));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Right),
        area,
    );
}

/// nmtui message-dialog button: a small bordered box with the label.
/// `area` should be three rows tall.
fn draw_boxed_button(f: &mut Frame, area: Rect, label: &str) {
    let width = (label.len() + 4) as u16; // borders + padding
    let [_, button, _] = Layout::horizontal([
        Constraint::Min(1),
        Constraint::Length(width),
        Constraint::Min(1),
    ])
    .areas(area);
    f.render_widget(Block::bordered(), button);
    f.render_widget(
        Paragraph::new(format!(" {label} "))
            .alignment(Alignment::Center)
            .style(selected_style()),
        button.inner(Margin::new(1, 1)),
    );
}

/// nmtui-style vertical button column (as in "Edit a connection").
fn draw_button_column(f: &mut Frame, area: Rect, buttons: &[(&str, bool)]) {
    let mut y = area.y;
    for (label, focused) in buttons {
        if y >= area.bottom() {
            break;
        }
        let style = if *focused { selected_style() } else { Style::default() };
        f.render_widget(
            Paragraph::new(format!(" <{label}> ")).style(style),
            Rect { x: area.x, y, width: area.width, height: 1 },
        );
        y += 2; // one blank row between buttons, like nmtui
    }
}

/// newt-style scrollbar: `↑` top, `↓` bottom, `▮` thumb, `▒` filler.
fn draw_scrollbar(f: &mut Frame, area: Rect, selected: usize, total: usize) {
    if area.height == 0 || total == 0 {
        return;
    }
    let height = area.height as usize;
    let thumb = if total > 1 && height > 2 {
        1 + selected * (height - 3) / (total - 1)
    } else {
        height / 2
    };
    for row in 0..height {
        let (ch, style) = if row == 0 {
            ('↑', dim_style())
        } else if row == height - 1 {
            ('↓', dim_style())
        } else if row == thumb {
            ('▮', Style::default().fg(Color::White))
        } else {
            ('▒', dim_style())
        };
        f.render_widget(
            Paragraph::new(ch.to_string()).style(style),
            Rect { x: area.x, y: area.y + row as u16, width: 1, height: 1 },
        );
    }
}

/// Every deeper window is inset a little further, so the stack is visible.
fn layer_rect(area: Rect, depth: usize) -> Rect {
    let max_dx = (area.width / 2).saturating_sub(15);
    let max_dy = (area.height / 2).saturating_sub(5);
    let dx = ((8 + 8 * depth) as u16).min(max_dx);
    let dy = ((2 + 2 * depth) as u16).min(max_dy);
    centered(
        area,
        area.width.saturating_sub(2 * dx),
        area.height.saturating_sub(2 * dy),
    )
}

// ----------------------------------------------------------------- screens

fn draw_menu(f: &mut Frame, area: Rect, selected: usize) {
    let rect = centered(area, 32, 12);
    let inner = dialog(f, rect, "iwtui");

    let [subtitle, _, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    f.render_widget(
        Paragraph::new(" Please select an option").style(dim_style()),
        subtitle,
    );

    let items: Vec<ListItem> = MENU_ENTRIES
        .iter()
        .map(|entry| ListItem::new(format!(" {entry}")))
        .collect();
    let mut state =
        ListState::default().with_selected(Some(selected.min(MENU_ENTRIES.len() - 1)));
    f.render_stateful_widget(
        List::new(items).highlight_style(selected_style()),
        list,
        &mut state,
    );
}

fn draw_wifi(f: &mut Frame, area: Rect, app: &App) {
    let (focus, selected, button) = match &app.screen {
        Screen::Wifi { focus, selected, button } => (*focus, *selected, *button),
        _ => return,
    };

    // nmtui's "Activate a connection"-sized dialog: a big centered box.
    let height = area.height.saturating_sub(4).clamp(12, 30);
    let width = area.width.saturating_sub(8).clamp(40, 78);
    let rect = centered(area, width, height);
    let inner = dialog(f, rect, "Wi-Fi Networks");

    let [status, _, body, _] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let status_text = match (&app.wifi, &app.last_error) {
        (Some(w), _) => format!(
            " State: {}{}",
            w.state,
            if w.scanning { "  (scanning)" } else { "" }
        ),
        (None, Some(err)) => format!(" {err}"),
        (None, None) => " No Wi-Fi station (radio off? iwd running?)".to_owned(),
    };
    f.render_widget(Paragraph::new(status_text), status);

    let [list_region, button_region] =
        Layout::horizontal([Constraint::Min(30), Constraint::Length(18)]).areas(body);

    f.render_widget(Block::bordered(), list_region);
    let list_inner = list_region.inner(Margin::new(1, 1));
    let network_count = app.wifi.as_ref().map(|w| w.networks.len()).unwrap_or(0);

    match app.wifi.as_ref() {
        Some(w) if network_count > 0 => {
            // reserve a scrollbar column inside the list, like newt
            let items_area = Rect {
                width: list_inner.width.saturating_sub(3),
                ..list_inner
            };
            let scrollbar_area = Rect {
                x: list_inner.x + list_inner.width.saturating_sub(1),
                width: 1,
                ..list_inner
            };

            let name_width = items_area.width.saturating_sub(26).max(1) as usize;
            let items: Vec<ListItem> = w
                .networks
                .iter()
                .map(|n| {
                    let marker = if n.connected { '*' } else { ' ' };
                    let signal = n
                        .signal
                        .map(|s| format!("{s:>4}"))
                        .unwrap_or_else(|| "    ".to_owned());
                    ListItem::new(format!(
                        " {marker}{:<name_width$} {:<11} {signal}",
                        n.name,
                        n.security,
                        name_width = name_width,
                    ))
                })
                .collect();

            let cursor = selected.min(network_count - 1);
            let highlight = if focus == Focus::NetworkList {
                selected_style()
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            let mut state = ListState::default().with_selected(Some(cursor));
            f.render_stateful_widget(
                List::new(items).highlight_style(highlight),
                items_area,
                &mut state,
            );
            draw_scrollbar(f, scrollbar_area, cursor, network_count);
        }
        _ => {
            let message = if app.wifi.is_some() {
                " No networks yet - scanning..."
            } else {
                " No Wi-Fi station"
            };
            f.render_widget(Paragraph::new(message).style(dim_style()), list_inner);
        }
    }

    let buttons: Vec<(&str, bool)> = WIFI_BUTTONS
        .iter()
        .enumerate()
        .map(|(i, label)| (*label, focus == Focus::Buttons && i == button))
        .collect();
    draw_button_column(f, button_region, &buttons);
}

// ---------------------------------------------------------------- overlays

fn draw_overlays(f: &mut Frame, area: Rect, app: &App) {
    for (depth, overlay) in app.overlays.iter().enumerate() {
        // Each layer dims everything below it, so depth is visible.
        f.buffer_mut()
            .set_style(area, Style::default().add_modifier(Modifier::DIM));
        let rect = layer_rect(area, depth);
        draw_shadow(f, rect);
        f.render_widget(Clear, rect);
        match overlay {
            Overlay::NetworkDetails { entry, button } => {
                draw_details(f, rect, entry, *button);
            }
            Overlay::Passphrase { network_name, input, .. } => {
                draw_passphrase(f, rect, network_name, input);
            }
            Overlay::ConfirmForget { entry } => {
                draw_confirm_forget(f, rect, entry);
            }
            Overlay::Message { title, text } => {
                draw_message(f, rect, title, text);
            }
        }
    }
}

fn draw_details(f: &mut Frame, rect: Rect, entry: &NetworkEntry, button: usize) {
    let inner = dialog(f, rect, &entry.name);

    let [_, info, _, row_a, row_b, _] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    let signal = entry
        .signal
        .map(|s| format!("{s} dBm"))
        .unwrap_or_else(|| "-".to_owned());
    f.render_widget(
        Paragraph::new(format!(
            " Name     : {}\n Security : {}\n Signal   : {}\n Status   : {}",
            entry.name,
            entry.security,
            signal,
            if entry.connected { "connected" } else { "not connected" },
        )),
        info,
    );

    draw_button_row(f, row_a, &[
        (DETAIL_BUTTONS[0], button == 0),
        (DETAIL_BUTTONS[1], button == 1),
    ]);
    draw_button_row(f, row_b, &[
        (DETAIL_BUTTONS[2], button == 2),
        (DETAIL_BUTTONS[3], button == 3),
    ]);
}

fn draw_passphrase(f: &mut Frame, rect: Rect, network_name: &str, input: &str) {
    let inner = dialog(f, rect, "Passphrase");

    let [_, label, _, field, hint, _stretch, buttons] = Layout::vertical([
        Constraint::Length(1), // top padding
        Constraint::Length(1), // label
        Constraint::Length(1), // gap
        Constraint::Length(1), // entry field
        Constraint::Length(1), // hint
        Constraint::Min(1),    // stretch
        Constraint::Length(1), // buttons row
    ])
    .areas(inner);

    f.render_widget(
        Paragraph::new(format!(" Passphrase for '{network_name}':")),
        label,
    );

    // nmtui-style entry field: masked input followed by underscores.
    let field_width = inner.width.saturating_sub(4) as usize;
    let masked = "*".repeat(input.len());
    let padding = "_".repeat(field_width.saturating_sub(masked.len()));
    f.render_widget(Paragraph::new(format!(" {masked}{padding}")), field);

    f.render_widget(
        Paragraph::new(" Enter: accept   Esc: cancel").style(dim_style()),
        hint,
    );

    draw_button_row_right(f, buttons, &[("Cancel", false), ("OK", true)]);
}

fn draw_confirm_forget(f: &mut Frame, rect: Rect, entry: &NetworkEntry) {
    let inner = dialog(f, rect, "Forget Network");

    let [_, message, _, button, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(inner);

    f.render_widget(
        Paragraph::new(vec![
            Line::from(format!(" Forget '{}'", entry.name)),
            Line::from(" The saved passphrase and profile will be deleted."),
        ]),
        message,
    );
    draw_boxed_button(f, button, "OK");
    f.render_widget(
        Paragraph::new(" Enter: forget   Esc: cancel").style(dim_style()),
        hint,
    );
}

fn draw_message(f: &mut Frame, rect: Rect, title: &str, text: &str) {
    let inner = dialog(f, rect, title);

    let [message, _, button] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1), Constraint::Length(3)])
            .areas(inner);

    f.render_widget(
        Paragraph::new(text.to_owned()).wrap(Wrap { trim: false }),
        message,
    );
    draw_boxed_button(f, button, "OK");
}
