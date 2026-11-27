//! A ratatui [`Buffer`] as an SVG picture: one `<text>` per run of cells that share a style, one `<rect>`
//! per run of cells that share a background, on a solid background, sized to the cell grid.
//!
//! Everything is integer arithmetic and fixed strings, so the same buffer gives the same bytes on every
//! machine (no float formatting, no map iteration, `\n` line ends). The colours are a fixed palette:
//! a terminal decides what `Color::Cyan` looks like, a picture has to say.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use crate::tui::snapshot::text_width;

/// Pixels of one terminal cell. A 13 px monospace font advances about 7.8 px, so 8 keeps the glyphs
/// of a row on the grid; `textLength` takes up what a different font leaves over.
pub const CELL_W: u32 = 8;
pub const CELL_H: u32 = 18;
const FONT_SIZE: u32 = 13;
/// Distance of the text baseline from the top of its row.
const BASELINE: u32 = 13;
/// Blank cells that end a text run: a longer gap is cheaper as two `<text>` elements than as spaces.
const MAX_GAP: u32 = 3;

const FONT_STACK: &str =
    "ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', 'Liberation Mono', monospace";

type Rgb = (u8, u8, u8);

/// The terminal's own foreground and background (`Color::Reset`): dark, so the picture reads the same
/// on a light and on a dark page, which only ever sees it as one solid rectangle.
const DEFAULT_FG: Rgb = (0xd7, 0xda, 0xe0);
const DEFAULT_BG: Rgb = (0x1b, 0x1e, 0x24);

/// The sixteen ANSI colours in the order of the 256-colour table (so `Color::Indexed(0..16)` matches).
const ANSI: [Rgb; 16] = [
    (0x28, 0x2c, 0x34), // black
    (0xe0, 0x6c, 0x75), // red
    (0x98, 0xc3, 0x79), // green
    (0xe5, 0xc0, 0x7b), // yellow
    (0x61, 0xaf, 0xef), // blue
    (0xc6, 0x78, 0xdd), // magenta
    (0x56, 0xb6, 0xc2), // cyan
    (0xab, 0xb2, 0xbf), // gray (ANSI white)
    (0x5c, 0x63, 0x70), // dark gray (bright black)
    (0xf0, 0x86, 0x8e), // light red
    (0xb0, 0xd9, 0x8f), // light green
    (0xf0, 0xd3, 0x97), // light yellow
    (0x82, 0xc0, 0xf7), // light blue
    (0xd9, 0x9c, 0xec), // light magenta
    (0x7c, 0xd0, 0xdc), // light cyan
    (0xff, 0xff, 0xff), // white (bright white)
];

/// `Color::Indexed(n)` as the xterm 256-colour table defines it: 16 ANSI, a 6x6x6 cube, 24 greys.
fn indexed(n: u8) -> Rgb {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match n {
        0..=15 => ANSI[n as usize],
        16..=231 => {
            let i = (n - 16) as usize;
            (LEVELS[i / 36], LEVELS[i / 6 % 6], LEVELS[i % 6])
        }
        _ => {
            let grey = 8 + 10 * (n - 232);
            (grey, grey, grey)
        }
    }
}

/// The colour of a cell's `fg` or `bg`; `Reset` is the terminal default for that role.
fn rgb(color: Color, default: Rgb) -> Rgb {
    match color {
        Color::Reset => default,
        Color::Black => ANSI[0],
        Color::Red => ANSI[1],
        Color::Green => ANSI[2],
        Color::Yellow => ANSI[3],
        Color::Blue => ANSI[4],
        Color::Magenta => ANSI[5],
        Color::Cyan => ANSI[6],
        Color::Gray => ANSI[7],
        Color::DarkGray => ANSI[8],
        Color::LightRed => ANSI[9],
        Color::LightGreen => ANSI[10],
        Color::LightYellow => ANSI[11],
        Color::LightBlue => ANSI[12],
        Color::LightMagenta => ANSI[13],
        Color::LightCyan => ANSI[14],
        Color::White => ANSI[15],
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(n) => indexed(n),
    }
}

fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// `fg` at 60 % over `bg`: what a terminal's dim (faint) attribute does, in integers.
fn dimmed(fg: Rgb, bg: Rgb) -> Rgb {
    let mix = |f: u8, b: u8| ((u16::from(f) * 3 + u16::from(b) * 2) / 5) as u8;
    (mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2))
}

/// What a cell looks like once reverse video and dim are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Look {
    fg: Rgb,
    bg: Rgb,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    hidden: bool,
}

impl Look {
    fn of(fg: Color, bg: Color, m: Modifier) -> Look {
        let (mut fg, mut bg) = (rgb(fg, DEFAULT_FG), rgb(bg, DEFAULT_BG));
        if m.contains(Modifier::REVERSED) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if m.contains(Modifier::DIM) {
            fg = dimmed(fg, bg);
        }
        Look {
            fg,
            bg,
            bold: m.contains(Modifier::BOLD),
            italic: m.contains(Modifier::ITALIC),
            underline: m.contains(Modifier::UNDERLINED),
            strike: m.contains(Modifier::CROSSED_OUT),
            hidden: m.contains(Modifier::HIDDEN),
        }
    }

    /// What a text run is told apart by: the background belongs to the rectangles.
    fn text_key(&self) -> (Rgb, bool, bool, bool, bool) {
        (self.fg, self.bold, self.italic, self.underline, self.strike)
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A run of text on one row: where it starts (in cells), how many cells it covers and what it says.
struct Run {
    col: u32,
    cells: u32,
    look: Look,
    text: String,
}

impl Run {
    fn write(&self, row: u32, out: &mut String) {
        let (x, y) = (self.col * CELL_W, row * CELL_H + BASELINE);
        out.push_str(&format!(
            "<text x=\"{x}\" y=\"{y}\" fill=\"{}\"",
            hex(self.look.fg)
        ));
        // lengthAdjust="spacing" pins each run to the grid of the cells without stretching any glyph
        if self.cells > 1 {
            out.push_str(&format!(
                " textLength=\"{}\" lengthAdjust=\"spacing\"",
                self.cells * CELL_W
            ));
        }
        if self.look.bold {
            out.push_str(" font-weight=\"700\"");
        }
        if self.look.italic {
            out.push_str(" font-style=\"italic\"");
        }
        match (self.look.underline, self.look.strike) {
            (true, true) => out.push_str(" text-decoration=\"underline line-through\""),
            (true, false) => out.push_str(" text-decoration=\"underline\""),
            (false, true) => out.push_str(" text-decoration=\"line-through\""),
            (false, false) => {}
        }
        out.push('>');
        out.push_str(&escape(&self.text));
        out.push_str("</text>\n");
    }
}

/// Draws `buf` as an SVG document of `width * CELL_W` by `height * CELL_H` pixels; `title` is what a
/// screen reader says about the picture.
pub fn render(buf: &Buffer, title: &str) -> String {
    let (cols, rows) = (u32::from(buf.area.width), u32::from(buf.area.height));
    let (w, h) = (cols * CELL_W, rows * CELL_H);
    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\"{}\">\n",
        escape(title).replace('"', "&quot;")
    ));
    out.push_str(&format!("<title>{}</title>\n", escape(title)));
    out.push_str(&format!(
        "<style>text{{font-family:{FONT_STACK};font-size:{FONT_SIZE}px;white-space:pre}}</style>\n"
    ));
    out.push_str(&format!(
        "<rect width=\"{w}\" height=\"{h}\" fill=\"{}\"/>\n",
        hex(DEFAULT_BG)
    ));
    // rectangles first, then the text, so a glyph is never covered by the cell next to it
    out.push_str("<g shape-rendering=\"crispEdges\">\n");
    for y in 0..buf.area.height {
        let mut x = 0;
        while x < buf.area.width {
            let bg = look(buf, x, y).bg;
            let start = x;
            while x < buf.area.width && look(buf, x, y).bg == bg {
                x += 1;
            }
            if bg != DEFAULT_BG {
                out.push_str(&format!(
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{CELL_H}\" fill=\"{}\"/>\n",
                    u32::from(start) * CELL_W,
                    u32::from(y) * CELL_H,
                    u32::from(x - start) * CELL_W,
                    hex(bg)
                ));
            }
        }
    }
    out.push_str("</g>\n");
    for y in 0..buf.area.height {
        for run in runs(buf, y) {
            run.write(u32::from(y), &mut out);
        }
    }
    out.push_str("</svg>\n");
    out
}

fn look(buf: &Buffer, x: u16, y: u16) -> Look {
    let cell = &buf[(x, y)];
    Look::of(cell.fg, cell.bg, cell.modifier)
}

/// The text of row `y` as runs of non-blank cells that look alike. A blank cell joins the run it sits
/// in (up to [`MAX_GAP`] of them between two cells of the same look), so a line of words is one element.
fn runs(buf: &Buffer, y: u16) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let mut current: Option<Run> = None;
    let mut gap = 0;
    let mut x = 0;
    while x < buf.area.width {
        let cell = &buf[(x, y)];
        let look = look(buf, x, y);
        let symbol = cell.symbol();
        // a wide glyph covers the cell after it, which holds nothing of its own
        let width = text_width(symbol).max(1) as u32;
        x += width as u16;
        if symbol.trim().is_empty() || look.hidden {
            gap += width;
            continue;
        }
        let col = u32::from(x) - width;
        match current.as_mut() {
            Some(run) if run.look.text_key() == look.text_key() && gap <= MAX_GAP => {
                run.text.push_str(&" ".repeat(gap as usize));
                run.cells += gap;
            }
            _ => {
                out.extend(current.take());
                current = Some(Run {
                    col,
                    cells: 0,
                    look,
                    text: String::new(),
                });
            }
        }
        if let Some(run) = current.as_mut() {
            run.text.push_str(symbol);
            run.cells += width;
        }
        gap = 0;
    }
    out.extend(current);
    out
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    use super::*;

    fn buffer(cols: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, cols, 2))
    }

    #[test]
    fn the_size_is_the_cell_grid_and_the_background_is_solid() {
        let svg = render(&buffer(10), "a title");
        assert!(
            svg.contains("width=\"80\" height=\"36\" viewBox=\"0 0 80 36\""),
            "{svg}"
        );
        assert!(
            svg.contains("<rect width=\"80\" height=\"36\" fill=\"#1b1e24\"/>"),
            "{svg}"
        );
        assert!(
            svg.contains("<title>a title</title>") && svg.contains("monospace;"),
            "{svg}"
        );
        assert!(svg.ends_with("</svg>\n") && !svg.contains('\r'));
    }

    #[test]
    fn a_run_carries_the_colour_weight_and_position_of_its_cells() {
        let mut buf = buffer(10);
        buf.set_string(
            2,
            1,
            "ok",
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        );
        let svg = render(&buf, "t");
        assert!(
            svg.contains("<text x=\"16\" y=\"31\" fill=\"#98c379\" textLength=\"16\" lengthAdjust=\"spacing\" font-weight=\"700\">ok</text>"),
            "{svg}"
        );
    }

    #[test]
    fn reverse_video_swaps_the_colours_and_draws_the_background() {
        let mut buf = buffer(10);
        buf.set_string(
            0,
            0,
            "Now",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::REVERSED),
        );
        let svg = render(&buf, "t");
        assert!(
            svg.contains("<rect x=\"0\" y=\"0\" width=\"24\" height=\"18\" fill=\"#56b6c2\"/>"),
            "{svg}"
        );
        assert!(
            svg.contains("fill=\"#1b1e24\" textLength=\"24\""),
            "text in the background colour:\n{svg}"
        );
    }

    #[test]
    fn the_selected_row_background_is_the_xterm_grey_and_dim_text_is_softer() {
        let mut buf = buffer(10);
        buf.set_string(0, 0, "row", Style::new().bg(Color::Indexed(238)));
        buf.set_string(0, 1, "dim", Style::new().add_modifier(Modifier::DIM));
        let svg = render(&buf, "t");
        assert!(svg.contains("fill=\"#444444\""), "{svg}");
        // 60 % of #d7dae0 over #1b1e24
        assert!(svg.contains("fill=\"#8b8e94\""), "{svg}");
    }

    #[test]
    fn text_is_escaped_and_short_gaps_stay_inside_one_run() {
        let mut buf = buffer(12);
        buf.set_string(0, 0, "a<b  &c", Style::new());
        let svg = render(&buf, "t");
        assert!(svg.contains(">a&lt;b  &amp;c</text>"), "{svg}");
        // a gap wider than MAX_GAP ends the run instead of padding it with spaces
        let mut wide = Buffer::empty(Rect::new(0, 0, 12, 1));
        wide.set_string(0, 0, "a", Style::new());
        wide.set_string(8, 0, "b", Style::new());
        let svg = render(&wide, "t");
        assert_eq!(svg.matches("<text ").count(), 2, "{svg}");
    }

    #[test]
    fn indexed_colours_follow_the_xterm_table() {
        assert_eq!(indexed(1), ANSI[1]);
        assert_eq!(indexed(16), (0, 0, 0));
        assert_eq!(indexed(231), (255, 255, 255));
        assert_eq!(indexed(196), (255, 0, 0));
        assert_eq!(indexed(232), (8, 8, 8));
        assert_eq!(indexed(255), (238, 238, 238));
    }
}
