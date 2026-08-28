//! Human-technique sudoku grader.
//!
//! This is deliberately *not* a brute-force solver. It applies the same
//! named techniques a person would (singles, locked candidates, naked/hidden
//! subsets, fish, wings, unique rectangles, single-digit coloring), always
//! trying the easiest applicable technique first, and only reaches for a
//! bounded "what if" bifurcation as an absolute last resort. The hardest
//! technique it had to use to finish the grid *is* the puzzle's difficulty
//! rating - this mirrors how sites like sudoku.coach and Sudoku Exchange
//! ("SE rating") grade puzzles: by the hardest step required, not by how
//! many clues are showing.
//!
//! Tier mapping (each tier is a strict superset of the one below it):
//!   Easy        - naked singles, hidden singles
//!   Medium      - + locked candidates (pointing pairs / box-line reduction)
//!   Hard        - + naked pairs, hidden pairs
//!   Expert      - + naked/hidden triples and quads
//!   Master      - + X-Wing, and single-digit chains (this covers Skyscraper
//!                  and Two-String Kite too, since those are just short
//!                  chains of strong links - see `apply_single_digit_chains`)
//!   Extreme     - + Swordfish
//!   Evil        - + XY-Wing, XYZ-Wing, Unique Rectangle (type 1)
//!   Diabolical  - + W-Wing, Jellyfish
//!   Grandmaster - needs one level of bounded "what if" bifurcation
//!   Ultimate    - needs two (or couldn't be confirmed within budget)

use crate::engine::{Board, Difficulty, CELLS};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// bits 1..=9 used (bit 0 always unused), matching the convention already
/// used for corner/center pencil marks in the UI layer.
const FULL_MASK: u16 = 0b0000_0011_1111_1110;

/// Safety net: `solve_and_grade` always terminates in theory (every branch
/// either makes concrete progress or returns), but this caps the loop so a
/// subtle bug in a technique can never hang the background generation
/// thread - it just falls back to whatever tier was reached so far.
const MAX_GRADE_ITERATIONS: usize = 400;

#[derive(Clone)]
struct Solver {
    cells: [u8; CELLS],
    candidates: [u16; CELLS],
}

fn cells_in_row(r: usize) -> impl Iterator<Item = usize> {
    (0..9).map(move |c| r * 9 + c)
}
fn cells_in_col(c: usize) -> impl Iterator<Item = usize> {
    (0..9).map(move |r| r * 9 + c)
}
fn cells_in_box(b: usize) -> impl Iterator<Item = usize> {
    let br = (b / 3) * 3;
    let bc = (b % 3) * 3;
    (0..9).map(move |i| (br + i / 3) * 9 + (bc + i % 3))
}
fn box_of(idx: usize) -> usize {
    let (r, c) = (idx / 9, idx % 9);
    (r / 3) * 3 + c / 3
}
/// The 27 units (9 rows + 9 cols + 9 boxes), computed once and cached.
/// Techniques call this a lot (multiple times per solving pass, many passes
/// per puzzle, many puzzles per generation attempt), so rebuilding a fresh
/// `Vec<Vec<usize>>` every call was a real cost - this made a measurable
/// difference in generation time for the harder tiers.
static UNITS: OnceLock<Vec<Vec<usize>>> = OnceLock::new();
fn all_units() -> &'static Vec<Vec<usize>> {
    UNITS.get_or_init(|| {
        let mut units = Vec::with_capacity(27);
        for r in 0..9 {
            units.push(cells_in_row(r).collect());
        }
        for c in 0..9 {
            units.push(cells_in_col(c).collect());
        }
        for b in 0..9 {
            units.push(cells_in_box(b).collect());
        }
        units
    })
}
fn is_peer(a: usize, b: usize) -> bool {
    if a == b {
        return false;
    }
    let (ra, ca) = (a / 9, a % 9);
    let (rb, cb) = (b / 9, b % 9);
    ra == rb || ca == cb || box_of(a) == box_of(b)
}
fn has_peer_pair(nodes: &[usize]) -> bool {
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            if is_peer(nodes[i], nodes[j]) {
                return true;
            }
        }
    }
    false
}
/// Unique peers of `idx` (20 in standard sudoku: 8 row + 8 col + 4 remaining
/// box peers), precomputed once for all 81 cells and cached - same
/// reasoning as `all_units` above; this is the single hottest lookup in the
/// whole solver.
static PEERS: OnceLock<Vec<Vec<usize>>> = OnceLock::new();
fn peers_of(idx: usize) -> &'static [usize] {
    let table = PEERS.get_or_init(|| {
        (0..CELLS)
            .map(|idx| {
                let (r, c) = (idx / 9, idx % 9);
                let b = box_of(idx);
                let mut seen = [false; CELLS];
                let mut out = Vec::with_capacity(20);
                for p in cells_in_row(r)
                    .chain(cells_in_col(c))
                    .chain(cells_in_box(b))
                {
                    if p != idx && !seen[p] {
                        seen[p] = true;
                        out.push(p);
                    }
                }
                out
            })
            .collect()
    });
    &table[idx]
}
/// Set-bit digits (1..=9) in a candidate mask.
fn bits(mask: u16) -> Vec<u8> {
    (1..=9u8).filter(|&d| mask & (1u16 << d) != 0).collect()
}
/// k-combinations of a slice, as owned Vecs (all the pools this is called on
/// are small - at most 9 - so this is cheap).
fn combinations<T: Copy>(items: &[T], k: usize) -> Vec<Vec<T>> {
    let mut result = Vec::new();
    let mut current = Vec::with_capacity(k);
    fn rec<T: Copy>(
        items: &[T],
        k: usize,
        start: usize,
        current: &mut Vec<T>,
        result: &mut Vec<Vec<T>>,
    ) {
        if current.len() == k {
            result.push(current.clone());
            return;
        }
        for i in start..items.len() {
            current.push(items[i]);
            rec(items, k, i + 1, current, result);
            current.pop();
        }
    }
    rec(items, k, 0, &mut current, &mut result);
    result
}

impl Solver {
    fn from_board(board: &Board) -> Self {
        let mut s = Solver {
            cells: board.cells,
            candidates: [0; CELLS],
        };
        s.recompute_all_candidates();
        s
    }

    fn recompute_all_candidates(&mut self) {
        for idx in 0..CELLS {
            if self.cells[idx] == 0 {
                let mut mask = FULL_MASK;
                for &peer in peers_of(idx) {
                    if self.cells[peer] != 0 {
                        mask &= !(1u16 << self.cells[peer]);
                    }
                }
                self.candidates[idx] = mask;
            } else {
                self.candidates[idx] = 0;
            }
        }
    }

    fn place(&mut self, idx: usize, digit: u8) {
        self.cells[idx] = digit;
        self.candidates[idx] = 0;
        for &p in peers_of(idx) {
            if self.cells[p] == 0 {
                self.candidates[p] &= !(1u16 << digit);
            }
        }
    }

    fn eliminate(&mut self, idx: usize, digit: u8) -> bool {
        let bit = 1u16 << digit;
        if self.candidates[idx] & bit != 0 {
            self.candidates[idx] &= !bit;
            true
        } else {
            false
        }
    }

    fn is_solved(&self) -> bool {
        self.cells.iter().all(|&v| v != 0)
    }

    fn has_contradiction(&self) -> bool {
        (0..CELLS).any(|i| self.cells[i] == 0 && self.candidates[i] == 0)
    }

    // ---- Techniques, roughly easiest to hardest ----

    fn apply_naked_singles(&mut self) -> bool {
        let mut progressed = false;
        for idx in 0..CELLS {
            if self.cells[idx] == 0 && self.candidates[idx].count_ones() == 1 {
                let digit = self.candidates[idx].trailing_zeros() as u8;
                self.place(idx, digit);
                progressed = true;
            }
        }
        progressed
    }

    fn apply_hidden_singles(&mut self) -> bool {
        let mut progressed = false;
        for unit in all_units() {
            for d in 1..=9u8 {
                let bit = 1u16 << d;
                let mut spots = unit
                    .iter()
                    .copied()
                    .filter(|&i| self.cells[i] == 0 && self.candidates[i] & bit != 0);
                if let Some(only) = spots.next() {
                    if spots.next().is_none() {
                        self.place(only, d);
                        progressed = true;
                    }
                }
            }
        }
        progressed
    }

    fn apply_locked_candidates(&mut self) -> bool {
        let mut progressed = false;

        // Pointing: a digit confined to one row/col within a box eliminates
        // that digit from the rest of that row/col outside the box.
        for b in 0..9 {
            let box_cells: Vec<usize> = cells_in_box(b).collect();
            for d in 1..=9u8 {
                let bit = 1u16 << d;
                let spots: Vec<usize> = box_cells
                    .iter()
                    .copied()
                    .filter(|&i| self.cells[i] == 0 && self.candidates[i] & bit != 0)
                    .collect();
                if spots.len() < 2 {
                    continue;
                }

                let rows: HashSet<usize> = spots.iter().map(|&i| i / 9).collect();
                if rows.len() == 1 {
                    let r = *rows.iter().next().unwrap();
                    for i in cells_in_row(r) {
                        if box_of(i) != b && self.cells[i] == 0 && self.eliminate(i, d) {
                            progressed = true;
                        }
                    }
                }
                let cols: HashSet<usize> = spots.iter().map(|&i| i % 9).collect();
                if cols.len() == 1 {
                    let c = *cols.iter().next().unwrap();
                    for i in cells_in_col(c) {
                        if box_of(i) != b && self.cells[i] == 0 && self.eliminate(i, d) {
                            progressed = true;
                        }
                    }
                }
            }
        }

        // Claiming: a digit confined to one box within a row/col eliminates
        // that digit from the rest of that box outside the row/col.
        for r in 0..9 {
            for d in 1..=9u8 {
                let bit = 1u16 << d;
                let spots: Vec<usize> = cells_in_row(r)
                    .filter(|&i| self.cells[i] == 0 && self.candidates[i] & bit != 0)
                    .collect();
                if spots.len() < 2 {
                    continue;
                }
                let boxes: HashSet<usize> = spots.iter().map(|&i| box_of(i)).collect();
                if boxes.len() == 1 {
                    let b = *boxes.iter().next().unwrap();
                    for i in cells_in_box(b) {
                        if i / 9 != r && self.cells[i] == 0 && self.eliminate(i, d) {
                            progressed = true;
                        }
                    }
                }
            }
        }
        for c in 0..9 {
            for d in 1..=9u8 {
                let bit = 1u16 << d;
                let spots: Vec<usize> = cells_in_col(c)
                    .filter(|&i| self.cells[i] == 0 && self.candidates[i] & bit != 0)
                    .collect();
                if spots.len() < 2 {
                    continue;
                }
                let boxes: HashSet<usize> = spots.iter().map(|&i| box_of(i)).collect();
                if boxes.len() == 1 {
                    let b = *boxes.iter().next().unwrap();
                    for i in cells_in_box(b) {
                        if i % 9 != c && self.cells[i] == 0 && self.eliminate(i, d) {
                            progressed = true;
                        }
                    }
                }
            }
        }

        progressed
    }

    /// Naked pair/triple/quad, generalized over `size`.
    fn apply_naked_subsets(&mut self, size: usize) -> bool {
        let mut progressed = false;
        for unit in all_units() {
            let empties: Vec<usize> = unit
                .iter()
                .copied()
                .filter(|&i| self.cells[i] == 0)
                .collect();
            let pool: Vec<usize> = empties
                .iter()
                .copied()
                .filter(|&i| {
                    let cnt = self.candidates[i].count_ones() as usize;
                    cnt >= 2 && cnt <= size
                })
                .collect();
            if pool.len() < size {
                continue;
            }

            for combo in combinations(&pool, size) {
                let mut union_mask = 0u16;
                for &i in &combo {
                    union_mask |= self.candidates[i];
                }
                if union_mask.count_ones() as usize != size {
                    continue;
                }

                for &i in &empties {
                    if !combo.contains(&i) {
                        let before = self.candidates[i];
                        self.candidates[i] &= !union_mask;
                        if self.candidates[i] != before {
                            progressed = true;
                        }
                    }
                }
            }
        }
        progressed
    }

    /// Hidden pair/triple/quad, generalized over `size`.
    fn apply_hidden_subsets(&mut self, size: usize) -> bool {
        let mut progressed = false;
        let digits: Vec<u8> = (1..=9).collect();
        let digit_combos = combinations(&digits, size);

        for unit in all_units() {
            let empties: Vec<usize> = unit
                .iter()
                .copied()
                .filter(|&i| self.cells[i] == 0)
                .collect();
            if empties.len() < size {
                continue;
            }

            for combo in &digit_combos {
                let mask: u16 = combo.iter().fold(0u16, |acc, &d| acc | (1u16 << d));
                let cells_with: Vec<usize> = empties
                    .iter()
                    .copied()
                    .filter(|&i| self.candidates[i] & mask != 0)
                    .collect();
                if cells_with.len() != size {
                    continue;
                }

                for &i in &cells_with {
                    let before = self.candidates[i];
                    self.candidates[i] &= mask;
                    if self.candidates[i] != before {
                        progressed = true;
                    }
                }
            }
        }
        progressed
    }

    /// Basic (non-finned) fish, generalized over `size` (2=X-Wing,
    /// 3=Swordfish, 4=Jellyfish), checked in both row->col and col->row
    /// directions.
    fn apply_fish(&mut self, size: usize) -> bool {
        let mut progressed = false;
        for d in 1..=9u8 {
            let bit = 1u16 << d;

            let row_positions: Vec<Vec<usize>> = (0..9)
                .map(|r| {
                    (0..9)
                        .filter(|&c| {
                            let i = r * 9 + c;
                            self.cells[i] == 0 && self.candidates[i] & bit != 0
                        })
                        .collect()
                })
                .collect();
            let candidate_rows: Vec<usize> = (0..9)
                .filter(|&r| {
                    let n = row_positions[r].len();
                    n >= 1 && n <= size
                })
                .collect();
            if candidate_rows.len() >= size {
                for combo in combinations(&candidate_rows, size) {
                    let mut col_union: HashSet<usize> = HashSet::new();
                    for &r in &combo {
                        for &c in &row_positions[r] {
                            col_union.insert(c);
                        }
                    }
                    if col_union.len() == size {
                        for &c in &col_union {
                            for r in 0..9 {
                                if !combo.contains(&r) {
                                    let i = r * 9 + c;
                                    if self.cells[i] == 0 && self.eliminate(i, d) {
                                        progressed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let col_positions: Vec<Vec<usize>> = (0..9)
                .map(|c| {
                    (0..9)
                        .filter(|&r| {
                            let i = r * 9 + c;
                            self.cells[i] == 0 && self.candidates[i] & bit != 0
                        })
                        .collect()
                })
                .collect();
            let candidate_cols: Vec<usize> = (0..9)
                .filter(|&c| {
                    let n = col_positions[c].len();
                    n >= 1 && n <= size
                })
                .collect();
            if candidate_cols.len() >= size {
                for combo in combinations(&candidate_cols, size) {
                    let mut row_union: HashSet<usize> = HashSet::new();
                    for &c in &combo {
                        for &r in &col_positions[c] {
                            row_union.insert(r);
                        }
                    }
                    if row_union.len() == size {
                        for &r in &row_union {
                            for c in 0..9 {
                                if !combo.contains(&c) {
                                    let i = r * 9 + c;
                                    if self.cells[i] == 0 && self.eliminate(i, d) {
                                        progressed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        progressed
    }

    /// Single-digit chains built from strong links (conjugate pairs). This
    /// one function covers Skyscraper and Two-String Kite (both are just
    /// 2-link chains) as well as general Simple Coloring for longer chains:
    /// same underlying mechanism at different chain lengths, so there's no
    /// need to special-case the short ones.
    fn apply_single_digit_chains(&mut self) -> bool {
        let mut progressed = false;
        for d in 1..=9u8 {
            let bit = 1u16 << d;
            let mut links: Vec<(usize, usize)> = Vec::new();
            for unit in all_units() {
                let spots: Vec<usize> = unit
                    .iter()
                    .copied()
                    .filter(|&i| self.cells[i] == 0 && self.candidates[i] & bit != 0)
                    .collect();
                if spots.len() == 2 {
                    links.push((spots[0], spots[1]));
                }
            }
            if links.len() < 2 {
                continue;
            }

            let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
            for &(a, b) in &links {
                adj.entry(a).or_default().push(b);
                adj.entry(b).or_default().push(a);
            }

            let mut visited: HashSet<usize> = HashSet::new();
            let all_nodes: Vec<usize> = adj.keys().copied().collect();

            for &start in &all_nodes {
                if visited.contains(&start) {
                    continue;
                }

                let mut color: HashMap<usize, bool> = HashMap::new();
                let mut stack = vec![start];
                color.insert(start, true);
                visited.insert(start);
                while let Some(cur) = stack.pop() {
                    let cur_color = color[&cur];
                    if let Some(neighbors) = adj.get(&cur) {
                        for &n in neighbors {
                            if !color.contains_key(&n) {
                                color.insert(n, !cur_color);
                                visited.insert(n);
                                stack.push(n);
                            }
                        }
                    }
                }
                if color.len() < 3 {
                    continue;
                } // a bare 2-node link has no extra deductions

                let true_nodes: Vec<usize> =
                    color.iter().filter(|&(_, &c)| c).map(|(&i, _)| i).collect();
                let false_nodes: Vec<usize> = color
                    .iter()
                    .filter(|&(_, &c)| !c)
                    .map(|(&i, _)| i)
                    .collect();

                // Rule 2: two same-colored nodes seeing each other -> that
                // whole color is impossible.
                let true_dead = has_peer_pair(&true_nodes);
                let false_dead = has_peer_pair(&false_nodes);
                if true_dead {
                    for &i in &true_nodes {
                        if self.eliminate(i, d) {
                            progressed = true;
                        }
                    }
                }
                if false_dead {
                    for &i in &false_nodes {
                        if self.eliminate(i, d) {
                            progressed = true;
                        }
                    }
                }
                if true_dead || false_dead {
                    continue;
                }

                // Rule 4: an outside cell seeing both a true- and a
                // false-colored node can't be this digit either way.
                for idx in 0..CELLS {
                    if self.cells[idx] != 0 || self.candidates[idx] & bit == 0 {
                        continue;
                    }
                    if color.contains_key(&idx) {
                        continue;
                    }
                    let sees_true = true_nodes.iter().any(|&t| is_peer(idx, t));
                    let sees_false = false_nodes.iter().any(|&f| is_peer(idx, f));
                    if sees_true && sees_false && self.eliminate(idx, d) {
                        progressed = true;
                    }
                }
            }
        }
        progressed
    }

    fn apply_xy_wing(&mut self) -> bool {
        let mut progressed = false;
        let bivalue: Vec<usize> = (0..CELLS)
            .filter(|&i| self.cells[i] == 0 && self.candidates[i].count_ones() == 2)
            .collect();

        for &pivot in &bivalue {
            let pivot_digits = bits(self.candidates[pivot]);
            let (x, y) = (pivot_digits[0], pivot_digits[1]);
            let pivot_peers: Vec<usize> = peers_of(pivot)
                .iter()
                .copied()
                .filter(|p| bivalue.contains(p))
                .collect();

            for &wing1 in &pivot_peers {
                let w1_digits = bits(self.candidates[wing1]);
                let shared: Vec<u8> = w1_digits
                    .iter()
                    .copied()
                    .filter(|d| *d == x || *d == y)
                    .collect();
                if shared.len() != 1 {
                    continue;
                }
                let shared_digit = shared[0];
                let z = match w1_digits.iter().copied().find(|&d| d != shared_digit) {
                    Some(z) => z,
                    None => continue,
                };
                let other_pivot_digit = if shared_digit == x { y } else { x };

                for &wing2 in &pivot_peers {
                    if wing2 == wing1 {
                        continue;
                    }
                    let w2_digits = bits(self.candidates[wing2]);
                    if w2_digits.len() == 2
                        && w2_digits.contains(&other_pivot_digit)
                        && w2_digits.contains(&z)
                    {
                        let peers2 = peers_of(wing2);
                        for &c in peers_of(wing1) {
                            if c != pivot
                                && peers2.contains(&c)
                                && self.cells[c] == 0
                                && self.eliminate(c, z)
                            {
                                progressed = true;
                            }
                        }
                    }
                }
            }
        }
        progressed
    }

    fn apply_xyz_wing(&mut self) -> bool {
        let mut progressed = false;
        let trivalue: Vec<usize> = (0..CELLS)
            .filter(|&i| self.cells[i] == 0 && self.candidates[i].count_ones() == 3)
            .collect();

        for &pivot in &trivalue {
            let pivot_mask = self.candidates[pivot];
            let bival_peers: Vec<usize> = peers_of(pivot)
                .iter()
                .copied()
                .filter(|&p| self.cells[p] == 0 && self.candidates[p].count_ones() == 2)
                .filter(|&p| self.candidates[p] & !pivot_mask == 0)
                .collect();

            for i in 0..bival_peers.len() {
                for j in (i + 1)..bival_peers.len() {
                    let (w1, w2) = (bival_peers[i], bival_peers[j]);
                    if self.candidates[w1] | self.candidates[w2] != pivot_mask {
                        continue;
                    }
                    let common_mask = self.candidates[w1] & self.candidates[w2];
                    if common_mask.count_ones() != 1 {
                        continue;
                    }
                    let z = bits(common_mask)[0];

                    for c in 0..CELLS {
                        if c == pivot || c == w1 || c == w2 {
                            continue;
                        }
                        if self.cells[c] != 0 {
                            continue;
                        }
                        if is_peer(c, pivot)
                            && is_peer(c, w1)
                            && is_peer(c, w2)
                            && self.eliminate(c, z)
                        {
                            progressed = true;
                        }
                    }
                }
            }
        }
        progressed
    }

    fn apply_unique_rectangle_type1(&mut self) -> bool {
        let mut progressed = false;
        for r1 in 0..9 {
            for r2 in (r1 + 1)..9 {
                for c1 in 0..9 {
                    for c2 in (c1 + 1)..9 {
                        let cells4 = [r1 * 9 + c1, r1 * 9 + c2, r2 * 9 + c1, r2 * 9 + c2];
                        if cells4.iter().any(|&i| self.cells[i] != 0) {
                            continue;
                        }
                        let boxes: HashSet<usize> = cells4.iter().map(|&i| box_of(i)).collect();
                        if boxes.len() != 2 {
                            continue;
                        }

                        for extra_idx in 0..4 {
                            let extra = cells4[extra_idx];
                            let floor: Vec<usize> = (0..4)
                                .filter(|&j| j != extra_idx)
                                .map(|j| cells4[j])
                                .collect();
                            let (m0, m1, m2) = (
                                self.candidates[floor[0]],
                                self.candidates[floor[1]],
                                self.candidates[floor[2]],
                            );
                            if m0.count_ones() == 2 && m0 == m1 && m0 == m2 {
                                let extra_mask = self.candidates[extra];
                                if extra_mask & m0 == m0 && extra_mask != m0 {
                                    let before = self.candidates[extra];
                                    self.candidates[extra] &= !m0;
                                    if self.candidates[extra] != before {
                                        progressed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        progressed
    }

    fn apply_w_wing(&mut self) -> bool {
        let mut progressed = false;
        let bivalue: Vec<usize> = (0..CELLS)
            .filter(|&i| self.cells[i] == 0 && self.candidates[i].count_ones() == 2)
            .collect();

        let mut strong_links: HashMap<u8, Vec<(usize, usize)>> = HashMap::new();
        for d in 1..=9u8 {
            let bit = 1u16 << d;
            let mut links = Vec::new();
            for unit in all_units() {
                let spots: Vec<usize> = unit
                    .iter()
                    .copied()
                    .filter(|&i| self.cells[i] == 0 && self.candidates[i] & bit != 0)
                    .collect();
                if spots.len() == 2 {
                    links.push((spots[0], spots[1]));
                }
            }
            strong_links.insert(d, links);
        }

        for i in 0..bivalue.len() {
            for j in (i + 1)..bivalue.len() {
                let (a, b) = (bivalue[i], bivalue[j]);
                if self.candidates[a] != self.candidates[b] {
                    continue;
                }
                if is_peer(a, b) {
                    continue;
                }
                let digits = bits(self.candidates[a]);
                let (x, y) = (digits[0], digits[1]);

                for &(cand_x, cand_y) in &[(x, y), (y, x)] {
                    if let Some(links) = strong_links.get(&cand_y) {
                        for &(p, q) in links {
                            if p == a || p == b || q == a || q == b {
                                continue;
                            }
                            let matches = (is_peer(p, a) && is_peer(q, b))
                                || (is_peer(q, a) && is_peer(p, b));
                            if matches {
                                for c in 0..CELLS {
                                    if c == a || c == b {
                                        continue;
                                    }
                                    if self.cells[c] != 0 {
                                        continue;
                                    }
                                    if is_peer(c, a) && is_peer(c, b) && self.eliminate(c, cand_x) {
                                        progressed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        progressed
    }

    /// Bounded "what if" bifurcation: pick the emptiest-but-still-ambiguous
    /// cell, trial each of its remaining candidates, and run the technique
    /// list (with one fewer level of bifurcation budget) on each trial. If
    /// only one candidate survives without hitting a contradiction, it must
    /// be correct; if some (but not all) survive, we can at least eliminate
    /// the dead ones. This is what "forcing chains" amount to in practice,
    /// without trying to name and detect every individual chain shape.
    fn apply_bifurcation(&mut self, depth_budget: u8) -> bool {
        if depth_budget == 0 {
            return false;
        }

        let mut best: Option<usize> = None;
        for i in 0..CELLS {
            if self.cells[i] == 0 {
                let n = self.candidates[i].count_ones();
                if n >= 2 {
                    let better = match best {
                        None => true,
                        Some(b) => n < self.candidates[b].count_ones(),
                    };
                    if better {
                        best = Some(i);
                    }
                }
            }
        }
        let idx = match best {
            Some(i) => i,
            None => return false,
        };

        let digits = bits(self.candidates[idx]);
        let mut surviving: Vec<u8> = Vec::new();
        for &d in &digits {
            let mut trial = self.clone();
            trial.place(idx, d);
            if trial.run_contradiction_check(depth_budget - 1) {
                surviving.push(d);
            }
        }

        if surviving.is_empty() {
            // Every branch contradicted - the puzzle's underlying grid data
            // must be inconsistent with our candidate bookkeeping somewhere;
            // bail out without making a (wrong) change.
            return false;
        }
        if surviving.len() == 1 {
            self.place(idx, surviving[0]);
            return true;
        }
        if surviving.len() < digits.len() {
            let survive_mask: u16 = surviving.iter().fold(0u16, |acc, &d| acc | (1u16 << d));
            let before = self.candidates[idx];
            self.candidates[idx] &= survive_mask;
            return self.candidates[idx] != before;
        }
        false
    }

    /// Runs techniques to a fixpoint (recursing into further bifurcation if
    /// budget remains and it gets stuck) and reports whether this branch
    /// still looks viable: `false` only when it hits a definite contradiction
    /// (some cell left with zero candidates); `true` if solved, or if we
    /// simply can't prove a contradiction within the depth budget (the safe
    /// default - we never want to falsely eliminate a valid branch).
    ///
    /// Deliberately uses only the *cheap* technique subset (singles, locked
    /// candidates, pairs) rather than the full arsenal: bifurcation trials
    /// only need to answer "does this immediately blow up", and real forcing-
    /// chain contradictions almost always show up through plain constraint
    /// propagation. Running the expensive pattern-matching techniques (wings,
    /// unique rectangles, fish) inside every trial of every trial was the
    /// main cost driver during generation - this cut it dramatically without
    /// changing what gets graded (grading itself still uses the full list).
    fn run_contradiction_check(&mut self, mut depth_budget: u8) -> bool {
        loop {
            let progressed = self.apply_cheap_techniques();
            if self.has_contradiction() {
                return false;
            }
            if self.is_solved() {
                return true;
            }
            if progressed {
                continue;
            }
            if depth_budget == 0 {
                return true;
            }
            depth_budget -= 1;
            if !self.apply_bifurcation(depth_budget) {
                return true;
            }
        }
    }

    /// Fast propagation subset used only inside bifurcation trials - see
    /// `run_contradiction_check`.
    fn apply_cheap_techniques(&mut self) -> bool {
        self.apply_naked_singles()
            || self.apply_hidden_singles()
            || self.apply_locked_candidates()
            || self.apply_naked_subsets(2)
            || self.apply_hidden_subsets(2)
    }

    /// Full grading pass: same "easiest first" loop as `run_contradiction_check`,
    /// but tracks which tier each technique that actually fired belongs to,
    /// so we come out with the hardest tier the grid required.
    /// `cap`, when set, lets grading bail out the moment the puzzle is
    /// confirmed harder than `cap`, skipping the remaining (pricier)
    /// techniques entirely. Used by the digger below, which only cares
    /// whether a candidate removal stayed at-or-under its target tier, not
    /// its exact rating once it's already blown past that - this is what
    /// keeps digging affordable even though it re-rates after every
    /// candidate cell removal.
    fn solve_and_grade(&mut self, cap: Option<Difficulty>) -> Difficulty {
        let mut hardest = Difficulty::Easy;
        macro_rules! bump {
            ($tier:expr) => {{
                hardest = hardest.max($tier);
                if let Some(c) = cap {
                    if hardest > c {
                        return hardest;
                    }
                }
            }};
        }
        for _ in 0..MAX_GRADE_ITERATIONS {
            if self.apply_naked_singles() || self.apply_hidden_singles() {
                continue;
            }
            if self.apply_locked_candidates() {
                bump!(Difficulty::Medium);
                continue;
            }
            if self.apply_naked_subsets(2) || self.apply_hidden_subsets(2) {
                bump!(Difficulty::Hard);
                continue;
            }
            if self.apply_naked_subsets(3)
                || self.apply_hidden_subsets(3)
                || self.apply_naked_subsets(4)
                || self.apply_hidden_subsets(4)
            {
                bump!(Difficulty::Expert);
                continue;
            }
            if self.apply_fish(2) || self.apply_single_digit_chains() {
                bump!(Difficulty::Master);
                continue;
            }
            if self.apply_fish(3) {
                bump!(Difficulty::Extreme);
                continue;
            }
            if self.apply_xy_wing() || self.apply_xyz_wing() || self.apply_unique_rectangle_type1()
            {
                bump!(Difficulty::Evil);
                continue;
            }
            if self.apply_w_wing() || self.apply_fish(4) {
                bump!(Difficulty::Diabolical);
                continue;
            }
            if self.is_solved() {
                return hardest;
            }
            if self.apply_bifurcation(1) {
                bump!(Difficulty::Grandmaster);
                continue;
            }
            if self.apply_bifurcation(2) {
                bump!(Difficulty::Ultimate);
                continue;
            }
            return hardest.max(Difficulty::Ultimate);
        }
        // Hit the safety iteration cap without resolving - report the
        // hardest tier confirmed so far rather than hanging.
        hardest.max(Difficulty::Ultimate)
    }
}

/// Rates a puzzle by the hardest human technique required to solve it.
#[allow(dead_code)]
pub fn rate_difficulty(board: &Board) -> Difficulty {
    let mut solver = Solver::from_board(board);
    solver.solve_and_grade(None)
}

/// Same as `rate_difficulty`, but stops early once the puzzle is confirmed
/// harder than `cap` - used by the digger in `engine.rs` so it doesn't pay
/// for full grading on every rejected candidate removal.
pub fn rate_difficulty_capped(board: &Board, cap: Difficulty) -> Difficulty {
    let mut solver = Solver::from_board(board);
    solver.solve_and_grade(Some(cap))
}
