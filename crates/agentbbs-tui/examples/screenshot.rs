//! Render the AgentBBS TUI to text frames — a headless, reproducible "screenshot"
//! of the SSH / local-terminal UI. This is a REAL ratatui render (via the
//! `TestBackend`), not a mock: the same `App::render` that drives a live SSH PTY
//! draws into an off-screen buffer which we dump as text.
//!
//! Run: `cargo run -p agentbbs-tui --example screenshot`

use agentbbs_tui::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

/// Render the app into an off-screen w×h buffer and return it as text rows.
fn frame(app: &App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("test backend");
    term.draw(|f| app.render(f)).expect("draw");
    let buf = term.backend().buffer().clone();
    let width = buf.area.width as usize;
    let cells: Vec<String> = buf
        .content()
        .iter()
        .map(|c| c.symbol().to_string())
        .collect();
    cells
        .chunks(width)
        .map(|row| row.concat().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn show(label: &str, app: &App, w: u16, h: u16) {
    let bar = "═".repeat(w as usize);
    println!("\n{bar}\n {label}\n{bar}\n{}", frame(app, w, h));
}

fn main() {
    let (w, h) = (96u16, 28u16);

    let mut app = App::in_memory();
    show("1. Connect splash", &app, w, h);

    app.on_key(key(KeyCode::Enter)); // splash → main
    show("2. Main menu (lightbar)", &app, w, h);

    app.on_key(key(KeyCode::Char('m'))); // → message bases
    show("3. Message bases", &app, w, h);

    app.on_key(key(KeyCode::Enter)); // open selected board → read
    show("4. Reading a board", &app, w, h);

    // Fresh session for the Arena screen (avoids back-nav ambiguity).
    let mut a = App::in_memory();
    a.on_key(key(KeyCode::Enter));
    a.on_key(key(KeyCode::Char('a'))); // → arena
    show("5. Arena leaderboard", &a, w, h);
}
