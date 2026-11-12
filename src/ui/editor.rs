use crate::{
    core::{
        buffer::Document,
        config::Settings,
        markdown::{self, BlockIndex, Presentation, RowKind, Style},
    },
    render::{self, CpuRenderer, Rect},
};
use cosmic_text::{
    Attrs, BidiParagraphs, Buffer, Cursor, Family, FontSystem, Metrics, Motion, Shaping, Weight, Wrap,
};
use std::collections::VecDeque;

const CHUNK: usize = 512;
pub struct Tile {
    pub start: usize,
    pub y: f32,
    pub height: f32,
    pub preview: Presentation,
    pub buffer: Buffer,
    source: String,
    raw: bool,
    code: bool,
    /// Row kind and column widths this tile was asked to draw as a table.
    table: Option<(RowKind, Vec<usize>)>,
    width: f32,
    size: f32,
    line_bytes: Vec<usize>,
}
impl Tile {
    fn display_cursor(&self, source: usize) -> Cursor {
        let byte = self.preview.byte_at(source.saturating_sub(self.start));
        let line = self
            .line_bytes
            .partition_point(|b| *b <= byte)
            .saturating_sub(1);
        Cursor::new(line, byte - self.line_bytes[line])
    }
    fn source_cursor(&self, cursor: Cursor) -> usize {
        self.start
            + self
                .preview
                .source_at(self.line_bytes.get(cursor.line).copied().unwrap_or(0) + cursor.index)
    }
    pub fn caret(&self, source: usize) -> Option<(f32, f32)> {
        self.buffer
            .cursor_position(&self.display_cursor(source))
            .map(|(x, y)| (x, self.y + y))
    }
}
/// The table (or run of pipe lines that is not one) around the last line asked about.
struct TableScan {
    revision: u64,
    rows: std::ops::Range<usize>,
    /// Header line, delimiter line and column widths.
    found: Option<(usize, usize, Vec<usize>)>,
}
#[derive(Default)]
pub struct EditorView {
    pub top: usize,
    pub tiles: Vec<Tile>,
    pub rect: Option<Rect>,
    cache: VecDeque<Tile>,
    table: Option<TableScan>,
    desired_x: Option<i32>,
    /// Cursor and revision after the last Up/Down.
    vertical: Option<(usize, u64)>,
    palette: render::theme::Palette,
    history: Vec<usize>,
    /// Top and revision after the last scroll; `history` is valid only while both match.
    scrolled: (usize, u64),
    /// Sorted char ranges to tint, e.g. every match of the find bar.
    pub highlights: Vec<(usize, usize)>,
}
fn attrs<'a>(style: Style, size: f32, settings: &'a Settings) -> Attrs<'a> {
    let palette = render::theme::Palette::new(settings.light(), settings.accent_color());
    let color = render::theme::color(if style.dim {
        palette.muted
    } else if style.link {
        palette.accent
    } else {
        palette.text
    });
    Attrs::new()
        .family(Family::Name(if style.code || style.mono {
            &settings.code_font
        } else {
            &settings.editor_font
        }))
        .weight(if style.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if style.italic {
            cosmic_text::Style::Italic
        } else {
            cosmic_text::Style::Normal
        })
        .color(color)
        .metadata(
            usize::from(style.code)
                | usize::from(style.strike) << 1
                | usize::from(style.link) << 2
                | usize::from(style.border) << 3,
        )
        .metrics(Metrics::new(
            if style.heading > 0 {
                size * (1.5 - style.heading as f32 * 0.06)
            } else {
                size
            },
            size * 1.65,
        ))
}
fn shape(preview: &Presentation, wrap: Wrap, width: f32, settings: &Settings, fonts: &mut FontSystem) -> Buffer {
    let mut buffer = Buffer::new_empty(Metrics::new(settings.font_size, settings.font_size * 1.65));
    buffer.set_size(Some(width), None);
    buffer.set_wrap(wrap);
    buffer.set_rich_text(
        preview.spans.iter().map(|span| {
            (
                &preview.text[span.bytes.clone()],
                attrs(span.style, settings.font_size, settings),
            )
        }),
        &attrs(Style::default(), settings.font_size, settings),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(fonts, false);
    buffer
}
/// Line start bytes split like `set_rich_text` (also at U+001C etc.); a trailing separator adds an end entry.
fn line_starts(text: &str) -> Vec<usize> {
    let base = text.as_ptr() as usize;
    let mut starts = vec![0];
    let mut end = 0;
    for (i, line) in BidiParagraphs::new(text).enumerate() {
        let start = line.as_ptr() as usize - base;
        if i > 0 {
            starts.push(start);
        }
        end = start + line.len();
    }
    if end < text.len() {
        starts.push(text.len());
    }
    starts
}
impl EditorView {
    /// How `line` should be drawn if it belongs to a pipe table: its role and the column widths.
    fn table_row(
        &mut self,
        doc: &Document,
        blocks: &BlockIndex,
        line: usize,
    ) -> Option<(RowKind, Vec<usize>)> {
        let fresh = self
            .table
            .as_ref()
            .is_some_and(|t| t.revision == doc.revision && t.rows.contains(&line));
        if !fresh {
            let pipes = |l: usize| {
                let text = doc.text.line(l);
                (text.len_chars() <= CHUNK && !blocks.in_code(l))
                    .then(|| text.to_string())
                    .filter(|s| s.contains('|'))
            };
            pipes(line)?;
            // ponytail: tables longer than 300 rows either side of the line get widths from a window.
            let mut first = line;
            while first > 0 && line - first < 300 && pipes(first - 1).is_some() {
                first -= 1;
            }
            let mut last = line;
            while last + 1 < doc.text.len_lines() && last - line < 300 && pipes(last + 1).is_some() {
                last += 1;
            }
            let lines: Vec<String> = (first..=last).map(|l| pipes(l).unwrap_or_default()).collect();
            let found = lines
                .iter()
                .position(|l| markdown::is_delimiter(l))
                .filter(|&d| d > 0)
                .map(|d| {
                    let rows = lines.iter().enumerate();
                    let rows = rows.filter(|&(i, _)| i + 1 >= d && i != d).map(|(_, l)| l.as_str());
                    (first + d - 1, first + d, markdown::column_widths(rows))
                });
            self.table = Some(TableScan { revision: doc.revision, rows: first..last + 1, found });
        }
        let scan = self.table.as_ref()?;
        let (header, delimiter, widths) = scan.found.as_ref()?;
        let kind = match line {
            l if l < *header => return None,
            l if l == *header => RowKind::Header,
            l if l == *delimiter => RowKind::Delimiter,
            l if l + 1 == scan.rows.end => RowKind::Last,
            _ => RowKind::Body,
        };
        Some((kind, widths.clone()))
    }
    pub fn clear_cache(&mut self) {
        self.tiles.clear();
        self.cache.clear();
    }
    pub(crate) fn caret(&self, cursor: usize) -> Option<(f32, f32)> {
        // A caret at a line start is also the end of the previous tile, which has no row for it.
        self.tiles
            .iter()
            .filter(|t| cursor >= t.start && cursor <= t.start + t.preview.end)
            .find_map(|t| t.caret(cursor))
    }
    pub fn layout(
        &mut self,
        doc: &Document,
        blocks: &BlockIndex,
        settings: &Settings,
        fonts: &mut FontSystem,
        rect: Rect,
    ) {
        self.fill(doc, blocks, settings, fonts, rect);
        if !settings.typewriter {
            return;
        }
        let middle = rect.height as f32 * 0.5;
        let row = settings.font_size * 1.65;
        // Move `top` so the caret sits near the middle and the pane stays full; passes are bounded.
        for _ in 0..3 {
            let Some((_, y)) = self.caret(doc.cursor) else {
                break;
            };
            let rows = ((y - middle) / row) as i32;
            let top = self.top;
            if rows != 0 {
                self.scroll(doc, rows);
            }
            if self.top == top {
                break;
            }
            self.fill(doc, blocks, settings, fonts, rect);
        }
        if let Some((_, y)) = self.caret(doc.cursor) {
            let shift = middle - y;
            for tile in &mut self.tiles {
                tile.y += shift;
            }
        }
    }
    fn fill(
        &mut self,
        doc: &Document,
        blocks: &BlockIndex,
        settings: &Settings,
        fonts: &mut FontSystem,
        rect: Rect,
    ) {
        self.palette =
            render::theme::Palette::new(settings.light(), settings.accent_color());
        self.rect = Some(rect);
        self.cache.extend(self.tiles.drain(..));
        let cursor_line = doc.text.char_to_line(doc.cursor);
        let mut start = self.top.min(doc.text.len_chars());
        let mut y = 0.0;
        while y < rect.height as f32 + settings.font_size * 2.0 {
            let line = doc.text.char_to_line(start);
            let line_start = doc.text.line_to_char(line);
            let line_end = if line + 1 < doc.text.len_lines() {
                doc.text.line_to_char(line + 1)
            } else {
                doc.text.len_chars()
            };
            let mut end = (start + CHUNK).min(line_end);
            // Don't split a CRLF pair between virtual chunks.
            if end > start
                && end < doc.text.len_chars()
                && doc.text.char(end - 1) == '\r'
                && doc.text.char(end) == '\n'
            {
                end += 1;
            }
            let source = doc.text.slice(start..end).to_string();
            let raw = settings.show_markers || line == cursor_line;
            let code = blocks.in_code(line)
                || (start > line_start
                    && doc
                        .text
                        .slice(line_start..(line_start + 3).min(line_end))
                        .to_string()
                        .starts_with("```"));
            let width = rect.width as f32;
            let row = if !raw && !code && start == line_start && end == line_end {
                self.table_row(doc, blocks, line)
            } else {
                None
            };
            let cached = self.cache.iter().position(|tile| {
                tile.source == source
                    && tile.raw == raw
                    && tile.code == code
                    && tile.table == row
                    && tile.width == width
                    && tile.size == settings.font_size
            });
            let mut tile = if let Some(index) = cached {
                self.cache.remove(index).unwrap()
            } else {
                let flowing = |fonts: &mut FontSystem| {
                    let preview = markdown::present(
                        &source,
                        0,
                        if raw { Some(0) } else { None },
                        settings.show_markers,
                        code,
                    );
                    let buffer = shape(&preview, Wrap::WordOrGlyph, width, settings, fonts);
                    (preview, buffer)
                };
                // A table wider than the pane would wrap into a mess, so it stays plain source.
                let (preview, buffer) = match &row {
                    Some((kind, widths)) => {
                        let grid = markdown::present_table_row(&source, 0, *kind, widths);
                        let buffer = shape(&grid, Wrap::None, width, settings, fonts);
                        let widest = buffer.layout_runs().map(|run| run.line_w).fold(0., f32::max);
                        if widest <= width { (grid, buffer) } else { flowing(fonts) }
                    }
                    None => flowing(fonts),
                };
                let mut height = buffer
                    .layout_runs()
                    .map(|run| run.line_top + run.line_height)
                    .last()
                    .unwrap_or(settings.font_size * 1.65);
                // Non-ASCII CRLF text gets an extra empty row; that newline belongs to the next tile.
                if source.ends_with('\n')
                    && buffer.lines.len() > 1
                    && buffer.lines.last().is_some_and(|line| line.text().is_empty())
                {
                    height -= settings.font_size * 1.65;
                }
                let line_bytes = line_starts(&preview.text);
                Tile {
                    start,
                    y,
                    height,
                    preview,
                    buffer,
                    source,
                    raw,
                    code,
                    table: row,
                    width,
                    size: settings.font_size,
                    line_bytes,
                }
            };
            tile.start = start;
            tile.y = y;
            if tile.preview.row == Some(RowKind::Delimiter) {
                // The header rule takes a sliver, not a blank line.
                tile.height = 3.;
                y += 3.;
            } else {
                y += tile.height.max(settings.font_size * 1.65);
            }
            self.tiles.push(tile);
            if end == doc.text.len_chars() {
                break;
            }
            start = end;
        }
        while self.cache.len() > 128 {
            self.cache.pop_front();
        }
    }
    pub fn paint(&mut self, doc: &Document, fonts: &mut FontSystem, renderer: &mut CpuRenderer) {
        let Some(rect) = self.rect else {
            return;
        };
        renderer.fill(
            Rect {
                x: rect.x - 8,
                width: rect.width + 8,
                ..rect
            },
            self.palette.background,
        );
        let selection = doc.selection();
        for tile in &mut self.tiles {
            if tile.code {
                renderer.fill(
                    Rect {
                        x: rect.x,
                        y: rect.y + tile.y as i32,
                        width: rect.width,
                        height: tile.height.ceil() as u32,
                    }
                    .intersect(rect),
                    self.palette.code,
                );
            }
            if tile.preview.quote {
                renderer.fill(
                    Rect {
                        x: rect.x - 8,
                        y: rect.y + tile.y as i32,
                        width: 3,
                        height: tile.height.ceil() as u32,
                    }
                    .intersect(Rect {
                        x: rect.x - 8,
                        width: rect.width + 8,
                        ..rect
                    }),
                    self.palette.accent,
                );
            }
            if !tile.preview.rules.is_empty() && !tile.raw {
                renderer.fill(
                    Rect {
                        x: rect.x,
                        y: rect.y + tile.y as i32 + (tile.size * 0.75) as i32,
                        width: rect.width,
                        height: 1,
                    }
                    .intersect(rect),
                    self.palette.muted,
                );
            }
            let delimiter = tile.preview.row == Some(RowKind::Delimiter);
            let mut edges: Option<(f32, f32)> = None;
            for run in tile.buffer.layout_runs() {
                let y = rect.y + (tile.y + run.line_top) as i32;
                if y >= rect.y + rect.height as i32 {
                    break;
                }
                for glyph in run.glyphs {
                    if glyph.metadata & 8 != 0 {
                        let x = glyph.x + glyph.w / 2.;
                        edges = Some(edges.map_or((x, x), |(a, b)| (a.min(x), b.max(x))));
                        renderer.fill(
                            Rect {
                                x: rect.x + x as i32,
                                y: rect.y + tile.y as i32,
                                width: 1,
                                height: tile.height.ceil() as u32,
                            }
                            .intersect(rect),
                            self.palette.muted,
                        );
                        continue;
                    }
                    if delimiter {
                        continue;
                    }
                    let byte = tile.line_bytes[run.line_i] + glyph.start;
                    let source = tile.start + tile.preview.source_at(byte);
                    let color = if selection.contains(&source) {
                        Some(self.palette.selection)
                    } else if {
                        let at = self.highlights.partition_point(|(start, _)| *start <= source);
                        at > 0 && source < self.highlights[at - 1].1
                    } {
                        Some(render::theme::mix(self.palette.background, self.palette.accent, 0.38))
                    } else if glyph.metadata & 1 != 0 {
                        Some(self.palette.code)
                    } else {
                        None
                    };
                    if let Some(color) = color {
                        renderer.fill(
                            Rect {
                                x: rect.x + glyph.x as i32,
                                y,
                                width: glyph.w.ceil() as u32 + 1,
                                height: run.line_height.ceil() as u32,
                            }
                            .intersect(rect),
                            color,
                        );
                    }
                    if glyph.metadata & 2 != 0 {
                        renderer.fill(
                            Rect {
                                x: rect.x + glyph.x as i32,
                                y: y + (run.line_height * 0.5) as i32,
                                width: glyph.w.ceil() as u32,
                                height: 1,
                            }
                            .intersect(rect),
                            self.palette.muted,
                        );
                    }
                    if glyph.metadata & 4 != 0 {
                        renderer.fill(
                            Rect {
                                x: rect.x + glyph.x as i32,
                                y: y + (run.line_height * 0.85) as i32,
                                width: glyph.w.ceil() as u32,
                                height: 1,
                            }
                            .intersect(rect),
                            self.palette.accent,
                        );
                    }
                }
            }
            if let (Some(kind), Some((left, right))) = (tile.preview.row, edges) {
                let bottom = tile.y + tile.height.ceil() - 1.;
                let lines = match kind {
                    RowKind::Header => [tile.y, tile.y],
                    RowKind::Delimiter => [tile.y, bottom],
                    _ => [bottom, bottom],
                };
                for y in lines {
                    renderer.fill(
                        Rect {
                            x: rect.x + left as i32,
                            y: rect.y + y as i32,
                            width: (right - left) as u32 + 1,
                            height: 1,
                        }
                        .intersect(rect),
                        self.palette.muted,
                    );
                }
            }
            renderer.text(
                fonts,
                &mut tile.buffer,
                rect.x,
                rect.y + tile.y as i32,
                rect,
            );
            if doc.cursor >= tile.start && doc.cursor <= tile.start + tile.preview.end {
                let cursor = tile.display_cursor(doc.cursor);
                if let Some((x, y)) = tile.buffer.cursor_position(&cursor) {
                    let y = rect.y + tile.y as i32 + y as i32;
                    if y < rect.y + rect.height as i32 {
                        renderer.fill(
                            Rect {
                                x: rect.x + x as i32,
                                y,
                                width: 2,
                                height: tile.size.ceil() as u32 + 5,
                            }
                            .intersect(rect),
                            self.palette.accent,
                        );
                    }
                }
            }
        }
    }
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let rect = self.rect?;
        let local_y = y - rect.y as f32;
        let tile = self
            .tiles
            .iter()
            .find(|tile| local_y < tile.y + tile.height)
            .or_else(|| self.tiles.last())?;
        tile.buffer
            .hit(x - rect.x as f32, local_y - tile.y)
            .map(|cursor| tile.source_cursor(cursor))
    }
    pub fn checkbox_at(&self, x: f32, y: f32) -> Option<(usize, bool)> {
        let source = self.hit(x, y)?;
        for tile in &self.tiles {
            for (_, index, checked) in &tile.preview.tasks {
                if (tile.start + index.saturating_sub(1)..=tile.start + index + 1).contains(&source)
                {
                    return Some((tile.start + index, *checked));
                }
            }
        }
        None
    }
    pub fn motion(
        &mut self,
        doc: &mut Document,
        blocks: &BlockIndex,
        settings: &Settings,
        fonts: &mut FontSystem,
        motion: Motion,
        extend: bool,
    ) {
        let Some(rect) = self.rect else {
            return;
        };
        let down = matches!(motion, Motion::Down | Motion::PageDown);
        // The column sticks only across consecutive vertical moves.
        if self.vertical != Some((doc.cursor, doc.revision)) {
            self.desired_x = None;
        }
        for attempt in 0..3 {
            let position = self.caret(doc.cursor);
            if let Some((x, y)) = position {
                // Aim 1 px past the caret row so short rows like a table rule (3 px) are not skipped.
                let row_end = self
                    .tiles
                    .iter()
                    .find(|t| t.y <= y && y < t.y + t.height)
                    .map_or(f32::INFINITY, |t| t.y + t.height);
                let target_y = if down { (y + settings.font_size * 1.65).min(row_end) + 1. } else { y - 1. };
                let desired_x = *self.desired_x.get_or_insert(x as i32);
                if target_y >= 0. && target_y < rect.height as f32 {
                    if let Some(source) =
                        self.hit(rect.x as f32 + desired_x as f32, rect.y as f32 + target_y)
                    {
                        doc.select(source, extend);
                    }
                    self.vertical = Some((doc.cursor, doc.revision));
                    return;
                }
            }
            if attempt < 2 {
                if position.is_none() {
                    // The caret is off screen: show it first, then move from it.
                    self.ensure_cursor(doc);
                } else {
                    self.scroll(doc, if down { 4 } else { -4 });
                }
                self.layout(doc, blocks, settings, fonts, rect);
            }
        }
        doc.select(if down { doc.text.len_chars() } else { 0 }, extend);
        self.vertical = Some((doc.cursor, doc.revision));
    }
    pub fn scroll(&mut self, doc: &Document, rows: i32) {
        if self.scrolled != (self.top, doc.revision) {
            self.history.clear();
        }
        if rows > 0 {
            let mut starts = Vec::new();
            for tile in &self.tiles {
                for run in tile.buffer.layout_runs() {
                    let byte =
                        tile.line_bytes[run.line_i] + run.glyphs.first().map_or(0, |g| g.start);
                    starts.push(if run.line_top == 0. {
                        tile.start
                    } else {
                        tile.start + tile.preview.source_at(byte)
                    });
                }
            }
            starts.sort_unstable();
            starts.dedup();
            for start in starts.into_iter().skip(1).take(rows as usize) {
                self.history.push(self.top);
                self.top = start;
            }
            if self.history.len() > 4096 {
                self.history.drain(..2048);
            }
        } else if !self.history.is_empty() {
            for _ in 0..-rows {
                if let Some(top) = self.history.pop() {
                    self.top = top.min(doc.text.len_chars());
                }
            }
        } else {
            let line = doc.text.char_to_line(self.top);
            let start = doc.text.line_to_char(line);
            if self.top > start {
                self.top = self.top.saturating_sub((-rows as usize) * 70).max(start);
            } else {
                self.top = doc.text.line_to_char(line.saturating_sub(-rows as usize));
            }
        }
        self.scrolled = (self.top, doc.revision);
    }
    pub fn ensure_cursor(&mut self, doc: &Document) {
        let visible_end = self
            .tiles
            .last()
            .map_or(self.top, |tile| tile.start + tile.preview.end);
        let height = self.rect.map_or(600, |r| r.height) as f32;
        let cursor_visible = self.caret(doc.cursor).is_some_and(|(_, y)| y < height - 30.0);
        if doc.cursor < self.top || doc.cursor > visible_end || !cursor_visible {
            let line_start = doc.text.line_to_char(doc.text.char_to_line(doc.cursor));
            self.top = if doc.cursor - line_start > CHUNK {
                doc.cursor.saturating_sub(160)
            } else {
                line_start
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;
    thread_local! {
        static FONTS: std::cell::RefCell<FontSystem> = std::cell::RefCell::new(FontSystem::new());
    }
    fn kinds(text: &str, cursor: usize, width: u32, markers: bool) -> Vec<Option<RowKind>> {
        let mut doc = Document::new(Rope::from_str(text));
        doc.select(cursor.min(doc.text.len_chars()), false);
        let mut blocks = BlockIndex::default();
        blocks.update(&doc.text, 0);
        let mut settings = Settings::default();
        settings.show_markers = markers;
        let mut view = EditorView::default();
        let rect = Rect { x: 0, y: 0, width, height: 4000 };
        FONTS.with(|fonts| view.layout(&doc, &blocks, &settings, &mut fonts.borrow_mut(), rect));
        // Painting and hit-testing every tile must not panic either.
        view.hit(10., 10.);
        view.tiles.iter().map(|t| t.preview.row).collect()
    }
    #[test]
    fn tables_get_header_rule_body_and_last_rows() {
        use RowKind::*;
        let table = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n\nafter\n";
        let cursor = table.len() - 2;
        let expect = vec![None, None, Some(Header), Some(Delimiter), Some(Body), Some(Last), None, None];
        assert_eq!(&kinds(table, cursor, 800, false)[..8], &expect[..]);
        // The same table in a CRLF file, and with no newline at the very end.
        let crlf = table.replace('\n', "\r\n");
        assert_eq!(&kinds(&crlf, crlf.len() - 2, 800, false)[..8], &expect[..]);
        let bare = "| a | b |\n|---|---|\n| 1 | 2 |";
        assert_eq!(kinds(bare, bare.len(), 800, false), [Some(Header), Some(Delimiter), None]);
        assert_eq!(kinds(bare, 0, 800, false), [None, Some(Delimiter), Some(Last)]);
    }
    #[test]
    fn tables_fall_back_to_source_when_they_cannot_be_drawn() {
        let table = "| a | b |\n|---|---|\n| 1 | 2 |\n\nx";
        let source = table.len() - 1;
        // Too narrow, markers shown, or inside a code fence: plain source, never a grid.
        assert!(kinds(table, source, 40, false).iter().all(Option::is_none));
        assert!(kinds(table, source, 800, true).iter().all(Option::is_none));
        let fenced = "```\n| a | b |\n|---|---|\n| 1 | 2 |\n```\nx";
        assert!(kinds(fenced, fenced.len(), 800, false).iter().all(Option::is_none));
        // A header with no delimiter row, or a delimiter with no header, is not a table.
        assert!(kinds("a | b\nc | d\nx", 12, 800, false).iter().all(Option::is_none));
        assert!(kinds("|---|---|\n| 1 | 2 |\nx", 19, 800, false).iter().all(Option::is_none));
    }
    #[test]
    fn long_tables_and_every_cursor_position_lay_out() {
        let mut big = String::from("| name | n |\n|---|---|\n");
        for i in 0..700 {
            big.push_str(&format!("| row {i} | {i} |\n"));
        }
        for cursor in [0, 30, big.len() / 2, big.len() - 1] {
            let k = kinds(&big, cursor, 800, false);
            assert!(k.len() > 10);
        }
        let small = "| é | 漢 |\r\n|:-:|--:|\r\n| `x` | **y** |\r\n\r\n|\r\n||\r\n|||\r\n";
        for cursor in 0..=small.chars().count() {
            kinds(small, cursor, 300, false);
        }
    }
    #[test]
    fn up_and_down_visit_the_table_rule_line() {
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\nx";
        let mut doc = Document::new(Rope::from_str(text));
        let mut blocks = BlockIndex::default();
        blocks.update(&doc.text, 0);
        let settings = Settings::default();
        let mut view = EditorView::default();
        let rect = Rect { x: 0, y: 0, width: 800, height: 600 };
        let mut lines = Vec::new();
        FONTS.with(|fonts| {
            let fonts = &mut fonts.borrow_mut();
            for motion in [Motion::Down, Motion::Down, Motion::Up, Motion::Up] {
                view.layout(&doc, &blocks, &settings, fonts, rect);
                view.motion(&mut doc, &blocks, &settings, fonts, motion, false);
                lines.push(doc.text.char_to_line(doc.cursor));
            }
        });
        assert_eq!(lines, [1, 2, 1, 0]);
    }
    #[test]
    fn separators_and_crlf_lay_out_one_row_per_line() {
        // cosmic-text breaks at U+001C and U+2029 too, and adds an empty row after a non-ASCII CRLF line.
        let text = "é\r\na\u{1c}b\u{2029}c\r\nend";
        let mut doc = Document::new(Rope::from_str(text));
        doc.select(doc.text.len_chars(), false);
        let mut blocks = BlockIndex::default();
        blocks.update(&doc.text, 0);
        let settings = Settings::default();
        let mut view = EditorView::default();
        let rect = Rect { x: 0, y: 0, width: 800, height: 600 };
        FONTS.with(|fonts| view.layout(&doc, &blocks, &settings, &mut fonts.borrow_mut(), rect));
        let row = settings.font_size * 1.65;
        assert!(view.tiles[0].height <= row + 0.5, "{}", view.tiles[0].height);
        assert_eq!(view.hit(1., view.tiles[0].height - 1.), Some(0));
        view.scroll(&doc, 1);
        view.scroll(&doc, -1);
        assert_eq!(view.top, 0);
    }
}
