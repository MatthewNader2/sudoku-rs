use rand::rngs::ThreadRng;
use rand::seq::SliceRandom;
use rand::thread_rng;

pub const ROWS: usize = 9;
pub const COLS: usize = 9;
pub const CELLS: usize = 81;

/// Difficulty ladder. The first six tiers use the same names as sudoku.com's
/// own Easy/Medium/Hard/Expert/Master/Extreme scale. Beyond that we add four
/// more tiers, named after the tiers apps like Sudoku Coach use for their
/// upper end (Evil / Diabolical / Grandmaster style naming), since sudoku.com
/// tops out at Extreme.
///
/// IMPORTANT: there is no universal, agreed-upon mapping from technique to
/// difficulty name across the sudoku world - every site/app picks its own
/// thresholds (this is confirmed by sudoku.coach's own "Sudoku Difficulty"
/// explainer). The technique-to-tier mapping below is this app's own
/// deliberately monotonic ladder: each tier is a strict superset of the
/// techniques below it. See `human_solver.rs` for exactly which technique
/// unlocks which tier.
///
/// Declaration order matters: `#[derive(PartialOrd, Ord)]` compares variants
/// by declaration order, so `Difficulty::Hard < Difficulty::Expert` etc. just
/// works, and `.max()` picks "whichever is harder" for free.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
    Expert,
    Master,
    Extreme,
    Evil,
    Diabolical,
    Grandmaster,
    Ultimate,
}

impl Difficulty {
    pub const ALL: [Difficulty; 10] = [
        Difficulty::Easy,
        Difficulty::Medium,
        Difficulty::Hard,
        Difficulty::Expert,
        Difficulty::Master,
        Difficulty::Extreme,
        Difficulty::Evil,
        Difficulty::Diabolical,
        Difficulty::Grandmaster,
        Difficulty::Ultimate,
    ];
}

#[derive(Clone)]
pub struct Board {
    pub cells: [u8; CELLS],
}

impl Board {
    pub fn new() -> Self {
        Self { cells: [0; CELLS] }
    }

    pub fn idx(r: usize, c: usize) -> usize {
        r * 9 + c
    }

    pub fn row_col(idx: usize) -> (usize, usize) {
        (idx / 9, idx % 9)
    }

    pub fn is_valid_placement(&self, idx: usize, num: u8) -> bool {
        let (r, c) = Self::row_col(idx);
        let box_r = (r / 3) * 3;
        let box_c = (c / 3) * 3;

        for i in 0..9 {
            if self.cells[Self::idx(r, i)] == num && i != c {
                return false;
            }
            if self.cells[Self::idx(i, c)] == num && i != r {
                return false;
            }
            let br = box_r + i / 3;
            let bc = box_c + i % 3;
            if self.cells[Self::idx(br, bc)] == num && (br != r || bc != c) {
                return false;
            }
        }
        true
    }

    pub fn solve(&mut self) -> bool {
        for idx in 0..CELLS {
            if self.cells[idx] == 0 {
                for num in 1..=9 {
                    if self.is_valid_placement(idx, num) {
                        self.cells[idx] = num;
                        if self.solve() {
                            return true;
                        }
                        self.cells[idx] = 0;
                    }
                }
                return false;
            }
        }
        true
    }

    pub fn count_solutions(&mut self, count: &mut usize) {
        if *count >= 2 {
            return;
        }
        let mut empty_idx = None;
        for i in 0..CELLS {
            if self.cells[i] == 0 {
                empty_idx = Some(i);
                break;
            }
        }

        match empty_idx {
            None => *count += 1,
            Some(idx) => {
                for num in 1..=9 {
                    if self.is_valid_placement(idx, num) {
                        self.cells[idx] = num;
                        self.count_solutions(count);
                        self.cells[idx] = 0;
                        if *count >= 2 {
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Seed the three diagonal boxes (which share no row/col/box constraints
    /// with each other) with independent random permutations before solving.
    /// Gives genuinely varied full grids while keeping `solve()` itself
    /// simple, deterministic, and fast.
    fn fill_diagonal_boxes(&mut self) {
        let mut rng = thread_rng();
        for b in 0..3 {
            let mut nums: Vec<u8> = (1..=9).collect();
            nums.shuffle(&mut rng);
            let base = b * 3;
            let mut n = 0;
            for r in 0..3 {
                for c in 0..3 {
                    self.cells[Self::idx(base + r, base + c)] = nums[n];
                    n += 1;
                }
            }
        }
    }

    /// Generates a puzzle rated at exactly `target` difficulty by the human
    /// technique solver in `human_solver.rs`, not just by clue count. Clue
    /// count alone is a poor proxy for how a puzzle actually plays: two
    /// puzzles with the same number of givens can rate completely
    /// differently depending on *which* cells are given. Digging to some
    /// clue count and hoping the rating lands where we want doesn't work
    /// well either - named techniques like X-Wing or a Unique Rectangle
    /// need a fairly specific structural alignment, so a puzzle dug purely
    /// at random tends to land either trivially easy or, once singles run
    /// out, straight into needing full bifurcation - with very little
    /// landing precisely in between.
    ///
    /// So instead we dig one cell at a time and *check the rating as we go*:
    /// a removal is only kept if the puzzle (a) still has a unique solution
    /// and (b) doesn't rate harder than `target`. This steers the dig
    /// directly toward "as hard as it can get without exceeding target"
    /// rather than gambling on a clue count.
    pub fn generate(target: Difficulty) -> (Self, Self) {
        const MAX_FULL_BOARD_ATTEMPTS: usize = 8;

        let mut rng = thread_rng();
        let mut best: Option<(Board, Board, Difficulty)> = None;

        for _ in 0..MAX_FULL_BOARD_ATTEMPTS {
            let mut full_board = Board::new();
            full_board.fill_diagonal_boxes();
            full_board.solve();
            let solution = full_board.clone();

            let (puzzle, rating) = dig_to_target(&full_board, target, &mut rng);

            if rating == target {
                return (puzzle, solution);
            }

            let is_closer = match &best {
                None => true,
                Some((_, _, best_rating)) => {
                    tier_distance(rating, target) < tier_distance(*best_rating, target)
                }
            };
            if is_closer {
                best = Some((puzzle, solution, rating));
            }
        }

        let (puzzle, solution, _) = best.expect("loop always runs at least once");
        (puzzle, solution)
    }
}

/// How far apart two tiers are on the ladder - used only to pick the
/// "closest" fallback puzzle if we can't hit the exact target within the
/// attempt budget.
fn tier_distance(a: Difficulty, b: Difficulty) -> i32 {
    (a as i32 - b as i32).abs()
}

/// Digs `full` down toward `target`, re-rating after every candidate
/// removal and only keeping ones that don't push the puzzle past `target`.
/// Makes repeated shuffled passes over the remaining givens (a cell that
/// can't be removed yet - it would blow the rating or break uniqueness -
/// can become removable once other cells are gone) until a full pass makes
/// no further progress. Returns the puzzle reached and its true rating
/// (equal to `target` on success; lower than `target` if this particular
/// full grid couldn't be dug hard enough - the caller retries with a fresh
/// grid in that case).
fn dig_to_target(full: &Board, target: Difficulty, rng: &mut ThreadRng) -> (Board, Difficulty) {
    let mut puzzle = full.clone();
    let mut current_rating = Difficulty::Easy;

    loop {
        let mut indices: Vec<usize> = (0..CELLS).filter(|&i| puzzle.cells[i] != 0).collect();
        indices.shuffle(rng);

        let mut progressed = false;
        for idx in indices {
            let temp = puzzle.cells[idx];
            puzzle.cells[idx] = 0;

            let mut count = 0;
            let mut check_board = puzzle.clone();
            check_board.count_solutions(&mut count);
            if count != 1 {
                puzzle.cells[idx] = temp;
                continue;
            }

            let rating = crate::human_solver::rate_difficulty_capped(&puzzle, target);
            if rating > target {
                puzzle.cells[idx] = temp;
                continue;
            }

            current_rating = rating;
            progressed = true;
        }

        if !progressed {
            break;
        }
    }

    (puzzle, current_rating)
}
