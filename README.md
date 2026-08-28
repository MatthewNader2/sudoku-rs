# 🧩 Sudoku Studio Pro

A fast, responsive, and modern Sudoku desktop application written in Rust using [`egui`](https://github.com/emilk/egui) / [`eframe`](https://github.com/emilk/egui/tree/master/crates/eframe).

---

## ✨ Features

- **Multiple Note / Pencil Modes:**
  - `Digit [Z]`: Standard cell number placement.
  - `Corner [X]`: Snyder notation for corner candidates.
  - `Center [C]`: Center candidate marks.
- **Smart Notes:** Automatically clears placed digits from candidate marks in the same row, column, and 3x3 box.
- **Dynamic Difficulty:** Background puzzle generation supporting tiers from beginner to Grandmaster / Diabolical.
- **Rich Audio & Visual Cues:** Sound effects via `rodio`, error highlighting, matching digit highlights, and light/dark theme toggles.
- **Full History & Replay Export:** Export full game metrics, board states, timings, and every move to JSON format.
- **Multi-Cell Undo:** Deep state rollbacks (`Ctrl + Z`) restoring affected peer marks and mistakes counters.

---

## 🎮 Controls & Keybindings

| Action | Keybinding |
| :--- | :--- |
| **Move Selection** | `Arrow Keys` / `W` `A` `S` `D` / Mouse Click |
| **Enter Digit** | `1` – `9` |
| **Add/Remove Corner Note** | `Shift` + `1` – `9` *(or switch to Corner mode `X`)* |
| **Add/Remove Center Note** | `Ctrl` + `1` – `9` *(or switch to Center mode `C`)* |
| **Clear Cell / Notes** | `Backspace` or `Delete` |
| **Undo Action** | `Ctrl + Z` |
| **Toggle Digit Mode** | `Z` |
| **Toggle Corner Mode** | `X` |
| **Toggle Center Mode** | `C` |
| **Return to Menu** | `Esc` |

---

## 📦 Installation & Releases

Pre-built portable bundles for **Linux** and **Windows** are available on the [Releases](https://github.com/your-username/rs-sudoku/releases) page.

### Linux
1. Download `rs-sudoku-linux-x86_64.tar.gz`.
2. Extract the archive:
   ```bash
   tar -xzvf rs-sudoku-linux-x86_64.tar.gz
   cd rs-sudoku-linux
   ```
3. Make executable and run:
   ```bash
   chmod +x rs-sudoku
   ./rs-sudoku
   ```

### Windows
1. Download `rs-sudoku-windows-x86_64.zip`.
2. Extract the archive.
3. Run `rs-sudoku.exe`.

---

## 🛠️ Building from Source

### Prerequisites (Linux)
Install the required audio and device development libraries:
```bash
sudo apt update
sudo apt install -y libasound2-dev libudev-dev
```

### Build & Run
```bash
# Clone the repository
git clone [https://github.com/your-username/rs-sudoku.git](https://github.com/your-username/rs-sudoku.git)
cd rs-sudoku

# Run debug build
cargo run

# Build optimized release binary
cargo build --release
```

---

## 📁 Project Structure

```
.
├── Cargo.toml
├── build.rs          # Compiles Windows application icon resources
├── Icon.ico          # Application icon
├── sounds/           # Bundled audio files (click, error, success, ui_tick)
└── src/
    ├── engine.rs     # Core board logic, puzzle generator, and solver
    ├── human_solver.rs
    ├── main.rs       # egui / eframe GUI interface and state machine
    └── replay.rs     # Game telemetry and JSON serialization
```

---

## 📄 License

This project is licensed under the MIT License.
