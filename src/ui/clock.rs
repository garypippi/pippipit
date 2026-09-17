//! Block-glyph clock.
//!
//! The bitmaps are built in, so no figlet-style crate is needed.
//!
//! A terminal cell is about twice as tall as it is wide, so drawing one bitmap pixel
//! per cell stretches the digits vertically and leaves the vertical strokes looking
//! half the weight of the horizontal ones. Both are fixed by **packing two pixel rows
//! into one cell** with the half blocks `▀ ▄ █`: a 3x5 pixel digit lands on 3 columns
//! by 2.5 rows, which is about 3:5 on screen - the proportion a digit actually wants -
//! and every stroke ends up one unit thick. `HH:MM:SS` is 27 columns by 3 rows.

/// Height of one glyph, in cells. Two pixel rows per cell, five pixel rows, rounded up.
pub const GLYPH_HEIGHT: usize = 3;

/// Bitmap height of one glyph, in pixels.
const PIXEL_ROWS: usize = 5;
/// Bitmap width of a digit, in pixels.
const DIGIT_WIDTH: usize = 3;

const DIGITS: [[&str; PIXEL_ROWS]; 10] = [
    ["███", "█ █", "█ █", "█ █", "███"], // 0
    // The flag hangs off the **left** of the stem and the stem sits in the middle column.
    // Hanging it off the right instead leaves the 1 pressed against the next digit.
    [" █ ", "██ ", " █ ", " █ ", " █ "], // 1
    ["███", "  █", "███", "█  ", "███"], // 2
    ["███", "  █", "███", "  █", "███"], // 3
    ["█ █", "█ █", "███", "  █", "  █"], // 4
    ["███", "█  ", "███", "  █", "███"], // 5
    ["███", "█  ", "███", "█ █", "███"], // 6
    ["███", "  █", "  █", "  █", "  █"], // 7
    ["███", "█ █", "███", "█ █", "███"], // 8
    ["███", "█ █", "███", "  █", "███"], // 9
];

/// One column wide, with the dots on pixel rows 1 and 3.
const COLON: [&str; PIXEL_ROWS] = [" ", "█", " ", "█", " "];
const BLANK: [&str; PIXEL_ROWS] = [" ", " ", " ", " ", " "];

/// Columns between two neighbouring glyphs.
const GAP: usize = 1;

fn bitmap(ch: char) -> &'static [&'static str; PIXEL_ROWS] {
    match ch {
        '0'..='9' => &DIGITS[ch as usize - '0' as usize],
        ':' => &COLON,
        _ => &BLANK,
    }
}

fn glyph_width(ch: char) -> usize {
    match ch {
        '0'..='9' => DIGIT_WIDTH,
        _ => 1,
    }
}

/// Pack the pixel rows into cells, two at a time.
///
/// The bottom pixel row has no partner, so it comes out as an upper half block.
fn pack(pixels: &[&str; PIXEL_ROWS]) -> Vec<String> {
    (0..PIXEL_ROWS)
        .step_by(2)
        .map(|row| {
            let top = pixels[row];
            let bottom = pixels.get(row + 1).copied().unwrap_or("");
            top.chars()
                .enumerate()
                .map(|(i, t)| {
                    let b = bottom.chars().nth(i).unwrap_or(' ');
                    match (t != ' ', b != ' ') {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => ' ',
                    }
                })
                .collect()
        })
        .collect()
}

/// Render `text` as block glyphs, returning `GLYPH_HEIGHT` lines.
///
/// Anything but a digit or `:` becomes a single blank column.
pub fn render(text: &str) -> Vec<String> {
    let mut rows = vec![String::new(); GLYPH_HEIGHT];
    for (i, ch) in text.chars().enumerate() {
        let glyph = pack(bitmap(ch));
        for (row, line) in rows.iter_mut().zip(&glyph) {
            if i > 0 {
                row.push_str(&" ".repeat(GAP));
            }
            row.push_str(line);
        }
    }
    rows
}

/// Display width of the lines `render` returns. Split out because the layout needs it
/// up front to place what sits beside the clock.
pub fn width(text: &str) -> u16 {
    let mut w = 0usize;
    for (i, ch) in text.chars().enumerate() {
        if i > 0 {
            w += GAP;
        }
        w += glyph_width(ch);
    }
    w as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_three_rows() {
        assert_eq!(render("12:34:56").len(), GLYPH_HEIGHT);
    }

    #[test]
    fn width_matches_rendered_rows() {
        for text in ["12:34:56", "00:00", "9", ":", "", "1 2"] {
            for row in &render(text) {
                assert_eq!(row.chars().count() as u16, width(text), "text={text}");
            }
        }
    }

    /// The clock has to leave the column beside it usable: at 27 columns the date
    /// still fits to its right.
    #[test]
    fn hhmmss_is_27_columns() {
        assert_eq!(width("12:34:56"), 27);
    }

    /// A bare stem is not a 1, and a stem on the right edge crowds the next digit.
    #[test]
    fn one_has_a_flag_left_of_a_centred_stem() {
        let rows = render("1");
        assert_eq!(rows[0], "▄█ ", "flag and stem");
        assert_eq!(rows[1], " █ ", "stem stays in the middle column");
        assert_eq!(rows[2], " ▀ ", "stem stays in the middle column");
    }

    /// Both halves of a cell filled must collapse into one full block, or the digits
    /// come out striped.
    #[test]
    fn full_cells_use_the_full_block() {
        // Pixel rows 0 and 1 of an 8 are "███" over "█ █".
        assert_eq!(render("8")[0], "█▀█");
    }

    #[test]
    fn the_colon_dots_sit_on_the_inner_pixel_rows() {
        let rows = render(":");
        assert_eq!(rows, ["▄", "▄", " "]);
    }

    #[test]
    fn empty_text_is_empty() {
        assert_eq!(width(""), 0);
        assert!(render("").iter().all(|r| r.is_empty()));
    }
}
