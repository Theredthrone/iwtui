use std::io;
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
