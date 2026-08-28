// Suppresses the console window that would otherwise briefly flash/stay
// open alongside the GUI window when launched on Windows (e.g. by
// double-clicking the .exe or a Start Menu shortcut). No effect on other
// platforms.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod engine;
mod human_solver;
mod replay;

use eframe::egui::{self, Color32, Key, Pos2, Rect, Rounding, Stroke, Vec2};
use engine::{Board, Difficulty, CELLS, COLS, ROWS};
use replay::GameSessionReport;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Screen {
    MainMenu,
    Options,
    Playing,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum NoteMode {
    Normal,
    Corner,
    Center,
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum AppTheme {
    Dark,
    Light,
}

/// Every toggle in the app lives here and is persisted to disk, so the
/// Options screen is really just a UI over this struct.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: AppTheme,
    pub sound_enabled: bool,
    /// When you place a digit, also clear that digit from pencil marks in
    /// every peer cell (same row, column, AND box - not just the box).
    pub smart_notes: bool,
    pub highlight_same_number: bool,
    pub highlight_peers: bool,
    pub show_mistakes: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: AppTheme::Dark,
            sound_enabled: true,
            smart_notes: true,
            highlight_same_number: true,
            highlight_peers: true,
            show_mistakes: true,
        }
    }
}

/// Directory the running executable lives in - NOT the current working
/// directory. Used only for *reading* resources bundled alongside the app
/// (the `sounds/` folder), since those ship read-only with the executable.
fn app_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Per-user, writable data directory: `~/Library/Application Support/SudokuStudioPro`
/// on macOS, `%APPDATA%\SudokuStudioPro` on Windows, `~/.local/share/SudokuStudioPro`
/// (or `$XDG_DATA_HOME`) on Linux. Settings and exported replays live here
/// rather than next to the executable, because once the app is actually
/// installed (`/Applications`, `Program Files`, `/usr/bin`, ...) that
/// location usually isn't writable by a normal user - only `app_dir()`
/// (read-only bundled resources) is safe to assume exists next to the exe.
fn user_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("SudokuStudioPro")
}

fn settings_path() -> PathBuf {
    user_data_dir().join("settings.json")
}

fn load_settings() -> Settings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &Settings) {
    let _ = std::fs::create_dir_all(user_data_dir());
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(settings_path(), json);
    }
}

pub struct CellState {
    pub given: bool,
    pub value: u8,
    pub corner_notes: u16,
    pub center_notes: u16,
}

/// Snapshot of one cell, used to build multi-cell undo entries. A placement
/// can now change more than just the target cell (see "smart notes" below),
/// so an undo entry has to hold a snapshot of every cell it touched, not
/// just one.
#[derive(Clone, Copy)]
struct CellSnapshot {
    idx: usize,
    value: u8,
    corner_notes: u16,
    center_notes: u16,
}

struct UndoEntry {
    cells: Vec<CellSnapshot>,
    mistakes_count: usize,
    /// Step number (1-based) this entry precedes - reported back if this
    /// entry is undone, so the exported log shows which step got reverted.
    step: usize,
}

/// Unique peers of `idx`: same row, same column, same box (up to 20 cells).
fn peers_of_cell(idx: usize) -> Vec<usize> {
    let (r, c) = (idx / 9, idx % 9);
    let box_r = (r / 3) * 3;
    let box_c = (c / 3) * 3;
    let mut seen = [false; CELLS];
    let mut out = Vec::with_capacity(20);
    for i in 0..9 {
        let row_i = r * 9 + i;
        let col_i = i * 9 + c;
        if row_i != idx && !seen[row_i] {
            seen[row_i] = true;
            out.push(row_i);
        }
        if col_i != idx && !seen[col_i] {
            seen[col_i] = true;
            out.push(col_i);
        }
    }
    for br in 0..3 {
        for bc in 0..3 {
            let i = (box_r + br) * 9 + (box_c + bc);
            if i != idx && !seen[i] {
                seen[i] = true;
                out.push(i);
            }
        }
    }
    out
}

/// Loads `sounds/<name>.<ext>` trying a few common audio extensions, so it
/// doesn't matter whether the sound pack you grabbed ships .wav, .ogg, or
/// .mp3 - rodio's decoder auto-detects the actual format from the file's
/// contents regardless of extension, so any of these work.
fn load_sound_file(name: &str) -> Option<Vec<u8>> {
    // 1. Installed/bundled app: sounds/ next to the executable
    // 2. Development: sounds/ in the project root (where cargo run is invoked)
    // 3. Debug builds: compile-time manifest dir as a last resort
    let candidates = {
        let mut v = vec![app_dir().join("sounds"), PathBuf::from("sounds")];
        #[cfg(debug_assertions)]
        v.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sounds"));
        v
    };

    for dir in &candidates {
        for ext in ["wav", "ogg", "mp3", "flac"] {
            let path = dir.join(format!("{name}.{ext}"));
            if let Ok(bytes) = std::fs::read(&path) {
                return Some(bytes);
            }
        }
    }
    None
}

/// Loads a handful of short sound effects once at startup and plays them on
/// demand. Every failure mode here (no audio device, missing sound file,
/// unparseable audio) is handled by simply not playing that sound - a
/// missing `sounds/` folder should never crash the game or block play.
///
/// Current rodio (0.22) API: `DeviceSinkBuilder::open_default_sink()` opens
/// the device once, returning a handle that must be kept alive for the
/// app's lifetime (dropping it stops all playback). `handle.mixer().add(source)`
/// plays a source immediately, mixed with anything else already playing -
/// exactly what one-shot sound effects need, no `Sink`/`Player` required.
/// NOTE: this API is gated behind rodio's optional `"playback"` feature -
/// see the Cargo.toml dependency line.
pub struct SoundEngine {
    handle: Option<rodio::MixerDeviceSink>,
    click: Option<Vec<u8>>,
    error: Option<Vec<u8>>,
    success: Option<Vec<u8>>,
    ui_tick: Option<Vec<u8>>,
}

impl SoundEngine {
    fn new() -> Self {
        let mut handle = rodio::DeviceSinkBuilder::open_default_sink().ok();
        if let Some(ref mut h) = &mut handle {
            h.log_on_drop(false);
        }
        Self {
            handle,
            click: load_sound_file("click"),
            error: load_sound_file("error"),
            success: load_sound_file("success"),
            ui_tick: load_sound_file("ui_tick"),
        }
    }

    fn play(&self, bytes: &Option<Vec<u8>>) {
        let (Some(handle), Some(bytes)) = (&self.handle, bytes) else {
            return;
        };
        if let Ok(source) = rodio::Decoder::new(std::io::Cursor::new(bytes.clone())) {
            handle.mixer().add(source);
        }
    }

    pub fn play_click(&self) {
        self.play(&self.click)
    }
    pub fn play_error(&self) {
        self.play(&self.error)
    }
    pub fn play_success(&self) {
        self.play(&self.success)
    }
    pub fn play_ui(&self) {
        self.play(&self.ui_tick)
    }
}

struct ThemeColors {
    bg: Color32,
    panel: Color32,
    cell_base: Color32,
    cell_seen: Color32,
    cell_selected: Color32,
    highlight_match: Color32,
    text_given: Color32,
    text_user: Color32,
    grid_main: Color32,
    grid_sub: Color32,
    accent: Color32,
}

pub struct SudokuApp {
    pub screen: Screen,
    pub options_return_to: Screen,
    pub settings: Settings,
    pub theme: AppTheme,
    pub difficulty: Difficulty,
    pub note_mode: NoteMode,
    pub selected_cell: Option<usize>,
    pub initial_board: Board,
    pub solution_board: Board,
    pub grid: Vec<CellState>,
    pub start_time: Instant,
    pub started_at_unix_ms: u128,
    pub moves_log: Vec<replay::MoveRecord>,
    pub mistakes_count: usize,
    pub solved: bool,
    undo_stack: Vec<UndoEntry>,
    pub generating: bool,
    gen_rx: Option<Receiver<(Board, Board)>>,
    export_status: Option<(String, Instant)>,
    sound: SoundEngine,
}

impl SudokuApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let settings = load_settings();
        Self {
            screen: Screen::MainMenu,
            options_return_to: Screen::MainMenu,
            theme: settings.theme,
            settings,
            difficulty: Difficulty::Extreme,
            note_mode: NoteMode::Normal,
            selected_cell: None,
            initial_board: Board::new(),
            solution_board: Board::new(),
            grid: Vec::new(),
            start_time: Instant::now(),
            started_at_unix_ms: unix_millis_now(),
            moves_log: Vec::new(),
            mistakes_count: 0,
            solved: false,
            undo_stack: Vec::new(),
            generating: false,
            gen_rx: None,
            export_status: None,
            sound: SoundEngine::new(),
        }
    }

    fn theme_colors(&self) -> ThemeColors {
        match self.theme {
            AppTheme::Dark => ThemeColors {
                bg: Color32::from_rgb(15, 17, 23),
                panel: Color32::from_rgb(23, 26, 35),
                cell_base: Color32::from_rgb(32, 36, 48),
                cell_seen: Color32::from_rgb(42, 48, 64),
                cell_selected: Color32::from_rgb(68, 92, 138),
                highlight_match: Color32::from_rgb(76, 92, 54),
                text_given: Color32::from_rgb(240, 244, 255),
                text_user: Color32::from_rgb(90, 170, 255),
                grid_main: Color32::from_rgb(160, 175, 205),
                grid_sub: Color32::from_rgb(50, 56, 74),
                accent: Color32::from_rgb(45, 100, 210),
            },
            AppTheme::Light => ThemeColors {
                bg: Color32::from_rgb(242, 245, 250),
                panel: Color32::from_rgb(255, 255, 255),
                cell_base: Color32::from_rgb(255, 255, 255),
                cell_seen: Color32::from_rgb(230, 238, 250),
                cell_selected: Color32::from_rgb(188, 218, 255),
                highlight_match: Color32::from_rgb(222, 240, 185),
                text_given: Color32::from_rgb(20, 24, 36),
                text_user: Color32::from_rgb(0, 102, 220),
                grid_main: Color32::from_rgb(40, 45, 60),
                grid_sub: Color32::from_rgb(215, 222, 235),
                accent: Color32::from_rgb(0, 105, 230),
            },
        }
    }

    /// Kicks off puzzle generation on a background thread instead of
    /// blocking the UI thread. Harder tiers can take a noticeable moment to
    /// dig down to (and rate-check against) their target.
    pub fn start_new_game(&mut self, diff: Difficulty) {
        self.difficulty = diff;
        self.generating = true;
        self.selected_cell = None;

        let (tx, rx) = channel();
        self.gen_rx = Some(rx);

        thread::spawn(move || {
            let generated = Board::generate(diff);
            let _ = tx.send(generated);
        });
    }

    fn apply_generated(&mut self, puzzle: Board, solution: Board) {
        self.initial_board = puzzle.clone();
        self.solution_board = solution;
        self.grid = (0..CELLS)
            .map(|i| CellState {
                given: puzzle.cells[i] != 0,
                value: puzzle.cells[i],
                corner_notes: 0,
                center_notes: 0,
            })
            .collect();
        self.moves_log.clear();
        self.undo_stack.clear();
        self.mistakes_count = 0;
        self.solved = false;
        self.selected_cell = Some(0);
        self.start_time = Instant::now();
        self.started_at_unix_ms = unix_millis_now();
        self.export_status = None;
    }

    fn record_action(&mut self, idx: usize, code: &str, digit: u8, extra: usize) {
        self.moves_log.push((
            idx,
            code.to_string(),
            digit,
            self.start_time.elapsed().as_millis(),
            extra,
        ));
    }

    fn begin_undo(&self, indices: &[usize]) -> UndoEntry {
        UndoEntry {
            cells: indices
                .iter()
                .map(|&i| CellSnapshot {
                    idx: i,
                    value: self.grid[i].value,
                    corner_notes: self.grid[i].corner_notes,
                    center_notes: self.grid[i].center_notes,
                })
                .collect(),
            mistakes_count: self.mistakes_count,
            step: self.moves_log.len() + 1,
        }
    }

    pub fn handle_number_input(&mut self, num: u8) {
        let Some(idx) = self.selected_cell else {
            return;
        };
        if self.grid[idx].given {
            return;
        }

        match self.note_mode {
            NoteMode::Normal => {
                if self.grid[idx].value == num {
                    let undo_entry = self.begin_undo(&[idx]);
                    self.grid[idx].value = 0;
                    self.record_action(idx, "C", num, 0);
                    self.undo_stack.push(undo_entry);
                } else {
                    let peer_ids = peers_of_cell(idx);
                    let bit: u16 = 1 << num;

                    // Smart notes: figure out which peer cells will actually
                    // lose a pencil mark for this digit *before* mutating
                    // anything, so the undo entry can restore them exactly.
                    let mut affected = vec![idx];
                    if self.settings.smart_notes {
                        for &p in &peer_ids {
                            if self.grid[p].value == 0
                                && (self.grid[p].corner_notes & bit != 0
                                    || self.grid[p].center_notes & bit != 0)
                            {
                                affected.push(p);
                            }
                        }
                    }
                    let undo_entry = self.begin_undo(&affected);

                    let is_correct = self.solution_board.cells[idx] == num;
                    if !is_correct {
                        self.mistakes_count += 1;
                    }
                    self.grid[idx].value = num;
                    self.grid[idx].corner_notes = 0;
                    self.grid[idx].center_notes = 0;

                    if self.settings.smart_notes {
                        // Clear this digit from every peer's pencil marks -
                        // same row, same column, AND same box, not just the
                        // box: e.g. if three cells in a box each had a "2"
                        // pencilled in and this placement resolves which one
                        // is actually 2, the other two lose that mark - and
                        // the same logic applies along the row and column.
                        for &p in &peer_ids {
                            if self.grid[p].value == 0 {
                                self.grid[p].corner_notes &= !bit;
                                self.grid[p].center_notes &= !bit;
                            }
                        }
                    }

                    self.record_action(idx, "P", num, 0);
                    self.undo_stack.push(undo_entry);

                    if self.settings.sound_enabled {
                        if is_correct {
                            self.sound.play_click();
                        } else {
                            self.sound.play_error();
                        }
                    }
                }

                let was_solved = self.solved;
                self.update_solved_status();
                if !was_solved && self.solved && self.settings.sound_enabled {
                    self.sound.play_success();
                }
            }
            NoteMode::Corner => {
                let undo_entry = self.begin_undo(&[idx]);
                let bit: u16 = 1 << num;
                let active = (self.grid[idx].corner_notes & bit) == 0;
                self.grid[idx].corner_notes ^= bit;
                self.record_action(idx, if active { "x+" } else { "x-" }, num, 0);
                self.undo_stack.push(undo_entry);
                if self.settings.sound_enabled {
                    self.sound.play_ui();
                }
            }
            NoteMode::Center => {
                let undo_entry = self.begin_undo(&[idx]);
                let bit: u16 = 1 << num;
                let active = (self.grid[idx].center_notes & bit) == 0;
                self.grid[idx].center_notes ^= bit;
                self.record_action(idx, if active { "e+" } else { "e-" }, num, 0);
                self.undo_stack.push(undo_entry);
                if self.settings.sound_enabled {
                    self.sound.play_ui();
                }
            }
        }
    }

    pub fn clear_current_cell(&mut self) {
        let Some(idx) = self.selected_cell else {
            return;
        };
        if self.grid[idx].given {
            return;
        }
        let has_content = self.grid[idx].value != 0
            || self.grid[idx].corner_notes != 0
            || self.grid[idx].center_notes != 0;
        if !has_content {
            return;
        }

        let undo_entry = self.begin_undo(&[idx]);

        if self.grid[idx].value != 0 {
            let prev = self.grid[idx].value;
            self.grid[idx].value = 0;
            self.record_action(idx, "C", prev, 0);
        } else {
            self.grid[idx].corner_notes = 0;
            self.grid[idx].center_notes = 0;
            self.record_action(idx, "N", 0, 0);
        }
        self.undo_stack.push(undo_entry);
        self.update_solved_status();
        if self.settings.sound_enabled {
            self.sound.play_ui();
        }
    }

    /// Restores every cell an action touched (not just one - see "smart
    /// notes" above) and rolls the mistake counter back too. Still recorded
    /// as an `"U"` move in the log, naming which step it reverted, so the
    /// exported session reflect what actually happened.
    pub fn undo(&mut self) {
        if let Some(entry) = self.undo_stack.pop() {
            for c in &entry.cells {
                self.grid[c.idx].value = c.value;
                self.grid[c.idx].corner_notes = c.corner_notes;
                self.grid[c.idx].center_notes = c.center_notes;
            }
            self.mistakes_count = entry.mistakes_count;
            let primary = entry.cells.first().map(|c| c.idx).unwrap_or(0);
            self.selected_cell = Some(primary);
            self.record_action(primary, "U", 0, entry.step);
            self.update_solved_status();
            if self.settings.sound_enabled {
                self.sound.play_ui();
            }
        }
    }

    fn update_solved_status(&mut self) {
        self.solved = (0..CELLS).all(|i| self.grid[i].value == self.solution_board.cells[i]);
    }

    fn build_session_report(&self) -> GameSessionReport {
        let givens_str: String = self
            .initial_board
            .cells
            .iter()
            .map(|&c| if c == 0 { '.' } else { (b'0' + c) as char })
            .collect();
        let sol_str: String = self
            .solution_board
            .cells
            .iter()
            .map(|&c| (b'0' + c) as char)
            .collect();
        GameSessionReport {
            engine_version: "3.0.0-rs".to_string(),
            difficulty: format!("{:?}", self.difficulty),
            format: replay::FORMAT_DOC.to_string(),
            givens_board: givens_str,
            solution_board: sol_str,
            started_at_unix_ms: self.started_at_unix_ms,
            total_solve_time_seconds: self.start_time.elapsed().as_secs_f64(),
            mistakes_count: self.mistakes_count,
            completed: self.solved,
            moves: self.moves_log.clone(),
        }
    }

    /// Exports the session as compact JSON. Writes a timestamped file to
    /// `<user data dir>/replays/` - a writable per-user location that works
    /// whether the app is running from `cargo run` or fully installed, unlike
    /// writing next to the executable, which fails once installed to a
    /// system location. Also tries the clipboard as a bonus, and always sets
    /// `export_status` so the UI shows what actually happened instead of
    /// failing silently.
    pub fn export_session(&mut self) {
        let report = self.build_session_report();

        let json = match report.to_json_string() {
            Ok(j) => j,
            Err(e) => {
                self.export_status = Some((
                    format!("⚠ Export failed: couldn't serialize session ({e})"),
                    Instant::now(),
                ));
                return;
            }
        };

        let dir = user_data_dir().join("replays");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.export_status = Some((
                format!("⚠ Export failed: couldn't create {} ({e})", dir.display()),
                Instant::now(),
            ));
            return;
        }

        let filename = format!("replay_{}.json", unix_millis_now());
        let path = dir.join(&filename);

        match std::fs::write(&path, &json) {
            Ok(()) => {
                let clipboard_ok = arboard::Clipboard::new()
                    .and_then(|mut c| c.set_text(json.clone()))
                    .is_ok();
                let clip_note = if clipboard_ok {
                    " (also copied to clipboard)"
                } else {
                    ""
                };
                self.export_status = Some((
                    format!(
                        "✅ Exported to {}{} ({} bytes)",
                        path.display(),
                        clip_note,
                        json.len()
                    ),
                    Instant::now(),
                ));
            }
            Err(e) => {
                self.export_status = Some((
                    format!("⚠ Export failed: couldn't write {} ({e})", path.display()),
                    Instant::now(),
                ));
            }
        }
    }

    // ---- Screens ----

    fn render_main_menu(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        ui.add_space(36.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("Sudoku Studio Pro")
                    .size(30.0)
                    .strong()
                    .color(colors.text_given),
            );
            ui.add_space(20.0);

            let has_active_game = !self.grid.is_empty() && !self.solved;
            if has_active_game {
                if ui
                    .add_sized([220.0, 40.0], egui::Button::new("▶ Resume Game"))
                    .clicked()
                {
                    self.screen = Screen::Playing;
                }
                ui.add_space(10.0);
            }

            ui.label(
                egui::RichText::new("New Game - choose a difficulty")
                    .size(14.0)
                    .color(colors.text_user),
            );
            ui.add_space(8.0);

            egui::Grid::new("menu_difficulty_grid")
                .spacing([8.0, 8.0])
                .show(ui, |ui| {
                    for (i, d) in Difficulty::ALL.iter().enumerate() {
                        if ui
                            .add_sized([140.0, 32.0], egui::Button::new(format!("{:?}", d)))
                            .clicked()
                        {
                            self.start_new_game(*d);
                            self.screen = Screen::Playing;
                        }
                        if (i + 1) % 3 == 0 {
                            ui.end_row();
                        }
                    }
                });

            ui.add_space(24.0);
            if ui
                .add_sized([220.0, 34.0], egui::Button::new("⚙ Options"))
                .clicked()
            {
                self.options_return_to = Screen::MainMenu;
                self.screen = Screen::Options;
            }
            ui.add_space(8.0);
            if ui
                .add_sized([220.0, 34.0], egui::Button::new("✖ Quit"))
                .clicked()
            {
                std::process::exit(0);
            }
        });
    }

    fn render_options(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        ui.add_space(16.0);
        ui.heading(egui::RichText::new("Options").color(colors.text_given));
        ui.add_space(12.0);

        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label("Theme:");
            if ui
                .selectable_label(self.theme == AppTheme::Dark, "Dark")
                .clicked()
            {
                self.theme = AppTheme::Dark;
                changed = true;
            }
            if ui
                .selectable_label(self.theme == AppTheme::Light, "Light")
                .clicked()
            {
                self.theme = AppTheme::Light;
                changed = true;
            }
        });
        ui.add_space(8.0);

        changed |= ui
            .checkbox(&mut self.settings.sound_enabled, "Sound effects")
            .changed();
        changed |= ui
            .checkbox(
                &mut self.settings.smart_notes,
                "Smart notes (auto-clear a digit's pencil marks from its row, column, and box when placed)",
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut self.settings.highlight_same_number,
                "Highlight matching numbers",
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut self.settings.highlight_peers,
                "Highlight row / column / box",
            )
            .changed();
        changed |= ui
            .checkbox(&mut self.settings.show_mistakes, "Show mistakes in red")
            .changed();

        if changed {
            self.settings.theme = self.theme;
            save_settings(&self.settings);
        }

        ui.add_space(24.0);
        if ui.button("← Back").clicked() {
            self.screen = self.options_return_to;
        }
    }

    fn render_playing(&mut self, ui: &mut egui::Ui) {
        let colors = self.theme_colors();
        let scale = (ui.available_width() / 620.0).clamp(0.72, 1.3);

        // Top Header
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0 * scale, 6.0 * scale);

            if ui
                .button(egui::RichText::new("☰ Menu").size(13.0 * scale))
                .clicked()
            {
                self.screen = Screen::MainMenu;
            }
            if ui
                .button(egui::RichText::new("⚙").size(13.0 * scale))
                .clicked()
            {
                self.options_return_to = Screen::Playing;
                self.screen = Screen::Options;
            }

            let theme_label = match self.theme {
                AppTheme::Dark => "☀ Light",
                AppTheme::Light => "🌙 Dark",
            };
            if ui
                .button(egui::RichText::new(theme_label).size(13.0 * scale))
                .clicked()
            {
                self.theme = match self.theme {
                    AppTheme::Dark => AppTheme::Light,
                    AppTheme::Light => AppTheme::Dark,
                };
                self.settings.theme = self.theme;
                save_settings(&self.settings);
            }

            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("💾 Export")
                            .size(13.0 * scale)
                            .color(Color32::WHITE),
                    )
                    .fill(colors.accent),
                )
                .on_hover_text(
                    "Save the full puzzle + every move (guesses, notes, timing) as compact JSON",
                )
                .clicked()
            {
                self.export_session();
            }

            let undo_enabled = !self.undo_stack.is_empty();
            if ui
                .add_enabled(
                    undo_enabled,
                    egui::Button::new(egui::RichText::new("↶ Undo").size(13.0 * scale)),
                )
                .on_hover_text("Undo the last move (Ctrl+Z)")
                .clicked()
            {
                self.undo();
            }

            if ui
                .button(egui::RichText::new("⚡ New").size(13.0 * scale))
                .clicked()
            {
                self.start_new_game(self.difficulty);
            }

            egui::ComboBox::from_id_source("diff_select")
                .selected_text(format!("{:?}", self.difficulty))
                .width(140.0 * scale)
                .show_ui(ui, |ui| {
                    for d in Difficulty::ALL {
                        if ui
                            .selectable_value(&mut self.difficulty, d, format!("{:?}", d))
                            .clicked()
                        {
                            self.start_new_game(d);
                        }
                    }
                });
        });

        if let Some((msg, _)) = &self.export_status {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(msg)
                    .size(12.5 * scale)
                    .color(colors.text_user),
            );
        }

        ui.add_space(6.0);

        // Stats Bar
        egui::Frame::none()
            .fill(colors.panel)
            .rounding(Rounding::same(6.0))
            .inner_margin(Vec2::new(8.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let elapsed = self.start_time.elapsed().as_secs();
                    ui.label(
                        egui::RichText::new(format!("⏱ {:02}:{:02}", elapsed / 60, elapsed % 60))
                            .size(14.0 * scale)
                            .color(colors.text_given),
                    );
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!("❌ Mistakes: {}", self.mistakes_count))
                            .size(14.0 * scale)
                            .color(Color32::from_rgb(235, 80, 80)),
                    );
                    ui.separator();
                    let mode_txt = match self.note_mode {
                        NoteMode::Normal => "Mode: [Z] Digit Placement",
                        NoteMode::Corner => "Mode: [X] Corner (Snyder)",
                        NoteMode::Center => "Mode: [C] Center Candidate",
                    };
                    ui.label(
                        egui::RichText::new(mode_txt)
                            .size(13.0 * scale)
                            .color(colors.text_user)
                            .strong(),
                    );
                });
            });

        ui.add_space(8.0);

        if self.generating || self.grid.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.add(egui::widgets::Spinner::new().size(32.0));
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Generating puzzle…")
                        .size(15.0)
                        .color(colors.text_user),
                );
                if matches!(
                    self.difficulty,
                    Difficulty::Grandmaster | Difficulty::Ultimate | Difficulty::Diabolical
                ) {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Top-tier puzzles can take a few seconds to dig")
                            .size(12.0)
                            .color(colors.text_user.gamma_multiply(0.7)),
                    );
                }
            });
            return;
        }

        // Responsive Geometry Calculation
        let avail_w = ui.available_width();
        let avail_h = ui.available_height();
        let keypad_gap = 4.0;
        let mode_btn_h = 32.0;
        let num_btn_h = ((avail_w / 9.0) * 0.9).clamp(38.0, 52.0);
        let controls_total_h = mode_btn_h + num_btn_h + keypad_gap * 4.0 + 16.0;
        let board_size = avail_w.min(avail_h - controls_total_h).max(200.0);
        let cell_size = board_size / 9.0;
        let x_offset = (avail_w - board_size).max(0.0) / 2.0;

        // Draw Board
        ui.horizontal(|ui| {
            ui.add_space(x_offset);

            let (response, painter) =
                ui.allocate_painter(Vec2::splat(board_size), egui::Sense::click());
            let origin = response.rect.min;

            let selected_val = self.selected_cell.map(|i| self.grid[i].value).unwrap_or(0);
            let (sel_r, sel_c) = self.selected_cell.map(Board::row_col).unwrap_or((99, 99));

            if response.clicked() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let rel_x = pos.x - origin.x;
                    let rel_y = pos.y - origin.y;
                    if rel_x >= 0.0 && rel_y >= 0.0 && rel_x < board_size && rel_y < board_size {
                        let c = (rel_x / cell_size) as usize;
                        let r = (rel_y / cell_size) as usize;
                        if r < 9 && c < 9 {
                            self.selected_cell = Some(Board::idx(r, c));
                        }
                    }
                }
            }

            for r in 0..ROWS {
                for c in 0..COLS {
                    let idx = Board::idx(r, c);
                    let cell_rect = Rect::from_min_size(
                        Pos2::new(
                            origin.x + c as f32 * cell_size,
                            origin.y + r as f32 * cell_size,
                        ),
                        Vec2::splat(cell_size),
                    );

                    let mut fill = colors.cell_base;
                    let cell_val = self.grid[idx].value;

                    if self.settings.highlight_peers
                        && (r == sel_r || c == sel_c || (r / 3 == sel_r / 3 && c / 3 == sel_c / 3))
                    {
                        fill = colors.cell_seen;
                    }
                    if self.settings.highlight_same_number
                        && selected_val != 0
                        && cell_val == selected_val
                    {
                        fill = colors.highlight_match;
                    }
                    if self.selected_cell == Some(idx) {
                        fill = colors.cell_selected;
                    }

                    painter.rect_filled(cell_rect, 0.0, fill);

                    if cell_val != 0 {
                        let text_color = if self.grid[idx].given {
                            colors.text_given
                        } else if self.settings.show_mistakes
                            && cell_val != self.solution_board.cells[idx]
                        {
                            Color32::from_rgb(235, 75, 75)
                        } else {
                            colors.text_user
                        };

                        painter.text(
                            cell_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            cell_val.to_string(),
                            egui::FontId::proportional(cell_size * 0.58),
                            text_color,
                        );
                    } else {
                        for d in 1..=9 {
                            if (self.grid[idx].corner_notes & (1 << d)) != 0 {
                                let sub_r = (d - 1) / 3;
                                let sub_c = (d - 1) % 3;
                                let corner_pos = Pos2::new(
                                    cell_rect.min.x + (sub_c as f32 + 0.5) * (cell_size / 3.0),
                                    cell_rect.min.y + (sub_r as f32 + 0.5) * (cell_size / 3.0),
                                );
                                let mark_color = if selected_val == d {
                                    Color32::from_rgb(240, 200, 60)
                                } else {
                                    colors.text_given.gamma_multiply(0.6)
                                };
                                painter.text(
                                    corner_pos,
                                    egui::Align2::CENTER_CENTER,
                                    d.to_string(),
                                    egui::FontId::proportional(cell_size * 0.23),
                                    mark_color,
                                );
                            }
                        }

                        let mut center_digits = Vec::new();
                        for d in 1..=9 {
                            if (self.grid[idx].center_notes & (1 << d)) != 0 {
                                center_digits.push(d.to_string());
                            }
                        }
                        if !center_digits.is_empty() {
                            let text = center_digits.join("");
                            painter.text(
                                cell_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                text,
                                egui::FontId::proportional(cell_size * 0.22),
                                Color32::from_rgb(120, 175, 255),
                            );
                        }
                    }
                }
            }

            for i in 0..=9 {
                let stroke = if i % 3 == 0 {
                    Stroke::new(2.5_f32, colors.grid_main)
                } else {
                    Stroke::new(0.8_f32, colors.grid_sub)
                };
                let x = origin.x + i as f32 * cell_size;
                painter.line_segment(
                    [Pos2::new(x, origin.y), Pos2::new(x, origin.y + board_size)],
                    stroke,
                );
                let y = origin.y + i as f32 * cell_size;
                painter.line_segment(
                    [Pos2::new(origin.x, y), Pos2::new(origin.x + board_size, y)],
                    stroke,
                );
            }
        });

        ui.add_space(keypad_gap * 2.0);

        // Keypad & Mode Selectors
        ui.horizontal(|ui| {
            ui.add_space(x_offset);
            ui.vertical(|ui| {
                let mode_btn_w = (board_size - (3.0 * keypad_gap)) / 4.0;
                ui.spacing_mut().item_spacing = Vec2::new(keypad_gap, keypad_gap);

                ui.horizontal(|ui| {
                    let modes = [
                        (NoteMode::Normal, "Digit [Z]"),
                        (NoteMode::Corner, "Corner [X]"),
                        (NoteMode::Center, "Center [C]"),
                    ];
                    for (mode, label) in modes {
                        if ui
                            .add_sized(
                                [mode_btn_w, mode_btn_h],
                                egui::SelectableLabel::new(self.note_mode == mode, label),
                            )
                            .clicked()
                        {
                            self.note_mode = mode;
                        }
                    }
                    if ui
                        .add_sized([mode_btn_w, mode_btn_h], egui::Button::new("⌫ Clear"))
                        .clicked()
                    {
                        self.clear_current_cell();
                    }
                });

                let num_btn_w = (board_size - (8.0 * keypad_gap)) / 9.0;
                ui.horizontal(|ui| {
                    for num in 1..=9 {
                        if ui
                            .add_sized(
                                [num_btn_w, num_btn_h],
                                egui::Button::new(
                                    egui::RichText::new(num.to_string())
                                        .size(num_btn_h * 0.46)
                                        .strong(),
                                ),
                            )
                            .clicked()
                        {
                            self.handle_number_input(num);
                        }
                    }
                });
            });
        });
    }
}

fn unix_millis_now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

impl eframe::App for SudokuApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.gen_rx {
            if let Ok((puzzle, solution)) = rx.try_recv() {
                self.apply_generated(puzzle, solution);
                self.gen_rx = None;
                self.generating = false;
            }
        }

        if let Some((_, shown_at)) = &self.export_status {
            if shown_at.elapsed() > Duration::from_secs(6) {
                self.export_status = None;
            }
        }

        if self.screen == Screen::Playing {
            ctx.input(|i| {
                if self.generating || self.grid.is_empty() {
                    return;
                }

                for num in 1..=9 {
                    let key = match num {
                        1 => Key::Num1,
                        2 => Key::Num2,
                        3 => Key::Num3,
                        4 => Key::Num4,
                        5 => Key::Num5,
                        6 => Key::Num6,
                        7 => Key::Num7,
                        8 => Key::Num8,
                        9 => Key::Num9,
                        _ => unreachable!(),
                    };
                    if i.key_pressed(key) {
                        if i.modifiers.shift {
                            let prev = self.note_mode;
                            self.note_mode = NoteMode::Corner;
                            self.handle_number_input(num);
                            self.note_mode = prev;
                        } else if i.modifiers.ctrl {
                            let prev = self.note_mode;
                            self.note_mode = NoteMode::Center;
                            self.handle_number_input(num);
                            self.note_mode = prev;
                        } else {
                            self.handle_number_input(num);
                        }
                    }
                }

                if i.key_pressed(Key::Backspace) || i.key_pressed(Key::Delete) {
                    self.clear_current_cell();
                }
                if i.key_pressed(Key::Z) && i.modifiers.ctrl {
                    self.undo();
                } else if i.key_pressed(Key::Z) {
                    self.note_mode = NoteMode::Normal;
                }
                if i.key_pressed(Key::X) {
                    self.note_mode = NoteMode::Corner;
                }
                if i.key_pressed(Key::C) {
                    self.note_mode = NoteMode::Center;
                }
                if i.key_pressed(Key::Escape) {
                    self.screen = Screen::MainMenu;
                }

                if let Some(idx) = self.selected_cell {
                    let (r, c) = Board::row_col(idx);
                    if (i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::W)) && r > 0 {
                        self.selected_cell = Some(Board::idx(r - 1, c));
                    }
                    if (i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::S)) && r < 8 {
                        self.selected_cell = Some(Board::idx(r + 1, c));
                    }
                    if (i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::A)) && c > 0 {
                        self.selected_cell = Some(Board::idx(r, c - 1));
                    }
                    if (i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::D)) && c < 8 {
                        self.selected_cell = Some(Board::idx(r, c + 1));
                    }
                }
            });
        }

        let colors = self.theme_colors();

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(colors.bg).inner_margin(12.0))
            .show(ctx, |ui| match self.screen {
                Screen::MainMenu => self.render_main_menu(ui),
                Screen::Options => self.render_options(ui),
                Screen::Playing => self.render_playing(ui),
            });

        if self.screen == Screen::Playing && self.solved {
            egui::Window::new("🎉 Puzzle Solved!")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    let elapsed = self.start_time.elapsed().as_secs();
                    ui.label(format!("Time: {:02}:{:02}", elapsed / 60, elapsed % 60));
                    ui.label(format!("Mistakes: {}", self.mistakes_count));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("New Game").clicked() {
                            self.screen = Screen::MainMenu;
                        }
                        if ui.button("💾 Export Session").clicked() {
                            self.export_session();
                        }
                    });
                });
        }

        // Repaint on a timer instead of unconditionally every frame - menus
        // are fully static so they get the slowest tick; the game screen
        // needs enough to keep the elapsed timer live; generation needs a
        // faster tick so the result shows up promptly.
        let tick = if self.generating {
            Duration::from_millis(50)
        } else if self.screen == Screen::Playing {
            Duration::from_millis(200)
        } else {
            Duration::from_millis(400)
        };
        ctx.request_repaint_after(tick);
    }
}

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([620.0, 780.0])
            .with_min_inner_size([380.0, 520.0])
            .with_title("Sudoku Studio Pro"),
        ..Default::default()
    };
    eframe::run_native(
        "Sudoku Studio Pro",
        native_options,
        Box::new(|cc| Box::new(SudokuApp::new(cc))),
    )
}
