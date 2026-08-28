use serde::{Deserialize, Serialize};

/// One move, as a plain tuple so serde emits it as a compact JSON array
/// instead of an object with repeated field names on every entry - this is
/// most of the token/byte savings versus a field-per-move object format.
///
/// Shape: `(cell_idx, action_code, digit, elapsed_ms, extra)`
///
/// - `cell_idx`: 0..=80, row-major (`row = cell_idx / 9`, `col = cell_idx % 9`).
/// - `action_code`:
///     - `"P"`  place digit           - `digit` = the value placed
///     - `"C"`  clear digit           - `digit` = the value that was cleared
///     - `"N"`  clear notes           - (no value change; `digit` unused, 0)
///     - `"x+"` corner note added     - `digit` = the note digit
///     - `"x-"` corner note removed   - `digit` = the note digit
///     - `"e+"` center note added     - `digit` = the note digit
///     - `"e-"` center note removed   - `digit` = the note digit
///     - `"U"`  undo                  - `extra` = 1-based step number reverted
/// - `elapsed_ms`: milliseconds since the session started.
/// - `extra`: only meaningful for `"U"` (see above); 0 otherwise.
///
/// We deliberately do NOT repeat the full board state on every move. Given
/// `givens_board` and the move list, the board (values + every cell's notes)
/// at any step is 100% reconstructible by replaying moves in order - storing
/// it again on every entry was pure redundancy that bloated the file (and
/// the token count) for no informational gain. Whether a placement was
/// right or wrong is likewise derivable by comparing `digit` against
/// `solution_board` - no need to duplicate that per move either.
pub type MoveRecord = (usize, String, u8, u128, usize);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameSessionReport {
    pub engine_version: String,
    pub difficulty: String,
    /// Human/AI-readable schema note, so this file is self-describing
    /// without needing this source file for reference.
    pub format: String,
    /// 81 chars, row-major, '.' = empty.
    pub givens_board: String,
    /// 81 chars, row-major.
    pub solution_board: String,
    pub started_at_unix_ms: u128,
    pub total_solve_time_seconds: f64,
    pub mistakes_count: usize,
    pub completed: bool,
    pub moves: Vec<MoveRecord>,
}

/// Schema string embedded in every export - keeps the file self-contained.
pub const FORMAT_DOC: &str =
    "moves[i] = [cell_idx(0-80, row-major), action_code, digit, elapsed_ms, extra]. \
action_code: P=place(digit=value placed), C=clear(digit=value cleared), N=clear notes, \
x+/x-=corner note add/remove(digit=note), e+/e-=center note add/remove(digit=note), \
U=undo(extra=1-based step reverted). Board state at step i is givens_board replayed \
through moves[0..=i]; correctness of a placement = (digit == solution_board[cell_idx]).";

impl GameSessionReport {
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        // Compact, not pretty-printed: indentation/newlines are pure
        // overhead for a machine-oriented export like this one.
        serde_json::to_string(self)
    }
}
