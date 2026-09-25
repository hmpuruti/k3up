use std::{cell::OnceCell, collections::VecDeque};

pub const MAX_LINES: usize = 2000;
pub const MAX_LINE_BYTES: usize = 16 * 1024;
pub const INITIAL_LINES: usize = 400;

#[derive(Default)]
pub struct LogBuffer {
    lines: VecDeque<String>,
    offset: Option<u64>,
    open_line: bool,
    joined: OnceCell<String>,
}

impl LogBuffer {
    pub fn offset(&self) -> Option<u64> {
        self.offset
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.offset = None;
        self.open_line = false;
        self.joined.take();
    }

    pub fn clear_view(&mut self) {
        self.lines.clear();
        self.open_line = false;
        self.joined.take();
    }

    pub fn append(&mut self, text: &str, offset: u64) {
        let initial = self.offset.is_none();
        self.offset = Some(offset);
        if text.is_empty() {
            return;
        }
        self.joined.take();
        // Progress output rewrites one line with bare carriage returns; show each update as a line.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let ends_with_newline = text.ends_with('\n');
        let body = if ends_with_newline {
            &text[..text.len() - 1]
        } else {
            &text
        };
        for (index, segment) in body.split('\n').enumerate() {
            match self.lines.back_mut() {
                Some(last) if index == 0 && self.open_line => {
                    let room = MAX_LINE_BYTES.saturating_sub(last.len());
                    last.push_str(prefix(segment, room));
                }
                _ => self
                    .lines
                    .push_back(prefix(segment, MAX_LINE_BYTES).to_owned()),
            }
        }
        // The initial tail is line-oriented and cannot tell whether the file ended mid-line.
        self.open_line = !initial && !ends_with_newline;
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    pub fn text(&self) -> &str {
        self.joined.get_or_init(|| {
            let mut out = String::new();
            for (index, line) in self.lines.iter().enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                out.push_str(line);
            }
            out
        })
    }
}

fn prefix(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_partial_lines_across_chunks() {
        let mut buffer = LogBuffer::default();
        buffer.append("first\nsecond", 12);
        buffer.append("third\nfour", 22);
        buffer.append("th\n", 25);
        buffer.append("fifth\n", 31);
        assert_eq!(buffer.text(), "first\nsecond\nthird\nfourth\nfifth");
        assert_eq!(buffer.offset(), Some(31));
    }

    #[test]
    fn bounds_lines_without_newlines() {
        let mut buffer = LogBuffer::default();
        buffer.append("seed\n", 5);
        for index in 0..200 {
            buffer.append(&"é".repeat(1000), 10 + index);
        }
        assert_eq!(buffer.len(), 2);
        assert!(buffer.text().len() <= "seed\n".len() + MAX_LINE_BYTES);
        buffer.append("\r10%\r20%\r\ndone\n", 500);
        assert!(buffer.text().ends_with("\n10%\n20%\ndone"));
    }

    #[test]
    fn keeps_a_bounded_window() {
        let mut buffer = LogBuffer::default();
        buffer.append("seed\n", 5);
        for index in 0..(MAX_LINES + 10) {
            buffer.append(&format!("line {index}\n"), 10 + index as u64);
        }
        assert_eq!(buffer.len(), MAX_LINES);
        assert!(buffer.text().starts_with("line 10\n"));
    }
}
