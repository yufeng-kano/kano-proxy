//! Interactive ratatui screens for `init` and `add` (docs/cli.md § Command
//! surface). Every command also runs with `--no-tui` + flags — same code
//! path underneath, different input source — so these screens are strictly
//! input collectors: no network, no state writes.

use std::io;

use anyhow::{bail, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Terminal;

pub fn require_tty() -> Result<()> {
    use std::io::IsTerminal;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("not a terminal — use --no-tui with flags (see --help)");
    }
    Ok(())
}

struct Screen {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl Screen {
    fn open() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

fn is_cancel(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Esc) || (code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL))
}

/// One-line text input. Enter accepts (falling back to `default` when blank),
/// Esc / Ctrl-C cancels the whole command.
pub fn input(title: &str, label: &str, default: &str) -> Result<String> {
    let mut screen = Screen::open()?;
    let mut value = String::new();
    loop {
        let default_hint = if default.is_empty() { String::new() } else { format!(" [{default}]") };
        screen.terminal.draw(|f| {
            let chunks = Layout::vertical([Constraint::Length(3), Constraint::Length(3), Constraint::Min(0)])
                .split(f.area());
            f.render_widget(
                Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)),
                chunks[0],
            );
            let line = Line::from(vec![
                Span::styled(format!("{label}{default_hint}: "), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(value.clone()),
                Span::styled("▏", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            ]);
            f.render_widget(Paragraph::new(line), chunks[1]);
            f.render_widget(
                Paragraph::new("Enter accepts · Esc cancels").style(Style::default().add_modifier(Modifier::DIM)),
                chunks[2],
            );
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if is_cancel(key.code, key.modifiers) {
                bail!("cancelled");
            }
            match key.code {
                KeyCode::Enter => {
                    let v = if value.trim().is_empty() { default.to_string() } else { value.trim().to_string() };
                    if !v.is_empty() {
                        return Ok(v);
                    }
                }
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c) => value.push(c),
                _ => {}
            }
        }
    }
}

/// One-line masked input for a secret (the local API key): renders bullets,
/// never the characters — shoulder surfers and terminal recordings see
/// nothing. Enter with nothing typed returns an empty string ("no key").
pub fn input_secret(title: &str, label: &str) -> Result<String> {
    let mut screen = Screen::open()?;
    let mut value = String::new();
    loop {
        screen.terminal.draw(|f| {
            let chunks = Layout::vertical([Constraint::Length(3), Constraint::Length(3), Constraint::Min(0)])
                .split(f.area());
            f.render_widget(
                Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)),
                chunks[0],
            );
            let line = Line::from(vec![
                Span::styled(format!("{label}: "), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("\u{2022}".repeat(value.chars().count())),
                Span::styled("\u{258f}", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            ]);
            f.render_widget(Paragraph::new(line), chunks[1]);
            f.render_widget(
                Paragraph::new("Enter accepts (blank = none) \u{b7} input is hidden \u{b7} Esc cancels")
                    .style(Style::default().add_modifier(Modifier::DIM)),
                chunks[2],
            );
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if is_cancel(key.code, key.modifiers) {
                bail!("cancelled");
            }
            match key.code {
                KeyCode::Enter => return Ok(value.trim().to_string()),
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c) => value.push(c),
                _ => {}
            }
        }
    }
}

/// Single choice from a short fixed list (e.g. API type).
pub fn choose(title: &str, options: &[&str]) -> Result<usize> {
    let mut screen = Screen::open()?;
    let mut state = ListState::default();
    state.select(Some(0));
    loop {
        screen.terminal.draw(|f| {
            let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(1)])
                .split(f.area());
            f.render_widget(
                Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)),
                chunks[0],
            );
            let items: Vec<ListItem> = options.iter().map(|o| ListItem::new(*o)).collect();
            let list = List::new(items)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("› ");
            f.render_stateful_widget(list, chunks[1], &mut state);
            f.render_widget(
                Paragraph::new("↑/↓ move · Enter accepts · Esc cancels")
                    .style(Style::default().add_modifier(Modifier::DIM)),
                chunks[2],
            );
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if is_cancel(key.code, key.modifiers) {
                bail!("cancelled");
            }
            let selected = state.selected().unwrap_or(0);
            match key.code {
                KeyCode::Enter => return Ok(selected),
                KeyCode::Up | KeyCode::Char('k') => state.select(Some(selected.saturating_sub(1))),
                KeyCode::Down | KeyCode::Char('j') => {
                    state.select(Some((selected + 1).min(options.len().saturating_sub(1))))
                }
                _ => {}
            }
        }
    }
}

/// Searchable model picker whose first row is "All models (follow local
/// server)" — the default, which stores no filter (docs/cli.md § Command
/// surface). Returns `None` for that row, `Some(subset)` otherwise.
pub fn pick_models(title: &str, models: &[String]) -> Result<Option<Vec<String>>> {
    let all_row = "All models (follow local server)";
    let mut screen = Screen::open()?;
    let mut query = String::new();
    let mut selected: Vec<bool> = vec![false; models.len()];
    let mut state = ListState::default();
    state.select(Some(0));
    loop {
        let visible: Vec<usize> = models
            .iter()
            .enumerate()
            .filter(|(_, m)| query.is_empty() || m.to_lowercase().contains(&query.to_lowercase()))
            .map(|(i, _)| i)
            .collect();
        let row_count = visible.len() + 1;
        screen.terminal.draw(|f| {
            let chunks = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(f.area());
            f.render_widget(
                Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)),
                chunks[0],
            );
            f.render_widget(Paragraph::new(format!("Search: {query}▏")), chunks[1]);
            let mut items: Vec<ListItem> = vec![ListItem::new(Line::from(Span::styled(
                all_row,
                Style::default().add_modifier(Modifier::BOLD),
            )))];
            for &i in &visible {
                let mark = if selected[i] { "[x] " } else { "[ ] " };
                items.push(ListItem::new(format!("{mark}{}", models[i])));
            }
            let list = List::new(items)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("› ");
            f.render_stateful_widget(list, chunks[2], &mut state);
            f.render_widget(
                Paragraph::new("↑/↓ move · Space toggles · Enter accepts · type to search · Esc cancels")
                    .style(Style::default().add_modifier(Modifier::DIM)),
                chunks[3],
            );
        })?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if is_cancel(key.code, key.modifiers) {
                bail!("cancelled");
            }
            let cursor = state.selected().unwrap_or(0).min(row_count.saturating_sub(1));
            match key.code {
                KeyCode::Enter => {
                    if cursor == 0 {
                        return Ok(None);
                    }
                    let chosen: Vec<String> = models
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| selected[*i])
                        .map(|(_, m)| m.clone())
                        .collect();
                    // Enter on a row with nothing toggled means "just this one".
                    if chosen.is_empty() {
                        if let Some(&idx) = visible.get(cursor - 1) {
                            return Ok(Some(vec![models[idx].clone()]));
                        }
                        return Ok(None);
                    }
                    return Ok(Some(chosen));
                }
                KeyCode::Char(' ') => {
                    if cursor > 0 {
                        if let Some(&idx) = visible.get(cursor - 1) {
                            selected[idx] = !selected[idx];
                        }
                    }
                }
                KeyCode::Up => state.select(Some(cursor.saturating_sub(1))),
                KeyCode::Down => state.select(Some((cursor + 1).min(row_count.saturating_sub(1)))),
                KeyCode::Backspace => {
                    query.pop();
                    state.select(Some(0));
                }
                KeyCode::Char(c) => {
                    query.push(c);
                    state.select(Some(0));
                }
                _ => {}
            }
        }
    }
}
