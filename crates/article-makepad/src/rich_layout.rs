//! Mixed-face text layout shared by selection, native IME and rendering.
use std::rc::Rc;
use makepad_widgets::{
    *,
    text::{
        layouter::{LaidoutText, LaidoutRow},
        substr::Substr,
        geom::{Point, Size as TextSize},
        line_break::PrecedingContext,
    },
};
use unicode_segmentation::UnicodeSegmentation;
use super::document::Mark;

pub fn layout(
    cx: &mut Cx,
    normal: &DrawText,
    bold: &DrawText,
    italic: &DrawText,
    both: &DrawText,
    text: &str,
    marks: &[Mark],
    width: Option<f32>,
) -> Rc<LaidoutText> {
    if marks.is_empty() {
        return normal.layout(cx, 0.0, 0.0, width, width.is_some(), Align::default(), text);
    }
    let parent = Substr::from(text);
    let empty = normal.layout(cx, 0.0, 0.0, None, false, Align::default(), "");
    let seed = empty.rows[0].clone();
    let mut rows = Vec::new();
    let mut row = seed.clone();
    row.glyphs.clear();
    row.width_in_lpxs = 0.0;
    let mut row_start = 0;
    let mut y = 0.0;
    let finish = |rows: &mut Vec<LaidoutRow>,
                  row: &mut LaidoutRow,
                  end: usize,
                  newline: bool,
                  start: &mut usize,
                  y: &mut f32| {
        row.text = parent.substr(*start..end);
        row.newline = newline;
        row.origin_in_lpxs = Point::new(0.0, *y + row.ascender_in_lpxs);
        *y += (row.ascender_in_lpxs - row.descender_in_lpxs + row.line_gap_in_lpxs)
            * row.line_spacing_scale;
        rows.push(row.clone());
        *row = seed.clone();
        row.glyphs.clear();
        row.width_in_lpxs = 0.0;
        *start = end;
    };
    // Shape whole words where possible, keeping ligatures and kerning. Long words
    // are split only at grapheme boundaries. CJK word boundaries wrap naturally.
    for (word_start, word) in text.split_word_bound_indices() {
        let mut chunks: Vec<(usize, &str)> = Vec::new();
        let mut run = 0;
        let flags = |pos: usize| {
            let mut f = (false, false);
            for m in marks {
                if pos >= m.start && pos < m.end {
                    f.0 |= m.bold;
                    f.1 |= m.italic;
                }
            }
            f
        };
        let mut previous = flags(word_start);
        for (i, _) in word.grapheme_indices(true).skip(1) {
            let next = flags(word_start + i);
            if next != previous {
                chunks.push((word_start + run, &word[run..i]));
                run = i;
                previous = next;
            }
        }
        chunks.push((word_start + run, &word[run..]));
        for (start, chunk) in chunks {
            if chunk == "\n" || chunk == "\r\n" {
                finish(&mut rows, &mut row, start, true, &mut row_start, &mut y);
                row_start = start + chunk.len();
                continue;
            }
            let (b, i) = flags(start);
            let source = match (b, i) {
                (true, true) => both,
                (true, false) => bold,
                (false, true) => italic,
                _ => normal,
            };
            let styled = source;
            let shaped = styled.layout(cx, 0.0, 0.0, None, false, Align::default(), chunk);
            let pieces: Vec<(usize, &str)> = if width.is_some_and(|w| shaped.size_in_lpxs.width > w)
            {
                chunk
                    .grapheme_indices(true)
                    .map(|(i, g)| (start + i, g))
                    .collect()
            } else {
                vec![(start, chunk)]
            };
            for (offset, piece) in pieces {
                let shaped = styled.layout(cx, 0.0, 0.0, None, false, Align::default(), piece);
                let Some(fragment) = shaped.rows.first() else {
                    continue;
                };
                if width.is_some_and(|w| {
                    row.width_in_lpxs > 0.0 && row.width_in_lpxs + fragment.width_in_lpxs > w
                }) {
                    finish(&mut rows, &mut row, offset, false, &mut row_start, &mut y);
                }
                for glyph in &fragment.glyphs {
                    let mut g = glyph.clone();
                    g.cluster += offset - row_start;
                    g.origin_in_lpxs.x += row.width_in_lpxs;
                    row.glyphs.push(g);
                }
                row.width_in_lpxs += fragment.width_in_lpxs;
                row.ascender_in_lpxs = row.ascender_in_lpxs.max(fragment.ascender_in_lpxs);
                row.descender_in_lpxs = row.descender_in_lpxs.min(fragment.descender_in_lpxs);
                row.line_gap_in_lpxs = row.line_gap_in_lpxs.max(fragment.line_gap_in_lpxs);
            }
        }
    }
    finish(
        &mut rows,
        &mut row,
        text.len(),
        false,
        &mut row_start,
        &mut y,
    );
    let max = rows.iter().map(|r| r.width_in_lpxs).fold(0.0, f32::max);
    // JIT PORT: upstream's LaidoutText also carries run-joining state. This
    // layout is a whole paragraph, so the next run starts after all of it.
    let mut line_break_before_next_run = PrecedingContext::PARAGRAPH_START;
    line_break_before_next_run.append(text);
    Rc::new(LaidoutText {
        text: parent,
        size_in_lpxs: TextSize::new(max, y),
        rows,
        has_missing_glyph: false,
        is_truncated: false,
        line_break_before_next_run,
        is_trailing_glue_reserved: false,
    })
}
