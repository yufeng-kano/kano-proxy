//! Interactive ratatui screens for `login`, `tunnel add` and `agent setup`
//! (docs/cli.md § Command surface). Every command also runs with `--no-tui` and
//! flags (same code path underneath, different input source), so these
//! screens are strictly input collectors — no network, no state writes.
//!
//! Editing flows (`agent setup`, `tunnel add`) are built like an editor, not a
//! wizard (docs/cli.md § Navigation): a **root page** lists every item with its
//! value; Enter opens an item's page, where Enter applies the change *to the
//! draft* and Esc goes back without it. Nothing reaches the server or a config
//! file until **Tab on the root page** saves the whole draft. **Esc twice on the
//! root page** exits — and when saving would change something, it first asks
//! `Save changes? Y / N / Esc`, the way nano asks before closing a modified
//! buffer. `login` is a short wizard: Esc steps back, Esc twice exits.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;

/// The user quit (Esc twice, Ctrl-C, or "No" on a confirmation). Not a failure: `main`
/// prints it as a plain line — never `error: …` — and exits 130. The message says what
/// state things were left in when that is not simply "nothing changed".
#[derive(Debug)]
pub struct Cancelled(pub Option<String>);

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_deref().unwrap_or("Cancelled — nothing was changed."))
    }
}

impl std::error::Error for Cancelled {}

pub fn cancelled() -> anyhow::Error {
    Cancelled(None).into()
}

/// What a screen hands back: the answer, "go back" (the screen's edit is dropped), or
/// "exit" (Esc pressed twice in a row).
#[derive(Debug, PartialEq, Eq)]
pub enum Nav<T> {
    Next(T),
    Back,
    Exit,
}

/// The root page's outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum RootAction {
    /// Open the item at this row.
    Edit(usize),
    /// Tab: save the whole draft.
    Save,
    /// Esc twice: leave (the flow asks about unsaved changes first).
    Exit,
}

/// The answer to `Save changes?`.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveChoice {
    Yes,
    No,
    /// Esc: back to the root page, nothing decided.
    Cancel,
}

/// One row of a root page.
pub struct RootRow {
    pub label: String,
    pub value: String,
    /// Differs from what is in place now — marked so the draft's edits are visible.
    pub changed: bool,
}

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

/// Set by an Esc that went back; cleared by any other key on any screen. A
/// second Esc while it is set — the "twice in a row" — quits the command.
static ESC_ARMED: AtomicBool = AtomicBool::new(false);

/// What a key press means before the screen looks at it.
enum Key {
    Back,
    Exit,
    Other(KeyCode),
}

/// Reads one key press. Ctrl-C quits at once, unconditionally; Esc means "exit" when
/// the previous key was also Esc, otherwise "back".
fn read_key() -> Result<Key> {
    loop {
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Err(cancelled());
            }
            if key.code == KeyCode::Esc {
                if ESC_ARMED.swap(true, Ordering::SeqCst) {
                    ESC_ARMED.store(false, Ordering::SeqCst);
                    return Ok(Key::Exit);
                }
                return Ok(Key::Back);
            }
            ESC_ARMED.store(false, Ordering::SeqCst);
            return Ok(Key::Other(key.code));
        }
    }
}

/// The navigation half of every footer. Right after an Esc it changes to say that the
/// next Esc quits — on the first screen, where there is nothing to go back to, that line
/// is the only visible response to the first Esc.
fn nav_hint() -> &'static str {
    if ESC_ARMED.load(Ordering::SeqCst) {
        "Press Esc again to exit · any other key continues"
    } else {
        "Esc back · Esc twice exits"
    }
}

fn note_height(note: &[&str], width: usize) -> u16 {
    if note.is_empty() {
        0
    } else {
        note.iter().map(|l| l.chars().count().div_ceil(width).max(1) as u16).sum::<u16>() + 1
    }
}

fn dim(text: &str) -> Paragraph<'_> {
    Paragraph::new(text).style(Style::default().add_modifier(Modifier::DIM))
}

/// One-line text input with a block of lines rendered between the title and
/// the field (Enter accepts, falling back to `default` when blank),
/// and `initial` already typed into it (what the user answered before going
/// back, so the step is edited rather than retyped). The screens run on the
/// terminal's alternate screen, so anything printed *before* a screen opens
/// is invisible while it is up — the authorize URL `login` needs the user to
/// visit has to be drawn on the code screen itself (docs/cli.md § Command
/// surface). Long lines wrap rather than clip.
pub fn input_with_note(title: &str, note: &[&str], label: &str, default: &str, initial: &str) -> Result<Nav<String>> {
    let mut screen = Screen::open()?;
    let mut value = initial.to_string();
    let note_text = note.join("\n");
    loop {
        let default_hint = if default.is_empty() { String::new() } else { format!(" [{default}]") };
        screen.terminal.draw(|f| {
            let width = f.area().width.max(1) as usize;
            let chunks = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(note_height(note, width)),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .split(f.area());
            f.render_widget(Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)), chunks[0]);
            if !note.is_empty() {
                f.render_widget(Paragraph::new(note_text.as_str()).wrap(Wrap { trim: false }), chunks[1]);
            }
            let line = Line::from(vec![
                Span::styled(format!("{label}{default_hint}: "), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(value.clone()),
                Span::styled("▏", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            ]);
            f.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), chunks[2]);
            f.render_widget(dim(&format!("Enter accepts · {}", nav_hint())), chunks[3]);
        })?;
        match read_key()? {
            Key::Back => return Ok(Nav::Back),
            Key::Exit => return Ok(Nav::Exit),
            Key::Other(KeyCode::Enter) => {
                let v = if value.trim().is_empty() { default.to_string() } else { value.trim().to_string() };
                if !v.is_empty() {
                    return Ok(Nav::Next(v));
                }
            }
            Key::Other(KeyCode::Backspace) => {
                value.pop();
            }
            Key::Other(KeyCode::Char(c)) => value.push(c),
            Key::Other(_) => {}
        }
    }
}

/// One-line masked input for a secret (the local API key): renders bullets,
/// never the characters — shoulder surfers and terminal recordings see
/// nothing. Enter with nothing typed returns an empty string ("no key").
pub fn input_secret(title: &str, label: &str, initial: &str) -> Result<Nav<String>> {
    let mut screen = Screen::open()?;
    let mut value = initial.to_string();
    loop {
        screen.terminal.draw(|f| {
            let chunks = Layout::vertical([Constraint::Length(3), Constraint::Length(3), Constraint::Min(0)]).split(f.area());
            f.render_widget(Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)), chunks[0]);
            let line = Line::from(vec![
                Span::styled(format!("{label}: "), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("\u{2022}".repeat(value.chars().count())),
                Span::styled("\u{258f}", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            ]);
            f.render_widget(Paragraph::new(line), chunks[1]);
            f.render_widget(dim(&format!("Enter accepts (blank = none) · input is hidden · {}", nav_hint())), chunks[2]);
        })?;
        match read_key()? {
            Key::Back => return Ok(Nav::Back),
            Key::Exit => return Ok(Nav::Exit),
            Key::Other(KeyCode::Enter) => return Ok(Nav::Next(value.trim().to_string())),
            Key::Other(KeyCode::Backspace) => {
                value.pop();
            }
            Key::Other(KeyCode::Char(c)) => value.push(c),
            Key::Other(_) => {}
        }
    }
}

/// Single choice from a short fixed list (e.g. API type), with explanatory
/// lines between the title and the list — what the choice means, so the user
/// never has to guess from a bare identifier. `start` is where the cursor
/// begins (the previous answer when re-entering the step).
pub fn choose(title: &str, note: &[&str], options: &[&str], start: usize) -> Result<Nav<usize>> {
    choose_at(title, note, options, None, start)
}

/// `choose` with a `default` row — labelled `(default)` in words, so what the
/// pre-selection is never depends on spotting the highlight — and a line under
/// the list naming the row the cursor is on, i.e. what Enter will pick.
pub fn choose_default(title: &str, note: &[&str], options: &[&str], default: usize, start: usize) -> Result<Nav<usize>> {
    let last = options.len().saturating_sub(1);
    choose_at(title, note, options, Some(default.min(last)), start.min(last))
}

fn choose_at(title: &str, note: &[&str], options: &[&str], default: Option<usize>, start: usize) -> Result<Nav<usize>> {
    let mut screen = Screen::open()?;
    let mut state = ListState::default();
    state.select(Some(start));
    let note_text = note.join("\n");
    loop {
        screen.terminal.draw(|f| {
            let width = f.area().width.max(1) as usize;
            let chunks = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(note_height(note, width)),
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(f.area());
            f.render_widget(Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)), chunks[0]);
            if !note.is_empty() {
                f.render_widget(Paragraph::new(note_text.as_str()).wrap(Wrap { trim: false }), chunks[1]);
            }
            let items: Vec<ListItem> = options
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    if default == Some(i) {
                        ListItem::new(Line::from(vec![
                            Span::styled((*o).to_string(), Style::default().add_modifier(Modifier::BOLD)),
                            Span::styled("  (default)", Style::default().add_modifier(Modifier::DIM)),
                        ]))
                    } else {
                        ListItem::new(*o)
                    }
                })
                .collect();
            let list = List::new(items)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("› ");
            f.render_stateful_widget(list, chunks[2], &mut state);
            let current = state.selected().and_then(|i| options.get(i)).copied().unwrap_or("");
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Enter picks: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(current),
                ])),
                chunks[3],
            );
            f.render_widget(dim(&format!("↑/↓ move · Enter accepts · {}", nav_hint())), chunks[4]);
        })?;
        let selected = state.selected().unwrap_or(0);
        match read_key()? {
            Key::Back => return Ok(Nav::Back),
            Key::Exit => return Ok(Nav::Exit),
            Key::Other(KeyCode::Enter) => return Ok(Nav::Next(selected)),
            Key::Other(KeyCode::Up) | Key::Other(KeyCode::Char('k')) => state.select(Some(selected.saturating_sub(1))),
            Key::Other(KeyCode::Down) | Key::Other(KeyCode::Char('j')) => {
                state.select(Some((selected + 1).min(options.len().saturating_sub(1))))
            }
            Key::Other(_) => {}
        }
    }
}

/// Searchable model picker whose first row is "All models (follow local
/// server)" — the default, which stores no filter (docs/cli.md § Command
/// surface). Returns `None` for that row, `Some(subset)` otherwise.
/// `preselected` re-toggles a previous answer.
pub fn pick_models(title: &str, models: &[String], preselected: &[String]) -> Result<Nav<Option<Vec<String>>>> {
    let all_row = "All models (follow local server)";
    let mut screen = Screen::open()?;
    let mut query = String::new();
    let mut selected: Vec<bool> = models.iter().map(|m| preselected.contains(m)).collect();
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
            f.render_widget(Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)), chunks[0]);
            f.render_widget(Paragraph::new(format!("Search: {query}▏")), chunks[1]);
            let mut items: Vec<ListItem> =
                vec![ListItem::new(Line::from(Span::styled(all_row, Style::default().add_modifier(Modifier::BOLD))))];
            for &i in &visible {
                let mark = if selected[i] { "[x] " } else { "[ ] " };
                items.push(ListItem::new(format!("{mark}{}", models[i])));
            }
            let list = List::new(items)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("› ");
            f.render_stateful_widget(list, chunks[2], &mut state);
            f.render_widget(dim(&format!("↑/↓ move · Space toggles · Enter accepts · type to search · {}", nav_hint())), chunks[3]);
        })?;
        let cursor = state.selected().unwrap_or(0).min(row_count.saturating_sub(1));
        match read_key()? {
            Key::Back => return Ok(Nav::Back),
            Key::Exit => return Ok(Nav::Exit),
            Key::Other(KeyCode::Enter) => {
                if cursor == 0 {
                    return Ok(Nav::Next(None));
                }
                let chosen: Vec<String> = models.iter().enumerate().filter(|(i, _)| selected[*i]).map(|(_, m)| m.clone()).collect();
                // Enter on a row with nothing toggled means "just this one".
                if chosen.is_empty() {
                    if let Some(&idx) = visible.get(cursor - 1) {
                        return Ok(Nav::Next(Some(vec![models[idx].clone()])));
                    }
                    return Ok(Nav::Next(None));
                }
                return Ok(Nav::Next(Some(chosen)));
            }
            Key::Other(KeyCode::Char(' ')) => {
                if cursor > 0 {
                    if let Some(&idx) = visible.get(cursor - 1) {
                        selected[idx] = !selected[idx];
                    }
                }
            }
            Key::Other(KeyCode::Up) => state.select(Some(cursor.saturating_sub(1))),
            Key::Other(KeyCode::Down) => state.select(Some((cursor + 1).min(row_count.saturating_sub(1)))),
            Key::Other(KeyCode::Backspace) => {
                query.pop();
                state.select(Some(0));
            }
            Key::Other(KeyCode::Char(c)) => {
                query.push(c);
                state.select(Some(0));
            }
            Key::Other(_) => {}
        }
    }
}

/// Searchable ordered multi-select: Space toggles an item, and the order items
/// were toggled on is the order kept — each row shows its position (`[1]`,
/// `[2]`…) so the list reads as the target list it becomes. Enter accepts the
/// set (possibly empty — the caller decides what "none" means); `preselected`
/// restores the order already in place; the line under the list spells out
/// what Enter will accept.
pub fn pick_ordered(title: &str, note: &[&str], items: &[String], preselected: &[String]) -> Result<Nav<Vec<String>>> {
    let mut screen = Screen::open()?;
    let mut query = String::new();
    let mut selected: Vec<String> = preselected.iter().filter(|p| items.contains(p)).cloned().collect();
    let mut state = ListState::default();
    state.select(Some(0));
    let note_text = note.join("\n");
    loop {
        let visible: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, m)| query.is_empty() || m.to_lowercase().contains(&query.to_lowercase()))
            .map(|(i, _)| i)
            .collect();
        screen.terminal.draw(|f| {
            let width = f.area().width.max(1) as usize;
            let chunks = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(note_height(note, width)),
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(f.area());
            f.render_widget(Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)), chunks[0]);
            if !note.is_empty() {
                f.render_widget(Paragraph::new(note_text.as_str()).wrap(Wrap { trim: false }), chunks[1]);
            }
            f.render_widget(Paragraph::new(format!("Search: {query}▏")), chunks[2]);
            let rows: Vec<ListItem> = visible
                .iter()
                .map(|&i| {
                    let mark = match selected.iter().position(|s| *s == items[i]) {
                        Some(n) => format!("[{}] ", n + 1),
                        None => "[ ] ".to_string(),
                    };
                    ListItem::new(format!("{mark}{}", items[i]))
                })
                .collect();
            let list = List::new(rows)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("› ");
            f.render_stateful_widget(list, chunks[3], &mut state);
            let summary = if selected.is_empty() { "none".to_string() } else { selected.join(" › ") };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Enter sets targets: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(summary),
                ]))
                .wrap(Wrap { trim: false }),
                chunks[4],
            );
            f.render_widget(dim(&format!("↑/↓ move · Space toggles (in order) · Enter accepts · type to search · {}", nav_hint())), chunks[5]);
        })?;
        let cursor = state.selected().unwrap_or(0).min(visible.len().saturating_sub(1));
        match read_key()? {
            Key::Back => return Ok(Nav::Back),
            Key::Exit => return Ok(Nav::Exit),
            Key::Other(KeyCode::Enter) => return Ok(Nav::Next(selected)),
            Key::Other(KeyCode::Char(' ')) => {
                if let Some(&idx) = visible.get(cursor) {
                    match selected.iter().position(|s| *s == items[idx]) {
                        Some(n) => {
                            selected.remove(n);
                        }
                        None => selected.push(items[idx].clone()),
                    }
                }
            }
            Key::Other(KeyCode::Up) => state.select(Some(cursor.saturating_sub(1))),
            Key::Other(KeyCode::Down) => state.select(Some((cursor + 1).min(visible.len().saturating_sub(1)))),
            Key::Other(KeyCode::Backspace) => {
                query.pop();
                state.select(Some(0));
            }
            Key::Other(KeyCode::Char(c)) => {
                query.push(c);
                state.select(Some(0));
            }
            Key::Other(_) => {}
        }
    }
}

/// A yes/no screen with the lines that justify the question above it — what
/// `agent restore --full` and the drift check show before overwriting anything
/// the user may have edited by hand. `false` is the safe row, the default, and
/// what Esc means here.
pub fn confirm(title: &str, lines: &[&str], yes: &str) -> Result<bool> {
    let options = ["No — leave it as it is", yes];
    Ok(matches!(choose_at(title, lines, &options, None, 0)?, Nav::Next(1)))
}

/// The last screen before anything is written: the whole plan as lines, scrollable, with
/// Enter to go ahead and Esc to go back and change an answer (docs/cli.md § Navigation).
pub fn review(title: &str, lines: &[String], go: &str) -> Result<Nav<()>> {
    let mut screen = Screen::open()?;
    let mut scroll: u16 = 0;
    let text = lines.join("\n");
    loop {
        let mut max_scroll = 0u16;
        screen.terminal.draw(|f| {
            let chunks = Layout::vertical([
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(f.area());
            f.render_widget(Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)), chunks[0]);
            max_scroll = (lines.len() as u16).saturating_sub(chunks[1].height);
            f.render_widget(Paragraph::new(text.as_str()).scroll((scroll.min(max_scroll), 0)), chunks[1]);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Enter ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(go),
                ])),
                chunks[2],
            );
            let more = if max_scroll > 0 { "↑/↓ scroll · " } else { "" };
            f.render_widget(dim(&format!("{more}Enter goes ahead · {}", nav_hint())), chunks[3]);
        })?;
        match read_key()? {
            Key::Back => return Ok(Nav::Back),
            Key::Exit => return Ok(Nav::Exit),
            Key::Other(KeyCode::Enter) => return Ok(Nav::Next(())),
            Key::Other(KeyCode::Up) | Key::Other(KeyCode::Char('k')) => scroll = scroll.min(max_scroll).saturating_sub(1),
            Key::Other(KeyCode::Down) | Key::Other(KeyCode::Char('j')) => scroll = (scroll + 1).min(max_scroll),
            Key::Other(KeyCode::PageUp) => scroll = scroll.min(max_scroll).saturating_sub(10),
            Key::Other(KeyCode::PageDown) => scroll = (scroll + 10).min(max_scroll),
            Key::Other(_) => {}
        }
    }
}

/// The root page of an editing flow: every item with its value, edited items marked.
/// Enter opens an item, Tab saves the whole draft, Esc twice exits. `status` is the line
/// under the list that says whether the draft differs from what is in place.
pub fn root(title: &str, note: &[&str], rows: &[RootRow], cursor: usize, status: &str) -> Result<(RootAction, usize)> {
    let mut screen = Screen::open()?;
    let mut state = ListState::default();
    state.select(Some(cursor.min(rows.len().saturating_sub(1))));
    let note_text = note.join("\n");
    let label_width = rows.iter().map(|r| r.label.chars().count()).max().unwrap_or(0);
    loop {
        screen.terminal.draw(|f| {
            let width = f.area().width.max(1) as usize;
            let chunks = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(note_height(note, width)),
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(f.area());
            f.render_widget(Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)), chunks[0]);
            if !note.is_empty() {
                f.render_widget(Paragraph::new(note_text.as_str()).wrap(Wrap { trim: false }), chunks[1]);
            }
            let items: Vec<ListItem> = rows
                .iter()
                .map(|r| {
                    let mark = if r.changed { "* " } else { "  " };
                    let mut spans = vec![
                        Span::raw(mark),
                        Span::styled(format!("{:<label_width$}", r.label), Style::default().add_modifier(Modifier::BOLD)),
                        Span::raw("   "),
                        Span::raw(r.value.clone()),
                    ];
                    if r.changed {
                        spans.push(Span::styled("  (changed)", Style::default().add_modifier(Modifier::DIM)));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let list = List::new(items)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("› ");
            f.render_stateful_widget(list, chunks[2], &mut state);
            f.render_widget(Paragraph::new(status).style(Style::default().add_modifier(Modifier::BOLD)), chunks[3]);
            let nav = if ESC_ARMED.load(Ordering::SeqCst) {
                "Press Esc again to exit · any other key continues"
            } else {
                "Esc twice exits"
            };
            f.render_widget(dim(&format!("↑/↓ move · Enter edits · Tab saves all · {nav}")), chunks[4]);
        })?;
        let selected = state.selected().unwrap_or(0);
        match read_key()? {
            // The first Esc only arms the exit — there is no page above this one.
            Key::Back => {}
            Key::Exit => return Ok((RootAction::Exit, selected)),
            Key::Other(KeyCode::Tab) => return Ok((RootAction::Save, selected)),
            Key::Other(KeyCode::Enter) => return Ok((RootAction::Edit(selected), selected)),
            Key::Other(KeyCode::Up) | Key::Other(KeyCode::Char('k')) => state.select(Some(selected.saturating_sub(1))),
            Key::Other(KeyCode::Down) | Key::Other(KeyCode::Char('j')) => {
                state.select(Some((selected + 1).min(rows.len().saturating_sub(1))))
            }
            Key::Other(_) => {}
        }
    }
}

/// nano's question on closing a modified buffer: the unsaved changes, then
/// `Save changes?` with Y, N, or Esc to go back to editing.
pub fn save_prompt(title: &str, changes: &[String]) -> Result<SaveChoice> {
    let mut screen = Screen::open()?;
    let text = changes.join("\n");
    loop {
        screen.terminal.draw(|f| {
            let chunks = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(f.area());
            f.render_widget(Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM)), chunks[0]);
            f.render_widget(Paragraph::new("Unsaved changes:").style(Style::default().add_modifier(Modifier::BOLD)), chunks[1]);
            f.render_widget(Paragraph::new(text.as_str()).wrap(Wrap { trim: false }), chunks[2]);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Save changes? ", Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)),
                    Span::raw("  "),
                    Span::styled(" Y ", Style::default().add_modifier(Modifier::REVERSED)),
                    Span::raw(" Yes   "),
                    Span::styled(" N ", Style::default().add_modifier(Modifier::REVERSED)),
                    Span::raw(" No   "),
                    Span::styled(" Esc ", Style::default().add_modifier(Modifier::REVERSED)),
                    Span::raw(" Cancel"),
                ])),
                chunks[3],
            );
            f.render_widget(dim("Y saves and exits · N exits without saving · Esc goes back to editing"), chunks[4]);
        })?;
        match read_key()? {
            Key::Back | Key::Exit => {
                ESC_ARMED.store(false, Ordering::SeqCst);
                return Ok(SaveChoice::Cancel);
            }
            Key::Other(KeyCode::Char('y')) | Key::Other(KeyCode::Char('Y')) => return Ok(SaveChoice::Yes),
            Key::Other(KeyCode::Char('n')) | Key::Other(KeyCode::Char('N')) => return Ok(SaveChoice::No),
            Key::Other(_) => {}
        }
    }
}

/// The root-page loop's exit decision: nothing to save → leave; otherwise ask. `Ok(true)`
/// means save, `Ok(false)` means stay on the root page; "No" is the cancellation.
pub fn exit_decision(title: &str, changes: &[String]) -> Result<bool> {
    if changes.is_empty() {
        return Err(cancelled());
    }
    match save_prompt(title, changes)? {
        SaveChoice::Yes => Ok(true),
        SaveChoice::No => Err(cancelled()),
        SaveChoice::Cancel => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quitting is a typed outcome `main` recognises, not an error string to match on —
    /// and the default wording is the "nothing changed" one.
    #[test]
    fn cancelled_is_typed_and_says_nothing_changed() {
        let e = cancelled();
        assert!(e.is::<Cancelled>());
        assert_eq!(e.to_string(), "Cancelled — nothing was changed.");
        let partial: anyhow::Error = Cancelled(Some("Cancelled — Claude Code already restored.".into())).into();
        assert_eq!(partial.downcast_ref::<Cancelled>().unwrap().to_string(), "Cancelled — Claude Code already restored.");
        // Context added on the way up does not hide it.
        assert!(cancelled().context("while picking").is::<Cancelled>());
    }

    #[test]
    fn the_footer_says_when_the_next_esc_exits() {
        ESC_ARMED.store(false, Ordering::SeqCst);
        assert_eq!(nav_hint(), "Esc back · Esc twice exits");
        ESC_ARMED.store(true, Ordering::SeqCst);
        assert!(nav_hint().starts_with("Press Esc again to exit"));
        ESC_ARMED.store(false, Ordering::SeqCst);
    }

    /// Leaving a root page with nothing that saving would change never asks — it just
    /// exits, as nano closes an unmodified buffer without a question.
    #[test]
    fn exiting_without_changes_does_not_ask() {
        let e = exit_decision("t", &[]).unwrap_err();
        assert!(e.is::<Cancelled>());
        assert_eq!(e.to_string(), "Cancelled — nothing was changed.");
    }
}
