//! Bounded capture of a command's output: keeps its start and its end.

use std::collections::VecDeque;

/// Collects output up to a byte limit. Past the limit it keeps the first half and the
/// last half (where a build's errors usually are) and counts what fell in between.
pub struct Capture {
    /// 0 means unlimited.
    limit: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted: u64,
}

impl Capture {
    pub fn new(limit: usize) -> Capture {
        Capture { limit, head: Vec::new(), tail: VecDeque::new(), omitted: 0 }
    }

    pub fn push(&mut self, mut data: &[u8]) {
        if self.limit == 0 {
            self.head.extend_from_slice(data);
            return;
        }
        let head_cap = self.limit / 2;
        if self.head.len() < head_cap {
            let n = (head_cap - self.head.len()).min(data.len());
            self.head.extend_from_slice(&data[..n]);
            data = &data[n..];
        }
        let tail_cap = self.limit - head_cap;
        if data.len() >= tail_cap {
            self.omitted += (self.tail.len() + data.len() - tail_cap) as u64;
            self.tail.clear();
            self.tail.extend(&data[data.len() - tail_cap..]);
        } else {
            let overflow = (self.tail.len() + data.len()).saturating_sub(tail_cap);
            self.tail.drain(..overflow);
            self.omitted += overflow as u64;
            self.tail.extend(data);
        }
    }

    /// The captured bytes. When anything was dropped, both halves are cut back to whole
    /// lines (so the cut never splits a character or an escape sequence) and a
    /// `[recover: N bytes omitted]` line marks the gap.
    pub fn finish(self) -> Vec<u8> {
        let Capture { mut head, tail, mut omitted, .. } = self;
        let mut tail = Vec::from(tail);
        if omitted == 0 {
            head.extend_from_slice(&tail);
            return head;
        }
        if let Some(i) = head.iter().rposition(|&b| b == b'\n') {
            omitted += (head.len() - (i + 1)) as u64;
            head.truncate(i + 1);
        }
        if let Some(i) = tail.iter().position(|&b| b == b'\n') {
            omitted += (i + 1) as u64;
            tail.drain(..=i);
        }
        if !head.is_empty() && !head.ends_with(b"\n") {
            head.push(b'\n');
        }
        head.extend_from_slice(format!("[recover: {omitted} bytes omitted]\n").as_bytes());
        head.extend_from_slice(&tail);
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(range: std::ops::RangeInclusive<u32>) -> Vec<u8> {
        range.map(|i| format!("{i}\n")).collect::<String>().into_bytes()
    }

    fn capture(limit: usize, chunks: &[&[u8]]) -> Vec<u8> {
        let mut c = Capture::new(limit);
        for chunk in chunks {
            c.push(chunk);
        }
        c.finish()
    }

    #[test]
    fn output_within_the_limit_is_kept_whole() {
        let data = lines(1..=5); // 10 bytes
        assert_eq!(capture(10, &[&data]), data, "exactly at the limit");
        assert_eq!(capture(100, &[&data[..3], &data[3..]]), data);
    }

    #[test]
    fn zero_means_unlimited() {
        let data = lines(1..=100_000);
        assert_eq!(capture(0, &[&data]), data);
    }

    #[test]
    fn long_output_keeps_the_first_and_last_lines_around_a_marker() {
        let data = lines(1..=1000);
        let got = String::from_utf8(capture(40, &[&data])).unwrap();
        let (head, rest) = got.split_once("[recover: ").expect("marker");
        let (marker, tail) = rest.split_once(" bytes omitted]\n").expect("marker end");
        assert!(head.starts_with("1\n2\n3\n"), "head: {head:?}");
        assert!(head.ends_with('\n') && tail.ends_with("999\n1000\n"), "tail: {tail:?}");
        let kept = head.len() + tail.len();
        assert_eq!(marker.parse::<usize>().unwrap(), data.len() - kept, "omitted count");
        assert!(kept <= 40);
    }

    #[test]
    fn the_cut_falls_on_line_boundaries() {
        let data = lines(1..=1000);
        let got = String::from_utf8(capture(41, &[&data])).unwrap();
        for line in got.lines() {
            assert!(
                line.starts_with("[recover: ") || line.parse::<u32>().is_ok(),
                "partial line {line:?} in {got:?}"
            );
        }
    }

    #[test]
    fn chunking_does_not_change_the_result() {
        let data = lines(1..=500);
        let whole = capture(64, &[&data]);
        let bytes: Vec<&[u8]> = data.chunks(1).collect();
        assert_eq!(capture(64, &bytes), whole, "byte by byte");
        let odd: Vec<&[u8]> = data.chunks(7).collect();
        assert_eq!(capture(64, &odd), whole, "7-byte chunks");
    }
}
