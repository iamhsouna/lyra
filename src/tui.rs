use crate::core::{self, Config, Field, Group, Kind, ADVANCED_FIELDS, COMPONENT_FIELDS, SONG_FIELDS};
use crate::detect::Paths;
use crate::runner::{Job, Status};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use std::collections::BTreeMap;

const ACCENT: Color = Color::Cyan;
const TITLE: Color = Color::Magenta;
const DIM: Color = Color::DarkGray;
const OK: Color = Color::Green;
const ERR: Color = Color::Red;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Song,
    Advanced,
    Components,
    Run,
    Presets,
    Help,
}

pub const TABS: &[(&str, Tab)] = &[
    ("1 Song", Tab::Song),
    ("2 Advanced", Tab::Advanced),
    ("3 Components", Tab::Components),
    ("4 Run", Tab::Run),
    ("5 Presets", Tab::Presets),
    ("6 Help", Tab::Help),
];

impl Tab {
    fn fields(self) -> &'static [Field] {
        match self {
            Tab::Song => SONG_FIELDS,
            Tab::Advanced => ADVANCED_FIELDS,
            Tab::Components => COMPONENT_FIELDS,
            _ => &[],
        }
    }
}

pub struct TextInput {
    buf: Vec<char>,
    cursor: usize,
}

impl TextInput {
    pub fn new(s: &str) -> Self {
        let buf: Vec<char> = s.chars().collect();
        let cursor = buf.len();
        TextInput { buf, cursor }
    }
    pub fn as_string(&self) -> String {
        self.buf.iter().collect()
    }
    pub fn insert(&mut self, c: char) {
        self.buf.insert(self.cursor, c);
        self.cursor += 1;
    }
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buf.remove(self.cursor);
        }
    }
    pub fn delete(&mut self) {
        if self.cursor < self.buf.len() {
            self.buf.remove(self.cursor);
        }
    }
    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    pub fn right(&mut self) {
        if self.cursor < self.buf.len() {
            self.cursor += 1;
        }
    }
    pub fn home(&mut self) {
        self.cursor = 0;
    }
    pub fn end(&mut self) {
        self.cursor = self.buf.len();
    }
    pub fn line_col(&self) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        for &c in &self.buf[..self.cursor.min(self.buf.len())] {
            if c == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

enum EditTarget {
    Field(Field),
    PresetName,
}

struct Editor {
    target: EditTarget,
    input: TextInput,
    multiline: bool,
}

pub struct TuiApp {
    pub paths: Paths,
    pub cfg: Config,
    pub tab: Tab,
    pub selected: usize,
    edit: Option<Editor>,
    pub presets: BTreeMap<String, Config>,
    pub preset_names: Vec<String>,
    pub preset_idx: usize,
    pub job: Option<Job>,
    pub log: Vec<String>,
    pub history: Vec<core::HistoryEntry>,
    pub should_quit: bool,
    pub status_msg: Option<String>,
    spinner: u64,
    log_offset: usize,
    finished: bool,
}

impl TuiApp {
    pub fn new(paths: Paths) -> Self {
        let presets = core::load_presets();
        let history = core::load_history();
        let mut app = TuiApp {
            paths,
            cfg: Config::default(),
            tab: Tab::Song,
            selected: 0,
            edit: None,
            preset_names: presets.keys().cloned().collect(),
            presets,
            preset_idx: 0,
            job: None,
            log: Vec::new(),
            history,
            should_quit: false,
            status_msg: None,
            spinner: 0,
            log_offset: 0,
            finished: false,
        };
        app.apply_default_components();
        app
    }

    /// Pre-fill component selections from what is on disk (so the UI shows
    /// the real filenames rather than "(auto)").
    fn apply_default_components(&mut self) {
        if let Some(v) = &self.paths.language_model {
            self.cfg.lm_gguf = v.clone();
        }
        if let Some(v) = &self.paths.flow_transformer {
            self.cfg.flow_gguf = v.clone();
        }
        if let Some(v) = &self.paths.depth_decoder {
            self.cfg.depth_gguf = v.clone();
        }
    }

    fn fields(&self) -> &'static [Field] {
        self.tab.fields()
    }

    fn moving(&self) -> bool {
        self.job.as_ref().map(|j| j.running()).unwrap_or(false)
    }

    // --- events -------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.edit.is_some() {
            self.editor_key(key);
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if ctrl && key.code == KeyCode::Char('q') {
            if self.moving() {
                if let Some(j) = &self.job {
                    j.abort();
                }
            }
            self.should_quit = true;
            return;
        }
        if ctrl && key.code == KeyCode::Char('g') {
            self.generate();
            return;
        }

        match key.code {
            KeyCode::Tab => self.switch_tab(1),
            KeyCode::BackTab => self.switch_tab(-1),
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let i = c as usize - '1' as usize;
                if i < TABS.len() {
                    self.tab = TABS[i].1;
                    self.selected = 0;
                }
            }
            _ => match self.tab {
                Tab::Song | Tab::Advanced | Tab::Components => self.field_key(key),
                Tab::Run => self.run_key(key),
                Tab::Presets => self.presets_key(key),
                Tab::Help => {}
            },
        }
    }

    fn switch_tab(&mut self, dir: i64) {
        let i = TABS.iter().position(|(_, t)| *t == self.tab).unwrap_or(0) as i64;
        let n = TABS.len() as i64;
        self.tab = TABS[((i + dir).rem_euclid(n)) as usize].1;
        self.selected = 0;
    }

    fn field_key(&mut self, key: KeyEvent) {
        let fields = self.fields();
        if fields.is_empty() {
            return;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(fields.len() - 1);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                let f = fields[self.selected];
                core::adjust(&mut self.cfg, &self.paths, f, -1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                let f = fields[self.selected];
                core::adjust(&mut self.cfg, &self.paths, f, 1);
            }
            KeyCode::Char(' ') => {
                let f = fields[self.selected];
                match f.kind() {
                    Kind::Bool => core::toggle(&mut self.cfg, f),
                    _ => core::adjust(&mut self.cfg, &self.paths, f, 1),
                }
            }
            KeyCode::Enter => {
                let f = fields[self.selected];
                match f.kind() {
                    Kind::Bool => core::toggle(&mut self.cfg, f),
                    Kind::Select => {
                        if f == Field::Genre && self.cfg.is_custom_genre() {
                            self.open_editor(f);
                        } else {
                            core::adjust(&mut self.cfg, &self.paths, f, 1);
                        }
                    }
                    _ => self.open_editor(f),
                }
            }
            KeyCode::Char('g') => self.generate(),
            KeyCode::Char('r') => {
                let out = self.cfg.out.clone();
                self.cfg = Config { out, ..Config::default() };
                self.apply_default_components();
            }
            KeyCode::Esc => self.tab = Tab::Song,
            _ => {}
        }
    }

    fn run_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter | KeyCode::Char('g') => self.generate(),
            KeyCode::Char('a') | KeyCode::Char('x') => {
                if let Some(j) = &self.job {
                    j.abort();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.log_offset = self.log_offset.saturating_add(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.log_offset = self.log_offset.saturating_sub(1);
            }
            KeyCode::End => self.log_offset = 0,
            KeyCode::Char('n') => {
                self.cfg.out = core::default_out();
                self.finished = false;
            }
            KeyCode::Esc => self.tab = Tab::Song,
            _ => {}
        }
    }

    fn presets_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.preset_idx = self.preset_idx.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.preset_names.is_empty() {
                    self.preset_idx = (self.preset_idx + 1).min(self.preset_names.len() - 1);
                }
            }
            KeyCode::Enter => {
                if let Some(name) = self.preset_names.get(self.preset_idx).cloned() {
                    if let Some(cfg) = self.presets.get(&name) {
                        self.cfg = cfg.clone();
                        self.status_msg = Some(format!("loaded preset '{name}'"));
                    }
                }
            }
            KeyCode::Char('s') => {
                self.edit = Some(Editor {
                    target: EditTarget::PresetName,
                    input: TextInput::new(""),
                    multiline: false,
                });
            }
            KeyCode::Char('d') => {
                if let Some(name) = self.preset_names.get(self.preset_idx).cloned() {
                    self.presets.remove(&name);
                    core::save_presets(&self.presets);
                    self.reload_presets();
                    self.status_msg = Some(format!("deleted preset '{name}'"));
                }
            }
            KeyCode::Char('r') => {
                self.cfg = Config::default();
                self.apply_default_components();
                self.status_msg = Some("reset to defaults".into());
            }
            KeyCode::Esc => self.tab = Tab::Song,
            _ => {}
        }
    }

    fn reload_presets(&mut self) {
        self.preset_names = self.presets.keys().cloned().collect();
        if self.preset_idx >= self.preset_names.len() {
            self.preset_idx = self.preset_names.len().saturating_sub(1);
        }
    }

    fn open_editor(&mut self, f: Field) {
        let cur = match f {
            Field::Genre => self.cfg.genre_custom.clone(),
            _ => core::value_string(&self.cfg, f),
        };
        self.edit = Some(Editor {
            target: EditTarget::Field(f),
            input: TextInput::new(&cur),
            multiline: f == Field::Lyrics,
        });
    }

    fn editor_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let multiline = self.edit.as_ref().map(|e| e.multiline).unwrap_or(false);
        match key.code {
            KeyCode::Esc => self.edit = None,
            KeyCode::Enter if !multiline => self.commit_editor(),
            KeyCode::Enter if ctrl => self.commit_editor(),
            KeyCode::Tab if multiline => self.commit_editor(),
            KeyCode::Enter => {
                if let Some(e) = self.edit.as_mut() {
                    e.input.insert('\n');
                }
            }
            _ => {
                if let Some(e) = self.edit.as_mut() {
                    edit_single(&mut e.input, key);
                }
            }
        }
    }

    fn commit_editor(&mut self) {
        if let Some(e) = self.edit.take() {
            let s = e.input.as_string();
            match e.target {
                EditTarget::Field(f) => core::set_text(&mut self.cfg, f, &s),
                EditTarget::PresetName => {
                    let name = s.trim().to_string();
                    if !name.is_empty() {
                        self.presets.insert(name.clone(), self.cfg.clone());
                        core::save_presets(&self.presets);
                        self.reload_presets();
                        self.status_msg = Some(format!("saved preset '{name}'"));
                    }
                }
            }
        }
    }

    pub fn generate(&mut self) {
        if self.moving() {
            self.status_msg = Some("already generating — press a to abort".into());
            return;
        }
        match Job::start(&self.paths, &self.cfg) {
            Ok(j) => {
                self.job = Some(j);
                self.log.clear();
                self.log_offset = 0;
                self.finished = false;
                self.status_msg = None;
                self.tab = Tab::Run;
            }
            Err(e) => self.status_msg = Some(format!("launch failed: {e}")),
        }
    }

    pub fn on_tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
        if let Some(j) = &self.job {
            self.log = j.lines();
            let st = j.status();
            if !st.is_running() && !self.finished {
                self.finished = true;
                let ok = st == Status::Success;
                j.record(ok);
                self.history = core::load_history();
                self.status_msg = Some(st.label());
            }
        }
    }

    // --- rendering ----------------------------------------------------------

    pub fn draw(&mut self, f: &mut Frame) {
        let layout = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(2),
        ])
        .split(f.area());
        self.draw_tabs(f, layout[0]);
        match self.tab {
            Tab::Song | Tab::Advanced | Tab::Components => self.draw_form(f, layout[1]),
            Tab::Run => self.draw_run(f, layout[1]),
            Tab::Presets => self.draw_presets(f, layout[1]),
            Tab::Help => self.draw_help(f, layout[1]),
        }
        self.draw_footer(f, layout[2]);
        if self.edit.is_some() {
            self.draw_editor(f);
        }
    }

    fn draw_tabs(&self, f: &mut Frame, area: Rect) {
        let mut spans = vec![Span::styled(" ✦ Lyra ", Style::default().fg(TITLE).add_modifier(Modifier::BOLD))];
        for (name, tab) in TABS {
            let active = *tab == self.tab;
            let style = if active {
                Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(format!(" {name} "), style));
        }
        spans.push(Span::styled(
            format!("   {}", self.cfg.caption().chars().take(60).collect::<String>()),
            Style::default().fg(DIM),
        ));
        let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(TITLE));
        f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
    }

    fn draw_form(&self, f: &mut Frame, area: Rect) {
        let cols = Layout::horizontal([Constraint::Length(42), Constraint::Min(24)]).split(area);
        let fields = self.fields();
        let mut lines: Vec<Line> = Vec::new();
        let mut last_group: Option<Group> = None;
        for (i, field) in fields.iter().enumerate() {
            if Some(field.group()) != last_group {
                last_group = Some(field.group());
                lines.push(Line::from(Span::styled(
                    format!("  {} ", group_name(field.group())),
                    Style::default().fg(TITLE).add_modifier(Modifier::BOLD),
                )));
            }
            let selected = i == self.selected;
            let marker = if selected { "▶ " } else { "  " };
            let label = format!("{:<24}", field.label());
            let val_style = match field.kind() {
                Kind::Bool => Style::default().fg(if core::value_string(&self.cfg, *field) == "true" { OK } else { DIM }),
                Kind::Select => Style::default().fg(ACCENT),
                _ => Style::default().fg(Color::White),
            };
            let name_style = if selected {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(ACCENT)),
                Span::styled(label, name_style),
                Span::styled(truncate(&core::value_string(&self.cfg, *field), 40), val_style),
            ]));
        }
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL).title(" settings "))
                .wrap(Wrap { trim: false }),
            cols[0],
        );
        self.draw_detail(f, cols[1]);
    }

    fn draw_detail(&self, f: &mut Frame, area: Rect) {
        let rows = Layout::vertical([Constraint::Length(4), Constraint::Min(4)]).split(area);
        let field = self.fields().get(self.selected).copied();
        let (title, help) = match field {
            Some(field) => (format!(" {} ", field.label()), field.help().to_string()),
            None => ("".to_string(), String::new()),
        };
        let tip = vec![
            Line::from(Span::styled(format!(" {help}"), Style::default().fg(DIM))),
            Line::from(Span::styled(
                " ←/→ adjust · Enter edit · space toggle/cycle · g generate",
                Style::default().fg(DIM),
            )),
        ];
        f.render_widget(
            Paragraph::new(Text::from(tip))
                .block(Block::default().borders(Borders::ALL).title(title))
                .wrap(Wrap { trim: false }),
            rows[0],
        );
        self.draw_preview(f, rows[1]);
    }

    fn draw_preview(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(" Caption", Style::default().fg(ACCENT).bold())));
        for l in wrap(&self.cfg.caption(), area.width.saturating_sub(3) as usize) {
            lines.push(Line::raw(format!("  {l}")));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(" Lyrics", Style::default().fg(ACCENT).bold())));
        for l in self.cfg.lyrics_text().lines().take(12) {
            lines.push(Line::raw(format!("  {l}")));
        }
        let warns = self.cfg.warnings(&self.paths);
        if !warns.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(" Warnings", Style::default().fg(ERR).bold())));
            for w in warns {
                lines.push(Line::from(Span::styled(format!("  • {w}"), Style::default().fg(ERR))));
            }
        }
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL).title(" preview "))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn draw_run(&self, f: &mut Frame, area: Rect) {
        let rows = Layout::vertical([
            Constraint::Length(6),
            Constraint::Min(4),
            Constraint::Length(5),
        ])
        .split(area);

        let warnings = self.cfg.warnings(&self.paths);
        let mut head: Vec<Line> = Vec::new();
        head.push(Line::from(Span::styled(" command", Style::default().fg(ACCENT).bold())));
        head.push(Line::raw("  ".to_string() + &truncate(&core::shell_join(&core::build_argv(&self.paths, &self.cfg)), 2000)));
        if !warnings.is_empty() {
            for w in warnings {
                head.push(Line::from(Span::styled(format!("  ⚠ {w}"), Style::default().fg(ERR))));
            }
        }
        f.render_widget(
            Paragraph::new(Text::from(head))
                .block(Block::default().borders(Borders::ALL).title(" ready "))
                .wrap(Wrap { trim: true }),
            rows[0],
        );

        // log
        let visible = rows[1].height.saturating_sub(2) as usize;
        let end = self.log.len().saturating_sub(self.log_offset);
        let start = end.saturating_sub(visible);
        let log_lines: Vec<Line> = self.log[start..end]
            .iter()
            .map(|l| Line::from(Span::styled(l.clone(), Style::default().fg(DIM))))
            .collect();
        let title = self
            .status_msg
            .as_ref()
            .map(|s| format!(" log · {s} "))
            .unwrap_or_else(|| " log ".into());
        f.render_widget(
            Paragraph::new(Text::from(log_lines))
                .block(Block::default().borders(Borders::ALL).title(title))
                .wrap(Wrap { trim: false }),
            rows[1],
        );

        // status / history
        let mut status: Vec<Line> = Vec::new();
        match &self.job {
            Some(j) if j.running() => {
                let spin = SPINNER[(self.spinner as usize / 3) % SPINNER.len()];
                status.push(Line::from(vec![
                    Span::styled(format!(" {spin} "), Style::default().fg(TITLE)),
                    Span::styled("composing…", Style::default().fg(Color::White).bold()),
                    Span::styled(format!("  {:.0}s", j.elapsed_secs()), Style::default().fg(ACCENT)),
                    Span::styled("   a abort", Style::default().fg(DIM)),
                ]));
            }
            Some(j) if self.finished => {
                let st = j.status();
                let (c, m) = match st {
                    Status::Success => (OK, format!("✓ saved → {}", self.cfg.out_path())),
                    Status::Aborted => (ERR, "aborted".into()),
                    _ => (ERR, format!("✗ {}", st.label())),
                };
                status.push(Line::from(Span::styled(format!(" {m}"), Style::default().fg(c).bold())));
                status.push(Line::from(Span::styled(
                    format!(" wall {:.1}s · n new path · Enter/g regenerate", j.wall_ms() / 1000.0),
                    Style::default().fg(DIM),
                )));
            }
            _ => {
                status.push(Line::from(Span::styled(
                    " idle — press Enter or g to compose",
                    Style::default().fg(DIM),
                )));
            }
        }
        if let Some(h) = self.history.last() {
            status.push(Line::from(Span::styled(
                format!(" last: {} ({:.1}s)", truncate(&h.out, 60), h.wall_ms / 1000.0),
                Style::default().fg(DIM),
            )));
        }
        f.render_widget(
            Paragraph::new(Text::from(status)).block(Block::default().borders(Borders::ALL).title(" status ")),
            rows[2],
        );
    }

    fn draw_presets(&self, f: &mut Frame, area: Rect) {
        let cols = Layout::horizontal([Constraint::Length(38), Constraint::Min(24)]).split(area);
        let items: Vec<Line> = if self.preset_names.is_empty() {
            vec![Line::from(Span::styled("  (no presets yet)", Style::default().fg(DIM)))]
        } else {
            self.preset_names
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    if i == self.preset_idx {
                        Line::from(Span::styled(
                            format!(" ▶ {n}"),
                            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                        ))
                    } else {
                        Line::from(Span::styled(format!("   {n}"), Style::default().fg(DIM)))
                    }
                })
                .collect()
        };
        f.render_widget(
            Paragraph::new(Text::from(items))
                .block(Block::default().borders(Borders::ALL).title(" presets "))
                .wrap(Wrap { trim: false }),
            cols[0],
        );

        let mut detail: Vec<Line> = Vec::new();
        if let Some(name) = self.preset_names.get(self.preset_idx) {
            if let Some(c) = self.presets.get(name) {
                detail.push(Line::from(Span::styled(format!(" {name}"), Style::default().fg(ACCENT).bold())));
                detail.push(Line::raw(""));
                for field in core::ALL_FIELDS {
                    detail.push(Line::from(vec![
                        Span::styled(format!(" {:<24}", field.label()), Style::default().fg(DIM)),
                        Span::styled(truncate(&core::value_string(c, *field), 40), Style::default().fg(Color::White)),
                    ]));
                }
            }
        } else {
            detail.push(Line::from(Span::styled("  Enter load · s save current as · d delete · r reset", Style::default().fg(DIM))));
        }
        f.render_widget(
            Paragraph::new(Text::from(detail))
                .block(Block::default().borders(Borders::ALL).title(" preset details "))
                .wrap(Wrap { trim: false }),
            cols[1],
        );
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        let head = |t: &str| Line::from(Span::styled(format!(" {t}"), Style::default().fg(TITLE).bold()));
        let body = |t: &str| Line::from(Span::styled(format!("   {t}"), Style::default().fg(DIM)));
        lines.push(head("Navigation"));
        lines.push(body("Tab / Shift-Tab or 1-6 switch tabs · ↑/↓ move · ←/→ adjust values"));
        lines.push(body("Enter edit text / toggle / cycle · Space toggle/cycle · Esc back to Song"));
        lines.push(body("g or Ctrl-G generate · a abort · Ctrl-Q quit"));
        lines.push(Line::raw(""));
        lines.push(head("The workflow"));
        lines.push(body("1 Song: style, mood, vocals, caption override, lyrics, length, steps, output"));
        lines.push(body("2 Advanced: CFG scales, top-k, seed, flow/perf tuning, memory options"));
        lines.push(body("3 Components: backend + which LM / flow / depth GGUF to load"));
        lines.push(body("4 Run: preview + warnings, live log, status; regenerate or reroll seed"));
        lines.push(body("5 Presets: save/load named configs (stored in ~/.config/lyra/presets.json)"));
        lines.push(Line::raw(""));
        lines.push(head("Option notes"));
        for field in core::ALL_FIELDS {
            lines.push(Line::from(vec![
                Span::styled(format!("   {:<24}", field.label()), Style::default().fg(ACCENT)),
                Span::styled(field.help().to_string(), Style::default().fg(DIM)),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(head("Model"));
        for w in self.cfg.warnings(&self.paths) {
            lines.push(Line::from(Span::styled(format!("   ⚠ {w}"), Style::default().fg(ERR))));
        }
        if self.cfg.warnings(&self.paths).is_empty() {
            lines.push(Line::from(Span::styled("   ✓ setup looks good", Style::default().fg(OK))));
        }
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL).title(" help "))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let line = match &self.status_msg {
            Some(m) => Line::from(Span::styled(format!(" {m}"), Style::default().fg(ACCENT))),
            None => Line::from(Span::styled(
                " Tab switch · ↑/↓ field · ←/→ adjust · Enter edit · g generate · ? help (6) · Ctrl-Q quit",
                Style::default().fg(DIM),
            )),
        };
        f.render_widget(
            Paragraph::new(line).block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(DIM))),
            area,
        );
    }

    fn draw_editor(&self, f: &mut Frame) {
        let Some(ed) = &self.edit else { return };
        let area = centered(f.area(), 70, if ed.multiline { 60 } else { 20 });
        f.render_widget(Clear, area);
        let (title, hint) = match ed.target {
            EditTarget::Field(field) => (
                format!(" {} ", field.label()),
                if ed.multiline {
                    "Enter = new line · Ctrl-Enter or Tab = save · Esc = cancel"
                } else {
                    "Enter = save · Esc = cancel"
                },
            ),
            EditTarget::PresetName => (" save preset as ".to_string(), "Enter = save · Esc = cancel"),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(TITLE))
            .title(title);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
        f.render_widget(Paragraph::new(Text::from(ed.input.as_string())), rows[0]);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {hint}"), Style::default().fg(DIM))))
                .alignment(Alignment::Left),
            rows[1],
        );
        let (line, col) = ed.input.line_col();
        f.set_cursor_position((rows[0].x + col as u16, rows[0].y + line as u16));
    }
}

fn edit_single(t: &mut TextInput, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return;
    }
    match key.code {
        KeyCode::Char(c) => t.insert(c),
        KeyCode::Backspace => t.backspace(),
        KeyCode::Delete => t.delete(),
        KeyCode::Left => t.left(),
        KeyCode::Right => t.right(),
        KeyCode::Home => t.home(),
        KeyCode::End => t.end(),
        _ => {}
    }
}

fn group_name(g: Group) -> &'static str {
    match g {
        Group::Song => "Song",
        Group::Advanced => "Advanced",
        Group::Components => "Components",
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', "⏎");
    if s.chars().count() <= max {
        s
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if line.is_empty() {
                line = word.to_string();
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        out.push(line);
    }
    out
}

fn centered(area: Rect, pw: u16, ph: u16) -> Rect {
    let w = pw.min(area.width.saturating_sub(2)).max(20);
    let h = ph.min(area.height.saturating_sub(2)).max(5);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}
