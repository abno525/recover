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

    /// The captured bytes. When anything was dropped, a `[recover: N bytes omitted]` line
    /// marks the gap, and both halves are cut back to whole lines, so the cut never splits
    /// a character or an escape sequence. Output without newlines (binary data, progress
    /// bars) is instead cut at a character boundary clear of any escape sequence.
    pub fn finish(self) -> Vec<u8> {
        let Capture { mut head, tail, mut omitted, .. } = self;
        let mut tail = Vec::from(tail);
        if omitted == 0 {
            head.extend_from_slice(&tail);
            return head;
        }
        let head_end = match head.iter().rposition(|&b| b == b'\n') {
            Some(i) => i + 1,
            None => clean_end(&head),
        };
        omitted += (head.len() - head_end) as u64;
        head.truncate(head_end);
        let tail_start = match tail.iter().position(|&b| b == b'\n') {
            Some(i) => i + 1,
            None => clean_start(&tail),
        };
        omitted += tail_start as u64;
        tail.drain(..tail_start);
        if !head.is_empty() && !head.ends_with(b"\n") {
            head.push(b'\n');
        }
        head.extend_from_slice(format!("[recover: {omitted} bytes omitted]\n").as_bytes());
        head.extend_from_slice(&tail);
        head
    }
}

/// How far back an unfinished escape sequence is looked for at a cut.
const ESCAPE_WINDOW: usize = 32;

/// Where to end `b` so it stops neither inside an escape sequence nor inside a character.
fn clean_end(b: &[u8]) -> usize {
    let from = b.len().saturating_sub(ESCAPE_WINDOW);
    let mut end = b.len();
    if let Some(esc) = b[from..].iter().rposition(|&c| c == 0x1b).map(|i| from + i)
        && !escape_finished(&b[esc..])
    {
        end = esc;
    }
    char_floor(&b[..end])
}

/// Where to start `b` so it begins neither inside an escape sequence nor inside a
/// character: at an escape that shows up early (the bytes before it may be the end of a
/// cut one), else at the first character start.
fn clean_start(b: &[u8]) -> usize {
    let window = &b[..b.len().min(ESCAPE_WINDOW)];
    match window.iter().position(|&c| c == 0x1b) {
        Some(esc) => esc,
        None => b.iter().position(|&c| c & 0xC0 != 0x80).unwrap_or(b.len()),
    }
}

/// Whether the escape sequence at the start of `seq` (which begins with ESC) is complete.
fn escape_finished(seq: &[u8]) -> bool {
    match seq.get(1) {
        None => false,
        // CSI: parameters, then a final byte in 0x40..=0x7E.
        Some(b'[') => seq[2..].iter().any(|c| (0x40..=0x7e).contains(c)),
        // OSC: ends with BEL or ESC \.
        Some(b']') => seq.contains(&0x07) || seq.windows(2).any(|w| w == b"\x1b\\"),
        Some(_) => true,
    }
}

/// The length of `b` without a trailing incomplete UTF-8 character.
fn char_floor(b: &[u8]) -> usize {
    let n = b.len();
    let mut lead = n;
    while lead > 0 && n - lead < 4 && b[lead - 1] & 0xC0 == 0x80 {
        lead -= 1;
    }
    let Some(lead) = lead.checked_sub(1) else { return n };
    let width = match b[lead] {
        0xF0.. => 4,
        0xE0.. => 3,
        0xC0.. => 2,
        _ => 1,
    };
    if n - lead >= width { n } else { lead }
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
    fn a_cut_without_newlines_never_splits_a_character() {
        let data = "aé".repeat(200).into_bytes(); // 1- and 2-byte characters, no newlines
        for limit in 4..60 {
            let got = capture(limit, &[&data]);
            assert!(String::from_utf8(got.clone()).is_ok(), "limit {limit}: {got:?}");
        }
    }

    #[test]
    fn a_cut_without_newlines_never_splits_an_escape_sequence() {
        let data = "\x1b[31mred\x1b[0m ".repeat(50).into_bytes(); // colored words, no newlines
        for limit in 16..80 {
            let shown = crate::text::decode_lossy_stripped(&capture(limit, &[&data]));
            assert!(shown.contains("[recover: ") && shown.contains(" bytes omitted]\n"), "limit {limit}: {shown:?}");
            let text: String = shown.lines().filter(|l| !l.starts_with("[recover: ")).collect();
            assert!(text.chars().all(|c| "red ".contains(c)), "limit {limit}: fragments in {shown:?}");
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
