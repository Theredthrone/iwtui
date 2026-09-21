//! iwtui - an nmtui-style TUI for iwd.
//!
//! This file only wires things together: terminal setup, the three
//! background tasks (input, iwd signals, agent), and the event loop.
//! State and key handling live in `app`, rendering in `ui`, all D-Bus
//! work in `iwd`.

mod app;
mod iwd;
mod ui;

use std::{error::Error, io, time::Duration};

use app::{App, AppEvent};
use crossterm::event::EventStream;
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;
use zbus::Connection;

/// Restores the terminal even if main returns early with an error.
/// (Note: the release profile uses panic=abort, so a panic still leaves
/// the terminal raw - run `reset` if that ever happens.)
struct TerminalGuard;

impl TerminalGuard {
    fn setup() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let conn = Connection::system().await?;

    // Every background task talks to the main loop through this channel.
    let (events, mut event_queue) = mpsc::channel::<AppEvent>(64);

    // Task 1: keyboard / resize events.
    let input_events = events.clone();
    tokio::spawn(async move {
        let mut stream = EventStream::new();
        while let Some(event) = stream.next().await {
            match event {
                Ok(e) => {
                    if input_events.send(AppEvent::Input(e)).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Task 2: iwd signals -> IwdChanged. This is what keeps the network
    // list fresh without any manual refresh.
    let watcher_events = events.clone();
    let watcher_conn = conn.clone();
    tokio::spawn(async move {
        if let Err(e) = iwd::watch(watcher_conn, watcher_events).await {
            eprintln!("iwd watcher stopped: {e}");
        }
    });

    // Task 3: register as iwd's agent so passphrase requests reach the
    // UI as ordinary events.
    if let Err(e) = iwd::register_agent(&conn, events.clone()).await {
        eprintln!("warning: agent registration failed: {e}");
    }

    let mut app = App::new(conn.clone(), events);
    app.reload().await;

    let _terminal = TerminalGuard::setup()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    // One scan emits one signal per network: debounce bursts into a
    // single reload.
    let mut refresh = tokio::time::interval(Duration::from_millis(250));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Signal strength (dBm) is not part of the change signals, so poll
    // for it while the Wi-Fi screen is open.
    let mut poll = tokio::time::interval(Duration::from_secs(3));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while !app.quit {
        tokio::select! {
            event = event_queue.recv() => match event {
                None => break,
                Some(AppEvent::Input(e)) => app.handle_input(e).await,
                Some(AppEvent::IwdChanged) => app.dirty = true,
                Some(AppEvent::AgentQuery(q)) => app.handle_agent_query(q),
                Some(AppEvent::AgentCancelled(r)) => app.handle_agent_cancel(r),
                Some(AppEvent::Error(text)) => app.push_error(text),
            },
            _ = refresh.tick() => {
                if app.dirty {
                    app.reload().await;
                    app.dirty = false;
                }
            }
            _ = poll.tick() => {
                if app.on_wifi_screen() {
                    app.reload().await;
                }
            }
        }
        // Draw on every pass: the screen always reflects current state.
        terminal.draw(|frame| ui::draw(frame, &app))?;
    }

    let _ = iwd::unregister_agent(&conn).await;
    Ok(())
}
