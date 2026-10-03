use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ropey::Rope;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
    pub dim: bool,
    pub heading: u8,
}
#[derive(Clone, Debug)]
pub struct Span {
    pub bytes: Range<usize>,
    pub style: Style,
}

/// The map is in characters; cosmic-text hits are UTF-8 byte indices.
#[derive(Default)]
pub struct Presentation {
    pub text: String,
    pub spans: Vec<Span>,
    pub source: Vec<(usize, usize)>,
    pub tasks: Vec<(usize, usize, bool)>,
    pub rules: Vec<usize>,
    pub quote: bool,
    pub start: usize,
    pub end: usize,
}
impl Presentation {
    pub fn source_at(&self, byte: usize) -> usize {
        let index = self
            .source
            .partition_point(|(b, _)| *b <= byte)
            .saturating_sub(1);
        self.source
            .get(index)
            .map_or(self.start, |(_, source)| *source)
    }
    pub fn byte_at(&self, source: usize) -> usize {
        let index = self.source.partition_point(|(_, c)| *c < source);
        self.source
            .get(index)
            .map_or(self.text.len(), |(byte, _)| *byte)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockLine {
    pub before: Option<(char, usize)>,
    pub after: Option<(char, usize)>,
    pub heading: Option<(u8, String)>,
    prefix: String,
}
#[derive(Clone, Default)]
pub struct BlockIndex {
    pub lines: im::Vector<BlockLine>,
    pub headings: im::OrdMap<usize, (u8, String)>,
    pub levels: [im::OrdMap<usize, String>; 6],
}
impl BlockIndex {
    /// Only prefixes affect fence state. A 10 MiB physical line is never flattened.
    pub fn update(&mut self, rope: &Rope, from_line: usize) {
        self.update_range(rope, from_line..from_line + 1);
    }
    pub fn update_range(&mut self, rope: &Rope, dirty: Range<usize>) {
        let count = rope.len_lines();
        let initial = self.lines.is_empty();
        let from = dirty.start.min(count.saturating_sub(1));
        let structural = count != self.lines.len();
        if structural && !initial {
            let delta = count as isize - self.lines.len() as isize;
            self.headings = self
                .headings
                .iter()
                .filter_map(|(&line, value)| {
                    if line < from {
                        Some((line, value.clone()))
                    } else if delta < 0 && line < from + (-delta as usize) + 1 {
                        None
                    } else {
                        Some(((line as isize + delta).max(0) as usize, value.clone()))
                    }
                })
                .collect();
            self.levels = std::array::from_fn(|i| {
                self.headings
                    .iter()
                    .filter(|(_, (level, _))| *level as usize == i + 1)
                    .map(|(&line, (_, name))| (line, name.clone()))
                    .collect()
            });
        }
        if self.lines.is_empty() {
            self.lines = std::iter::repeat_n(BlockLine::default(), count).collect();
        } else if structural {
            let position = (from + 1).min(self.lines.len());
            if count > self.lines.len() {
                let added = count - self.lines.len();
                for _ in 0..added {
                    self.lines.insert(position, BlockLine::default());
                }
            } else {
                let removed = self.lines.len() - count;
                for _ in 0..removed {
                    self.lines.remove(position.min(self.lines.len() - 1));
                }
            }
        }
        let mut state = if from == 0 {
            None
        } else {
            self.lines.get(from - 1).and_then(|l| l.after)
        };
        for line in from..count {
            let prefix: String = rope.line(line).chars().take(512).collect();
            let trimmed = prefix.trim_start_matches(' ');
            let indentation = prefix.len() - trimmed.len();
            let before = state;
            if indentation <= 3 {
                let fence = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
                if let Some(marker) = fence {
                    let length = trimmed.chars().take_while(|c| *c == marker).count();
                    if length >= 3 {
                        match state {
                            None => state = Some((marker, length)),
                            Some((open, size))
                                if open == marker
                                    && length >= size
                                    && trimmed[length..].trim().is_empty() =>
                            {
                                state = None
                            }
                            _ => {}
                        }
                    }
                }
            }
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            let heading = if before.is_none()
                && (1..=6).contains(&level)
                && trimmed.as_bytes().get(level) == Some(&b' ')
            {
                Some((
                    level as u8,
                    trimmed[level..]
                        .trim()
                        .trim_end_matches('#')
                        .trim()
                        .to_owned(),
                ))
            } else {
                None
            };
            let entry = BlockLine {
                before,
                after: state,
                heading,
                prefix,
            };
            if !initial && line >= dirty.end && self.lines.get(line) == Some(&entry) {
                break;
            }
            for level in &mut self.levels {
                level.remove(&line);
            }
            if let Some(heading) = &entry.heading {
                self.headings.insert(line, heading.clone());
                self.levels[heading.0 as usize - 1].insert(line, heading.1.clone());
            } else {
                self.headings.remove(&line);
            }
            self.lines.set(line, entry);
        }
    }
    pub fn in_code(&self, line: usize) -> bool {
        self.lines.get(line).is_some_and(|l| l.before.is_some())
    }
}

/// Parse a bounded viewport chunk. Raw source remains on the cursor's physical line.
pub fn present(
    source: &str,
    start: usize,
    cursor_line: Option<usize>,
    show_markers: bool,
    initial_code: bool,
) -> Presentation {
    let mut output = Presentation {
        start,
        end: start + source.chars().count(),
        ..Default::default()
    };
    let mut byte_offset = 0;
    let mut char_offset = start;
    let mut code = initial_code;
    for (line_i, line) in source.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        let is_code = code || fence;
        let raw = show_markers || cursor_line == Some(line_i);
        let mut styles = vec![
            Style {
                code: is_code,
                ..Default::default()
            };
            line.len()
        ];
        let mut keep = vec![is_code && !fence; line.len()];
        let mut task_ranges = Vec::new();
        if !is_code {
            let mut style = Style::default();
            for (event, range) in Parser::new_ext(
                line,
                Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
            )
            .into_offset_iter()
            {
                match event {
                    Event::Start(tag) => match tag {
                        Tag::Strong => style.bold = true,
                        Tag::Emphasis => style.italic = true,
                        Tag::Strikethrough => style.strike = true,
                        Tag::Heading { level, .. } => {
                            style.heading = level as u8;
                            style.bold = true;
                        }
                        Tag::Link { .. } => style.link = true,
                        Tag::BlockQuote(_) => {
                            style.dim = true;
                            output.quote = true;
                        }
                        _ => {}
                    },
                    Event::End(tag) => match tag {
                        TagEnd::Strong => style.bold = false,
                        TagEnd::Emphasis => style.italic = false,
                        TagEnd::Strikethrough => style.strike = false,
                        TagEnd::Heading(_) => {
                            style.heading = 0;
                            style.bold = false;
                        }
                        TagEnd::Link => style.link = false,
                        TagEnd::BlockQuote(_) => style.dim = false,
                        _ => {}
                    },
                    Event::Text(_) | Event::Html(_) | Event::InlineHtml(_) => {
                        for i in range {
                            if i < line.len() {
                                keep[i] = true;
                                styles[i] = style;
                            }
                        }
                    }
                    Event::Code(_) => {
                        let raw_code = &line[range.clone()];
                        let ticks = raw_code.chars().take_while(|c| *c == '`').count();
                        for i in range.start + ticks..range.end.saturating_sub(ticks) {
                            keep[i] = true;
                            styles[i] = Style {
                                code: true,
                                ..style
                            };
                        }
                    }
                    Event::TaskListMarker(checked) => {
                        task_ranges.push((range.clone(), checked));
                        output.tasks.push((
                            output.text.len(),
                            char_offset + line[..range.start].chars().count() + 1,
                            checked,
                        ));
                        for i in range {
                            if i < line.len() {
                                keep[i] = true;
                                styles[i] = Style { dim: true, ..style };
                            }
                        }
                    }
                    Event::Rule => output.rules.push(output.text.len()),
                    Event::SoftBreak | Event::HardBreak => {
                        for i in range {
                            if i < line.len() {
                                keep[i] = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Lists keep their bullets, numbering and indentation as layout cues.
            let leading = line.len() - trimmed.len();
            let list_prefix = if ["- ", "* ", "+ "].iter().any(|m| trimmed.starts_with(m)) {
                leading + 2
            } else {
                let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
                if digits > 0 && trimmed[digits..].starts_with(". ") {
                    leading + digits + 2
                } else {
                    leading
                }
            };
            keep[..list_prefix.min(line.len())].fill(true);
            // Tables deliberately remain tidy source in v1.
            if line.contains('|') {
                keep.fill(true);
                styles.fill(Style {
                    code: true,
                    ..Default::default()
                });
            }
        }
        if fence {
            code = !code;
        }
        for (index, character) in line.char_indices() {
            let end = index + character.len_utf8();
            let task = if raw {
                None
            } else {
                task_ranges.iter().find(|(range, _)| range.contains(&index))
            };
            let visible =
                raw || (task.is_none() && keep[index]) || matches!(character, '\r' | '\n');
            if let Some((range, checked)) = task {
                if index == range.start {
                    let position = output.text.len();
                    output.source.push((position, char_offset + 1));
                    output.text.push(if *checked { '☑' } else { '☐' });
                    output.text.push(' ');
                    output.spans.push(Span {
                        bytes: position..output.text.len(),
                        style: Style::default(),
                    });
                }
            }
            if visible {
                let position = output.text.len();
                output.source.push((position, char_offset));
                output.text.push(character);
                let mut style = styles[index];
                if raw && !keep[index] {
                    style.dim = true;
                }
                if let Some(span) = output
                    .spans
                    .last_mut()
                    .filter(|s| s.style == style && s.bytes.end == position)
                {
                    span.bytes.end = output.text.len();
                } else {
                    output.spans.push(Span {
                        bytes: position..output.text.len(),
                        style,
                    });
                }
            }
            char_offset += 1;
            byte_offset += end - index;
        }
    }
    let _ = byte_offset;
    output.source.push((output.text.len(), output.end));
    output
}

/// The link under character column `col` of a line: `[text](url)` or a bare http(s) address.
pub fn link_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let text = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == ']' && chars[i + 1] == '(' {
            let open = chars[..i].iter().rposition(|c| *c == '[');
            let close = chars[i + 2..].iter().position(|c| *c == ')').map(|p| p + i + 2);
            if let (Some(open), Some(close)) = (open, close) {
                if (open..=close).contains(&col) {
                    return Some(text(i + 2, close).trim().to_owned()).filter(|u| !u.is_empty());
                }
                i = close;
            }
        }
        i += 1;
    }
    let mut start = 0;
    while start < chars.len() {
        let rest = text(start, chars.len().min(start + 8));
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = (start..chars.len())
                .find(|&k| chars[k].is_whitespace() || matches!(chars[k], ')' | ']' | '>' | '"'))
                .unwrap_or(chars.len());
            if (start..end).contains(&col) || col == end {
                return Some(text(start, end));
            }
            start = end;
        }
        start += 1;
    }
    None
}

/// Formatting that applies at the cursor, used to light up toolbar buttons.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub struct Format {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub bullet: bool,
    pub numbered: bool,
    pub heading: u8,
}
/// `line` is the cursor's line (possibly a window of a very long one) and `col` the cursor column in it.
pub fn format_at(line: &str, col: usize) -> Format {
    let mut format = Format::default();
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        format.heading = hashes as u8;
    }
    let list = ["- ", "* ", "+ "].iter().any(|m| trimmed.starts_with(m));
    format.bullet = list;
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    format.numbered = digits > 0 && trimmed[digits..].starts_with(". ");
    let chars: Vec<char> = line.chars().collect();
    let mut i = line.len() - trimmed.len();
    if list {
        i += 1; // the list marker is not emphasis
    }
    while i < chars.len().min(col) {
        let two = chars.get(i + 1).map(|n| (chars[i], *n));
        match (chars[i], two) {
            ('\\', _) => i += 1,
            ('`', _) => format.code = !format.code,
            _ if format.code => {}
            ('*', Some(('*', '*'))) => {
                format.bold = !format.bold;
                i += 1;
            }
            ('~', Some(('~', '~'))) => {
                format.strike = !format.strike;
                i += 1;
            }
            ('*', _) => format.italic = !format.italic,
            _ => {}
        }
        i += 1;
    }
    format
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_and_formatting_are_found_at_the_cursor() {
        let line = "see [docs](https://a.b/c) and https://x.y now";
        assert_eq!(link_at(line, 6).as_deref(), Some("https://a.b/c"));
        assert_eq!(link_at(line, 35).as_deref(), Some("https://x.y"));
        assert_eq!(link_at(line, 1), None);
        let f = format_at("## a **bold *it* x** `c`", 15);
        assert!(f.bold && f.italic && !f.code && f.heading == 2);
        assert!(format_at("- item ~~gone~~", 12).strike);
        assert!(format_at("- item", 3).bullet && format_at("12. item", 4).numbered);
        assert_eq!(format_at("plain text", 5), Format::default());
    }
    #[test]
    fn preview_hides_markers_and_maps_unicode_hits_to_source() {
        let source = "# Café\n**日本語** and `code`\n- [ ] task\n";
        let preview = present(source, 0, Some(0), false, false);
        assert!(preview.text.starts_with("# Café\n日本語 and code"));
        assert!(preview.spans.iter().any(|s| s.style.bold));
        let byte = preview.text.find('日').unwrap();
        assert_eq!(
            preview.source_at(byte),
            source[..source.find('日').unwrap()].chars().count()
        );
        assert!(preview.text.contains('☐'));
        assert_eq!(present(source, 0, None, true, false).text, source);
    }
    #[test]
    fn incremental_fence_scan_propagates_and_does_not_flatten_long_lines() {
        let mut text = Rope::from_str("# title\n```\n# not heading\n```\n# final\n");
        let mut index = BlockIndex::default();
        index.update(&text, 0);
        assert!(index.lines[2].heading.is_none());
        assert!(index.lines[4].heading.is_some());
        text.remove(8..11);
        index.update(&text, 1);
        assert!(index.lines[2].heading.is_some());
        assert!(index.lines[4].heading.is_none());
        let long = Rope::from_str(&"x".repeat(1024 * 1024));
        index.update(&long, 0);
        assert_eq!(index.lines[0].prefix.len(), 512);
    }
    #[test]
    fn incremental_heading_index_matches_a_fresh_scan_after_line_insertions() {
        let mut rope = Rope::from_str("# first\n\n## second\n```\n# code\n```\n# final\n");
        let mut index = BlockIndex::default();
        index.update(&rope, 0);
        for (start, end, value) in [(8, 8, "\n# inserted\n"), (0, 2, "### "), (16, 19, "")] {
            let line = rope.char_to_line(start);
            rope.remove(start..end);
            rope.insert(start, value);
            let to = rope.char_to_line(start + value.chars().count()) + 1;
            index.update_range(&rope, line..to);
            let mut fresh = BlockIndex::default();
            fresh.update(&rope, 0);
            assert_eq!(index.headings, fresh.headings);
            assert_eq!(index.lines, fresh.lines);
        }
    }
}
