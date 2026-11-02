//! Port of the parts of Python's `difflib.SequenceMatcher` that `fm-assignment-reassess.py` uses:
//! `SequenceMatcher(None, a, b, autojunk=False).get_opcodes()`. With no junk predicate and the
//! popularity heuristic disabled, the matcher is fully determined by `find_longest_match`, the
//! explicit queue of `get_matching_blocks`, its sort and adjacent-block collapse, and `get_opcodes`.

use std::collections::HashMap;
use std::hash::Hash;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Replace,
    Delete,
    Insert,
    Equal,
}

/// One `(tag, i1, i2, j1, j2)` opcode: `a[i1:i2]` and `b[j1:j2]` relate by `tag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opcode {
    pub tag: Tag,
    pub i1: usize,
    pub i2: usize,
    pub j1: usize,
    pub j2: usize,
}

/// Returns the opcodes that transform `a` into `b`, in order, as `get_opcodes` does.
pub fn get_opcodes<T: Eq + Hash>(a: &[T], b: &[T]) -> Vec<Opcode> {
    let matcher = Matcher::new(a, b);
    let mut opcodes = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (ai, bj, size) in matcher.matching_blocks() {
        let tag = if i < ai && j < bj {
            Some(Tag::Replace)
        } else if i < ai {
            Some(Tag::Delete)
        } else if j < bj {
            Some(Tag::Insert)
        } else {
            None
        };
        if let Some(tag) = tag {
            opcodes.push(Opcode {
                tag,
                i1: i,
                i2: ai,
                j1: j,
                j2: bj,
            });
        }
        i = ai + size;
        j = bj + size;
        if size > 0 {
            opcodes.push(Opcode {
                tag: Tag::Equal,
                i1: ai,
                i2: i,
                j1: bj,
                j2: j,
            });
        }
    }
    opcodes
}

struct Matcher<'a, T> {
    a: &'a [T],
    b: &'a [T],
    /// Positions of each element of `b`, ascending, as `__chain_b` builds them.
    b2j: HashMap<&'a T, Vec<usize>>,
}

impl<'a, T: Eq + Hash> Matcher<'a, T> {
    fn new(a: &'a [T], b: &'a [T]) -> Self {
        let mut b2j: HashMap<&'a T, Vec<usize>> = HashMap::new();
        for (index, element) in b.iter().enumerate() {
            b2j.entry(element).or_default().push(index);
        }
        Matcher { a, b, b2j }
    }

    /// `get_matching_blocks`: a LIFO queue of sub-ranges, sorted blocks, adjacent blocks merged,
    /// and the final `(len(a), len(b), 0)` sentinel.
    fn matching_blocks(&self) -> Vec<(usize, usize, usize)> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut blocks = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let (i, j, k) = self.find_longest_match(alo, ahi, blo, bhi);
            if k == 0 {
                continue;
            }
            blocks.push((i, j, k));
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
        blocks.sort_unstable();

        let mut non_adjacent = Vec::new();
        let (mut i1, mut j1, mut k1) = (0, 0, 0);
        for (i2, j2, k2) in blocks {
            if i1 + k1 == i2 && j1 + k1 == j2 {
                k1 += k2;
            } else {
                if k1 > 0 {
                    non_adjacent.push((i1, j1, k1));
                }
                (i1, j1, k1) = (i2, j2, k2);
            }
        }
        if k1 > 0 {
            non_adjacent.push((i1, j1, k1));
        }
        non_adjacent.push((la, lb, 0));
        non_adjacent
    }

    /// `find_longest_match` with no junk: the first longest block in scan order wins (`k > best`
    /// is strict, `i` ascends in the outer loop and `j` in the inner), then the block is extended
    /// over equal elements at both ends.
    fn find_longest_match(
        &self,
        alo: usize,
        ahi: usize,
        blo: usize,
        bhi: usize,
    ) -> (usize, usize, usize) {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for i in alo..ahi {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(positions) = self.b2j.get(&self.a[i]) {
                for &j in positions {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let previous = j
                        .checked_sub(1)
                        .and_then(|p| j2len.get(&p))
                        .copied()
                        .unwrap_or(0);
                    let k = previous + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }

        while besti > alo && bestj > blo && self.a[besti - 1] == self.b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && self.a[besti + bestsize] == self.b[bestj + bestsize]
        {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag_name(tag: Tag) -> &'static str {
        match tag {
            Tag::Replace => "replace",
            Tag::Delete => "delete",
            Tag::Insert => "insert",
            Tag::Equal => "equal",
        }
    }

    fn lines(text: &[&'static str]) -> Vec<&'static str> {
        text.to_vec()
    }

    fn ops(
        a: &[&'static str],
        b: &[&'static str],
    ) -> Vec<(&'static str, usize, usize, usize, usize)> {
        get_opcodes(&lines(a), &lines(b))
            .into_iter()
            .map(|op| (tag_name(op.tag), op.i1, op.i2, op.j1, op.j2))
            .collect()
    }

    // Python-derived. Every expectation in this module is the output of
    //   python3 -I -B /tmp/dpes884-prose/gen_opcodes.py
    // which runs `difflib.SequenceMatcher(None, a, b, autojunk=False).get_opcodes()` for each pair
    // (Python 3.14.6). The pairs are the same literals as below.

    #[test]
    fn identical_is_one_equal_block() {
        assert_eq!(
            ops(&["a", "b", "c"], &["a", "b", "c"]),
            vec![("equal", 0, 3, 0, 3)]
        );
    }

    #[test]
    fn both_empty_has_no_opcodes() {
        assert_eq!(ops(&[], &[]), vec![]);
    }

    #[test]
    fn insert_into_empty_is_one_insert() {
        assert_eq!(ops(&[], &["a", "b"]), vec![("insert", 0, 0, 0, 2)]);
    }

    #[test]
    fn delete_to_empty_is_one_delete() {
        assert_eq!(ops(&["a", "b"], &[]), vec![("delete", 0, 2, 0, 0)]);
    }

    #[test]
    fn insert_in_middle() {
        assert_eq!(
            ops(&["a", "c"], &["a", "b", "c"]),
            vec![
                ("equal", 0, 1, 0, 1),
                ("insert", 1, 1, 1, 2),
                ("equal", 1, 2, 2, 3)
            ]
        );
    }

    #[test]
    fn delete_in_middle() {
        assert_eq!(
            ops(&["a", "b", "c"], &["a", "c"]),
            vec![
                ("equal", 0, 1, 0, 1),
                ("delete", 1, 2, 1, 1),
                ("equal", 2, 3, 1, 2)
            ]
        );
    }

    #[test]
    fn replace_in_middle() {
        assert_eq!(
            ops(&["a", "b", "c"], &["a", "x", "c"]),
            vec![
                ("equal", 0, 1, 0, 1),
                ("replace", 1, 2, 1, 2),
                ("equal", 2, 3, 2, 3)
            ]
        );
    }

    #[test]
    fn repeated_lines_take_the_earliest_longest_block() {
        assert_eq!(
            ops(&["x", "x", "x", "y"], &["x", "x", "y", "x"]),
            vec![
                ("delete", 0, 1, 0, 0),
                ("equal", 1, 4, 0, 3),
                ("insert", 4, 4, 3, 4)
            ]
        );
    }

    #[test]
    fn transposition() {
        assert_eq!(
            ops(&["a", "b"], &["b", "a"]),
            vec![
                ("insert", 0, 0, 0, 1),
                ("equal", 0, 1, 1, 2),
                ("delete", 1, 2, 2, 2)
            ]
        );
    }

    #[test]
    fn near_duplicate_blocks() {
        assert_eq!(
            ops(
                &["a", "b", "c", "a", "b", "c"],
                &["a", "b", "x", "c", "a", "b", "c", "a"]
            ),
            vec![
                ("insert", 0, 0, 0, 4),
                ("equal", 0, 4, 4, 8),
                ("delete", 4, 6, 8, 8)
            ]
        );
    }

    #[test]
    fn all_different_is_one_replace() {
        assert_eq!(
            ops(&["a", "b"], &["c", "d", "e"]),
            vec![("replace", 0, 2, 0, 3)]
        );
    }

    #[test]
    fn one_line_same_and_changed() {
        assert_eq!(ops(&["a"], &["a"]), vec![("equal", 0, 1, 0, 1)]);
        assert_eq!(ops(&["a"], &["b"]), vec![("replace", 0, 1, 0, 1)]);
    }

    #[test]
    fn blank_lines_resolve_ties_by_first_longest_block() {
        assert_eq!(
            ops(&["a", "", "", "b"], &["", "b", "a"]),
            vec![
                ("delete", 0, 2, 0, 0),
                ("equal", 2, 4, 0, 2),
                ("insert", 4, 4, 2, 3)
            ]
        );
    }

    #[test]
    fn unicode_lines_compare_by_code_point_sequence() {
        assert_eq!(
            ops(&["é", "日本", "🙂"], &["é", "日本語", "🙂", "z"]),
            vec![
                ("equal", 0, 1, 0, 1),
                ("replace", 1, 2, 1, 2),
                ("equal", 2, 3, 2, 3),
                ("insert", 3, 3, 3, 4),
            ]
        );
    }

    #[test]
    fn popular_elements_are_not_dropped_without_autojunk() {
        let mut a = vec![""; 150];
        a.push("mark");
        a.extend(vec![""; 149]);
        let mut b = vec![""; 150];
        b.extend(vec![""; 3]);
        b.push("mark");
        b.extend(vec![""; 146]);
        let got: Vec<_> = get_opcodes(&a, &b)
            .into_iter()
            .map(|op| (tag_name(op.tag), op.i1, op.i2, op.j1, op.j2))
            .collect();
        assert_eq!(
            got,
            vec![
                ("insert", 0, 0, 0, 3),
                ("equal", 0, 297, 3, 300),
                ("delete", 297, 300, 300, 300)
            ]
        );
    }

    #[test]
    fn two_hundred_line_pair_with_deletions_and_insertions() {
        let a: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let mut b: Vec<String> = (0..200)
            .filter(|i| i % 37 != 5)
            .take(150)
            .map(|i| format!("line {i}"))
            .collect();
        b.extend((0..20).map(|i| format!("new {i}")));
        b.extend((150..200).map(|i| format!("line {i}")));
        let got: Vec<_> = get_opcodes(&a, &b)
            .into_iter()
            .map(|op| (tag_name(op.tag), op.i1, op.i2, op.j1, op.j2))
            .collect();
        assert_eq!(
            got,
            vec![
                ("equal", 0, 5, 0, 5),
                ("delete", 5, 6, 5, 5),
                ("equal", 6, 42, 5, 41),
                ("delete", 42, 43, 41, 41),
                ("equal", 43, 79, 41, 77),
                ("delete", 79, 80, 77, 77),
                ("equal", 80, 116, 77, 113),
                ("delete", 116, 117, 113, 113),
                ("equal", 117, 150, 113, 146),
                ("insert", 150, 150, 146, 170),
                ("equal", 150, 200, 170, 220),
            ]
        );
    }

    #[test]
    fn two_hundred_line_alternating_pair() {
        let a: Vec<&str> = std::iter::repeat_n(["", "x"], 100).flatten().collect();
        let b: Vec<&str> = std::iter::repeat_n(["x", ""], 100).flatten().collect();
        assert_eq!(
            ops(&a, &b),
            vec![
                ("insert", 0, 0, 0, 1),
                ("equal", 0, 199, 1, 200),
                ("delete", 199, 200, 200, 200)
            ]
        );
    }
}
