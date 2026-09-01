//! iwtui — a drop-in nmtui-style TUI for iwd (Intel Wireless Daemon).
//!
//! This binary owns terminal setup/teardown, CLI parsing, the async event
//! loop and top-level error handling. The rest lives in four modules:
//! `app` (state machine + actions), `iwd` (D-Bus), `agent` (credential
//! prompts), `system` (hostname), and `ui` (everything you see).

mod agent;
mod app;
mod iwd;
mod system;
mod ui;

use std::io;

use crossterm::event::EventStream;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};

use app::App;

pub type AppError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type AppResult<T> = Result<T, AppError>;

pub fn err(msg: impl Into<String>) -> AppError {
    Box::new(io::Error::other(msg.into()))
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_usage() {
    println!(
        "iwtui {VERSION} — nmtui-style TUI for iwd (Intel Wireless Daemon)

Usage: iwtui [OPTION]

Options:
  -h, --help       Print this help and exit
  -V, --version    Print version and exit

Requirements:
  iwd running and reachable on the system D-Bus. Password prompts use the
  built-in iwd agent, so run iwtui with permission to talk to iwd (usually
  root or the netdev group — the same requirements as iwctl). Setting the
  system hostname works as any user: iwtui asks for the root password and
  applies it via sudo.

Keys:
  Arrows / hjkl    navigate         Tab       list <-> buttons
  Enter            activate         Esc / q   back / quit
  ?                help overlay     Ctrl+R    show password in dialogs

License GPLv3+: GNU GPL version 3 or later <https://gnu.org/licenses/gpl.html>.
This is free software: you are free to change and redistribute it.
There is NO WARRANTY, to the extent permitted by law."
    );
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> AppResult<()> {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("-h" | "--help") => {
            print_usage();
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("iwtui {VERSION}");
            return Ok(());
        }
        Some(other) => {
            eprintln!("iwtui: unknown argument: {other}\nTry 'iwtui --help'.");
            std::process::exit(2);
        }
    }

    // Restore the terminal even if we panic.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("iwtui: {e}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> AppResult<()> {
    let mut app = App::new().await;
    let mut events = EventStream::new();
    let mut status_tick = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut last_status_at: Option<std::time::Instant> = None;
    let mut last_status_msg: Option<String> = None;

    // Clean up the terminal even when logind/systemd kills us
    // (SIGTERM/SIGHUP would otherwise bypass all terminal restoration).
    let mut sig_term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sig_hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

    loop {
        terminal.draw(|f| ui::draw(f, &app))?;

        app.poll_cancel_flag();
        app.poll_agent_requests();
        app.poll_iwd_events();
        app.poll_app_events();

        // Status messages expire after 3 seconds.
        if app.status_message != last_status_msg {
            last_status_msg = app.status_message.clone();
            last_status_at = Some(std::time::Instant::now());
        }
        if let Some(t) = last_status_at {
            if t.elapsed() > std::time::Duration::from_secs(3) {
                app.status_message = None;
                last_status_at = None;
                last_status_msg = None;
            }
        }

        tokio::select! {
            maybe_ev = events.next() => {
                let Some(ev_result) = maybe_ev else { break };
                let ev = ev_result?;
                if app.handle_event(ev).await {
                    break;
                }
            }
            _ = status_tick.tick() => {}
            _ = sig_term.recv() => { app.should_quit = true; }
            _ = sig_hup.recv() => { app.should_quit = true; }
        }

        if app.should_quit {
            break;
        }
    }

    app.shutdown().await;
    Ok(())
}use std::io;
use std::time::Duration;

use crossterm::event::EventStream;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};

mod agent;
mod app;
mod iwd;
mod system;
mod ui;

use app::App;

pub type AppError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type AppResult<T> = Result<T, AppError>;

pub fn err(msg: impl Into<String>) -> AppError {
    Box::new(std::io::Error::new(std::io::ErrorKind::Other, msg.into()))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> AppResult<()> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("iwtui: {e}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> AppResult<()> {
    let mut app = App::new().await;
    let mut events = EventStream::new();
    let mut status_tick = tokio::time::interval(Duration::from_millis(100));
    let mut last_status_at: Option<std::time::Instant> = None;

    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;

        app.poll_agent_requests();
        app.poll_iwd_events();
        app.poll_app_events();

        if app.status_message.is_some() && last_status_at.is_none() {
            last_status_at = Some(std::time::Instant::now());
        }

        if let Some(t) = last_status_at {
            if t.elapsed() > Duration::from_secs(3) {
                app.status_message = None;
                last_status_at = None;
            }
        }

        tokio::select! {
            maybe_ev = events.next() => {
                let Some(ev_result) = maybe_ev else { break };
                let ev = ev_result?;
                if app.handle_event(ev).await {
                    break;
                }
            }
            _ = app.cancel_notify.notified() => {
                app.dismiss_agent_dialog();
            }
            _ = status_tick.tick() => {}
        }

        if app.should_quit {
            break;
        }
    }

    app.shutdown().await;
    Ok(())
}
