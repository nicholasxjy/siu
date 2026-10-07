mod agents;
mod app;
mod cache;
mod github;
mod library;
mod market;
mod mcp;
mod net;
mod skills;
#[cfg(test)]
mod tests;
mod theme;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::agents::Env;
use crate::app::App;
use crate::library::Store;

const HELP: &str = "siu — manage your coding agents' MCP servers and skills

Usage: siu [--help | --version]

Keeps one library of MCP servers and skills in ~/.siu (or $SIU_HOME) and
writes each into the agents you pick: Claude Code, Claude Desktop, Codex,
Gemini CLI, OpenCode, Cursor, Copilot CLI, Droid and ~/.agents/skills.
Colors follow the terminal's dark or light background; SIU_THEME=dark or
SIU_THEME=light picks one. Press ? inside for keys, t to switch theme.";

fn root(env: &Env) -> PathBuf {
    std::env::var_os("SIU_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| env.home.join(".siu"))
}

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("-h" | "--help") => {
            println!("{HELP}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("siu {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(other) => anyhow::bail!("unknown argument {other}; try --help"),
        None => {}
    }
    let env = Env::from_system();
    let store = Store::open(&env, root(&env))?;
    let mut app = App::new(store, env);
    // asked of the terminal before the TUI takes its input over
    app.theme = theme::Theme::for_terminal(theme::detect());
    app.start();
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    app.shutdown();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.quit {
        app.poll();
        app.tick();
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(80))?
            && let Event::Key(k) = event::read()?
            && k.kind != KeyEventKind::Release
        {
            app.on_key(k);
        }
    }
    Ok(())
}
