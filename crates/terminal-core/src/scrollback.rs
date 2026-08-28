//! Scrollback: the bounded store of frozen [`LogicalLine`]s above the screen.
//!
//! PRD §17.5 #22: a dual cap. A logical line is unbounded, so a line-*count*
//! limit alone is not a memory bound (one `cat` of a newline-free megabyte would
//! blow it). So there are two limits — a line count (default 10k, hard max 100k
//! per §13) and a total-bytes safety cap — and eviction removes whole oldest
//! lines from the front until under both. Lines are never split or truncated.
//!
//! Lines carry monotonic ids (allocated by the owner, not here), so they are
//! stored in ascending id order and an anchor into an evicted line is detectably
//! stale rather than silently wrong (§16.2).

use std::collections::VecDeque;

use crate::logical_line::{LineId, LogicalLine};

pub const DEFAULT_MAX_LINES: usize = 10_000;
pub const HARD_MAX_LINES: usize = 100_000;
/// Total packed-text bytes across scrollback. A safety net for pathological
/// long lines; normal history stays orders of magnitude below it.
pub const DEFAULT_MAX_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Scrollback {
    lines: VecDeque<LogicalLine>,
    max_lines: usize,
    max_bytes: usize,
    total_bytes: usize,
}

impl Scrollback {
    /// A scrollback bounded by `max_lines` (clamped to [`HARD_MAX_LINES`]) and
    /// `max_bytes`.
    pub fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            max_lines: max_lines.min(HARD_MAX_LINES),
            max_bytes,
            total_bytes: 0,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES)
    }

    /// Append a frozen line at the newest end, then evict oldest lines until
    /// both caps are satisfied.
    pub fn push(&mut self, line: LogicalLine) {
        self.total_bytes += line.byte_len() as usize;
        self.lines.push_back(line);
        self.evict();
    }

    fn evict(&mut self) {
        // Line count is a hard cap.
        while self.lines.len() > self.max_lines {
            self.pop_oldest();
        }
        // Bytes cap keeps at least one line, so the most recent line is never
        // evicted just for being large.
        while self.total_bytes > self.max_bytes && self.lines.len() > 1 {
            self.pop_oldest();
        }
    }

    fn pop_oldest(&mut self) {
        if let Some(evicted) = self.lines.pop_front() {
            self.total_bytes -= evicted.byte_len() as usize;
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn max_lines(&self) -> usize {
        self.max_lines
    }

    /// The oldest still-retained line's id, or `None` if empty. An anchor whose
    /// id is below this has been evicted.
    pub fn oldest_id(&self) -> Option<LineId> {
        self.lines.front().map(LogicalLine::id)
    }

    /// The line with the given id, or `None` if it was never present or has been
    /// evicted. Lines are in ascending id order, so this is a binary search.
    pub fn get(&self, id: LineId) -> Option<&LogicalLine> {
        let idx = self.lines.binary_search_by(|l| l.id().cmp(&id)).ok()?;
        self.lines.get(idx)
    }

    /// Oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &LogicalLine> {
        self.lines.iter()
    }

    /// Oldest to newest, mutable — for the maintenance thread refreshing wrap
    /// caches (§16.5).
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut LogicalLine> {
        self.lines.iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;

    fn line(id: u64, s: &str) -> LogicalLine {
        let cells: Vec<Cell> = s
            .chars()
            .map(|c| Cell {
                content: c.to_string().into(),
                ..Cell::blank()
            })
            .collect();
        LogicalLine::from_cells(LineId(id), &cells)
    }

    #[test]
    fn push_tracks_len_and_bytes() {
        let mut sb = Scrollback::with_defaults();
        sb.push(line(0, "abc"));
        sb.push(line(1, "de"));
        assert_eq!(sb.len(), 2);
        assert_eq!(sb.total_bytes(), 5);
        assert!(!sb.is_empty());
    }

    #[test]
    fn line_count_cap_evicts_oldest() {
        let mut sb = Scrollback::new(3, DEFAULT_MAX_BYTES);
        for i in 0..5 {
            sb.push(line(i, "x"));
        }
        assert_eq!(sb.len(), 3);
        // The two oldest (ids 0,1) were evicted; 2..4 remain.
        assert_eq!(sb.oldest_id(), Some(LineId(2)));
        let ids: Vec<u64> = sb.iter().map(|l| l.id().0).collect();
        assert_eq!(ids, [2, 3, 4]);
    }

    #[test]
    fn byte_cap_evicts_but_keeps_at_least_one() {
        // Cap at 4 bytes; push three 3-byte lines.
        let mut sb = Scrollback::new(HARD_MAX_LINES, 4);
        sb.push(line(0, "aaa"));
        sb.push(line(1, "bbb")); // total would be 6 > 4 -> evict id 0
        assert_eq!(sb.len(), 1);
        assert_eq!(sb.oldest_id(), Some(LineId(1)));
        // A single line larger than the cap is still kept (never evict the last).
        let mut sb = Scrollback::new(HARD_MAX_LINES, 2);
        sb.push(line(0, "hello"));
        assert_eq!(sb.len(), 1);
        assert_eq!(sb.total_bytes(), 5);
    }

    #[test]
    fn get_finds_present_and_misses_evicted() {
        let mut sb = Scrollback::new(2, DEFAULT_MAX_BYTES);
        for i in 0..4 {
            sb.push(line(i, "z"));
        }
        // 0,1 evicted; 2,3 present.
        assert!(sb.get(LineId(1)).is_none());
        assert_eq!(sb.get(LineId(2)).map(|l| l.id()), Some(LineId(2)));
        assert!(sb.get(LineId(9)).is_none());
    }

    #[test]
    fn max_lines_is_clamped_to_hard_max() {
        let sb = Scrollback::new(HARD_MAX_LINES * 10, DEFAULT_MAX_BYTES);
        assert_eq!(sb.max_lines(), HARD_MAX_LINES);
    }
}
