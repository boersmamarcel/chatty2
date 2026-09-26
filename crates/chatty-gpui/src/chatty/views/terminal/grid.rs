//! The toolkit-free half of the terminal renderer: colour resolution and
//! turning a row of cells into a few text runs and background rects.
//!
//! Everything here is plain data so it can be unit-tested without a window.
//! Colours are packed `0xRRGGBB`; the element converts them to `Hsla` only
//! when it shapes or paints.

use chatty_terminal::alacritty_terminal::term::cell::{Cell, Flags};
use chatty_terminal::alacritty_terminal::term::color::Colors;
use chatty_terminal::alacritty_terminal::vte::ansi::{Color, NamedColor};
use gpui::{Hsla, Rgba};
use gpui_component::ThemeColor;

/// A colour as `0xRRGGBB`.
pub type Rgb24 = u32;

/// How much a `DIM` foreground moves towards the background.
const DIM_TOWARDS_BACKGROUND: f32 = 1.0 / 3.0;

/// The terminal's base colours: the 16 ANSI colours plus default
/// foreground, background and cursor. The 256-colour cube and grey ramp are
/// computed, not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub ansi: [Rgb24; 16],
    pub foreground: Rgb24,
    pub background: Rgb24,
    pub cursor: Rgb24,
}

impl Palette {
    /// A conventional palette for when no theme is available, and the source
    /// of the blacks and whites a chatty theme has no slot for.
    pub fn fallback(dark: bool) -> Self {
        if dark {
            Self {
                ansi: [
                    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5,
                    0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
                ],
                foreground: 0xcccccc,
                background: 0x1e1e1e,
                cursor: 0xcccccc,
            }
        } else {
            Self {
                ansi: [
                    0x000000, 0xcd3131, 0x00bc00, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555,
                    0x666666, 0xcd3131, 0x14ce14, 0xb5ba00, 0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5,
                ],
                foreground: 0x333333,
                background: 0xffffff,
                cursor: 0x333333,
            }
        }
    }

    /// The palette for a chatty theme: its red/green/yellow/blue/magenta/cyan,
    /// foreground, background and caret. Black and white come from
    /// [`fallback`](Self::fallback). On a light background the normal colours
    /// are darkened a little so yellow and cyan text stay readable; on a dark
    /// one the bright colours are the theme colours lightened.
    pub fn from_theme(theme: &ThemeColor, dark: bool) -> Self {
        let mut palette = Self::fallback(dark);
        let base = [
            theme.red,
            theme.green,
            theme.yellow,
            theme.blue,
            theme.magenta,
            theme.cyan,
        ];
        for (i, color) in base.into_iter().enumerate() {
            let (normal, bright) = if dark {
                (color, lighten(color, 0.25))
            } else {
                (darken(color, 0.2), color)
            };
            palette.ansi[1 + i] = pack(normal);
            palette.ansi[9 + i] = pack(bright);
        }
        palette.foreground = pack(theme.foreground);
        palette.background = pack(theme.background);
        palette.cursor = pack(theme.caret);
        palette
    }

    /// Colour for a 256-colour index: the 16 ANSI colours, the 6×6×6 cube,
    /// then the 24-step grey ramp.
    pub fn indexed(&self, index: u8) -> Rgb24 {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let i = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v as u32 };
                (level(i / 36) << 16) | (level((i / 6) % 6) << 8) | level(i % 6)
            }
            232..=255 => {
                let grey = 8 + 10 * (index - 232) as u32;
                (grey << 16) | (grey << 8) | grey
            }
        }
    }

    fn named(&self, color: NamedColor) -> Rgb24 {
        let index = color as usize;
        match color {
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => {
                blend(self.foreground, self.background, DIM_TOWARDS_BACKGROUND)
            }
            // DimBlack..=DimWhite follow Cursor (258) in the enum.
            _ if (259..267).contains(&index) => blend(
                self.ansi[index - 259],
                self.background,
                DIM_TOWARDS_BACKGROUND,
            ),
            _ => self.ansi[index.min(15)],
        }
    }
}

/// Resolve a cell colour: the program's own palette overrides (OSC 4/10/11)
/// first, then the palette.
pub fn resolve_color(color: Color, overrides: &Colors, palette: &Palette) -> Rgb24 {
    match color {
        Color::Spec(rgb) => ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32,
        Color::Indexed(index) => overrides[index as usize]
            .map(|rgb| ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32)
            .unwrap_or_else(|| palette.indexed(index)),
        Color::Named(named) => overrides[named]
            .map(|rgb| ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32)
            .unwrap_or_else(|| palette.named(named)),
    }
}

/// A cell's final foreground and background after `INVERSE`, `DIM` and
/// `HIDDEN`.
pub fn cell_colors(cell: &Cell, overrides: &Colors, palette: &Palette) -> (Rgb24, Rgb24) {
    let mut fg = resolve_color(cell.fg, overrides, palette);
    let mut bg = resolve_color(cell.bg, overrides, palette);
    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.flags.contains(Flags::DIM) {
        fg = blend(fg, bg, DIM_TOWARDS_BACKGROUND);
    }
    if cell.flags.contains(Flags::HIDDEN) {
        fg = bg;
    }
    (fg, bg)
}

/// What a text run looks like. Everything that changes the shaped glyphs or
/// their decorations; the background is painted separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunStyle {
    pub fg: Rgb24,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

impl RunStyle {
    fn decorated(&self) -> bool {
        self.underline || self.strikethrough
    }
}

/// How the glyphs of a run sit on the cell grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunGrid {
    /// One glyph per cell; glyphs are snapped to the cell width.
    Cells,
    /// A single wide character (CJK, emoji) spanning two cells.
    Wide,
    /// A character with combining marks: shaped as a cluster, not snapped.
    Cluster,
}

/// A run of cells shaped as one piece of text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TextRunSpec {
    /// First cell.
    pub col: u16,
    /// Cells covered.
    pub cells: u16,
    pub text: String,
    pub style: RunStyle,
    pub grid: RunGrid,
}

/// A span of cells with a non-default background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BgRect {
    pub col: u16,
    pub cells: u16,
    pub color: Rgb24,
}

/// One row, ready to shape and paint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowLayout {
    pub runs: Vec<TextRunSpec>,
    pub backgrounds: Vec<BgRect>,
}

/// Batch a row of cells into text runs and merged background rects.
///
/// Consecutive cells with the same [`RunStyle`] become one run; plain blanks
/// join whatever run they sit in (their colour does not show), so a line of
/// coloured words separated by spaces is one run per colour change, not per
/// word. Blanks are never shaped at the start or end of a run. Wide and
/// combining characters get runs of their own so the glyphs around them stay
/// on the grid. Cells whose background is the terminal default get no rect.
pub fn batch_row(cells: &[Cell], overrides: &Colors, palette: &Palette) -> RowLayout {
    let mut row = RowLayout::default();
    let mut current: Option<TextRunSpec> = None;

    for (col, cell) in cells.iter().enumerate() {
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            // The right half of a wide char: its run and rect cover it.
            continue;
        }
        let col = col as u16;
        let (fg, bg) = cell_colors(cell, overrides, palette);
        let wide = cell.flags.contains(Flags::WIDE_CHAR);
        let cells_spanned = if wide { 2 } else { 1 };

        if bg != palette.background {
            match row.backgrounds.last_mut() {
                Some(last) if last.color == bg && last.col + last.cells == col => {
                    last.cells += cells_spanned;
                }
                _ => row.backgrounds.push(BgRect {
                    col,
                    cells: cells_spanned,
                    color: bg,
                }),
            }
        }

        let style = RunStyle {
            fg,
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
            strikethrough: cell.flags.contains(Flags::STRIKEOUT),
        };
        let blank_cell = cell.c == ' '
            || cell.c == '\t'
            || cell
                .flags
                .intersects(Flags::HIDDEN | Flags::LEADING_WIDE_CHAR_SPACER);
        let zerowidth = cell.zerowidth().filter(|marks| !marks.is_empty());

        if blank_cell && zerowidth.is_none() {
            match current.as_mut() {
                // A blank inside a run costs one space glyph; splitting the
                // run would cost a whole shaping pass.
                Some(run) if !run.style.decorated() && !style.decorated() => {
                    run.text.push(' ');
                    run.cells += 1;
                }
                Some(run) if run.style == style => {
                    run.text.push(' ');
                    run.cells += 1;
                }
                _ => {
                    flush(&mut row, current.take());
                    if style.decorated() {
                        // An underlined blank is visible: start a run.
                        current = Some(TextRunSpec {
                            col,
                            cells: 1,
                            text: " ".into(),
                            style,
                            grid: RunGrid::Cells,
                        });
                    }
                }
            }
            continue;
        }

        if wide || zerowidth.is_some() {
            flush(&mut row, current.take());
            let mut text = String::from(cell.c);
            if let Some(marks) = zerowidth {
                text.extend(marks.iter());
            }
            row.runs.push(TextRunSpec {
                col,
                cells: cells_spanned,
                text,
                style,
                grid: if zerowidth.is_some() {
                    RunGrid::Cluster
                } else {
                    RunGrid::Wide
                },
            });
            continue;
        }

        match current.as_mut() {
            Some(run) if run.style == style => {
                run.text.push(cell.c);
                run.cells += 1;
            }
            _ => {
                flush(&mut row, current.take());
                current = Some(TextRunSpec {
                    col,
                    cells: 1,
                    text: String::from(cell.c),
                    style,
                    grid: RunGrid::Cells,
                });
            }
        }
    }
    flush(&mut row, current);
    row
}

/// Close a run: drop trailing plain blanks, keep it if anything is left.
fn flush(row: &mut RowLayout, run: Option<TextRunSpec>) {
    let Some(mut run) = run else { return };
    if !run.style.decorated() {
        let trimmed = run.text.trim_end_matches(' ').len();
        let dropped = run.text.len() - trimmed;
        run.text.truncate(trimmed);
        run.cells -= dropped as u16;
    }
    if !run.text.is_empty() {
        row.runs.push(run);
    }
}

pub fn to_hsla(color: Rgb24) -> Hsla {
    gpui::rgb(color).into()
}

fn pack(color: Hsla) -> Rgb24 {
    let rgba = Rgba::from(color);
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(rgba.r) << 16) | (channel(rgba.g) << 8) | channel(rgba.b)
}

fn lighten(color: Hsla, amount: f32) -> Hsla {
    Hsla {
        l: color.l + (1.0 - color.l) * amount,
        ..color
    }
}

fn darken(color: Hsla, amount: f32) -> Hsla {
    Hsla {
        l: color.l * (1.0 - amount),
        ..color
    }
}

/// `from` moved `amount` of the way to `to`, per channel.
fn blend(from: Rgb24, to: Rgb24, amount: f32) -> Rgb24 {
    let mix = |shift: u32| {
        let a = ((from >> shift) & 0xff) as f32;
        let b = ((to >> shift) & 0xff) as f32;
        ((a + (b - a) * amount).round() as u32) << shift
    };
    mix(16) | mix(8) | mix(0)
}

#[cfg(test)]
mod tests {
    use chatty_terminal::alacritty_terminal::Term;
    use chatty_terminal::alacritty_terminal::event::VoidListener;
    use chatty_terminal::alacritty_terminal::grid::Dimensions;
    use chatty_terminal::alacritty_terminal::index::Line;
    use chatty_terminal::alacritty_terminal::vte::ansi::{Processor, Rgb};

    use super::*;

    const DARK: fn() -> Palette = || Palette::fallback(true);

    fn cell(c: char) -> Cell {
        Cell {
            c,
            ..Cell::default()
        }
    }

    fn cells(text: &str) -> Vec<Cell> {
        text.chars().map(cell).collect()
    }

    struct Size(usize, usize);
    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    /// Feed `bytes` to a real terminal and batch its first row.
    fn render(bytes: &[u8], cols: usize) -> RowLayout {
        let mut term = Term::new(Default::default(), &Size(cols, 3), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        batch_row(&term.grid()[Line(0)][..], term.colors(), &DARK())
    }

    fn texts(row: &RowLayout) -> Vec<(&str, u16, u16)> {
        row.runs
            .iter()
            .map(|r| (r.text.as_str(), r.col, r.cells))
            .collect()
    }

    #[test]
    fn plain_row_is_one_run_with_trailing_blanks_trimmed() {
        let mut row = cells("hello world");
        row.extend(cells("      "));
        let layout = batch_row(&row, &Colors::default(), &DARK());
        assert_eq!(texts(&layout), vec![("hello world", 0, 11)]);
        assert!(layout.backgrounds.is_empty());
    }

    #[test]
    fn leading_blanks_are_not_shaped() {
        let layout = batch_row(&cells("   $ ls"), &Colors::default(), &DARK());
        assert_eq!(texts(&layout), vec![("$ ls", 3, 4)]);
    }

    #[test]
    fn blank_row_has_no_runs() {
        let layout = batch_row(&cells("          "), &Colors::default(), &DARK());
        assert_eq!(layout, RowLayout::default());
    }

    #[test]
    fn a_style_change_splits_the_run() {
        // Blue "src", default-colour gap, green "main.rs".
        let row = render(b"\x1b[34msrc\x1b[0m  \x1b[32mmain.rs\x1b[0m", 20);
        let palette = DARK();
        assert_eq!(texts(&row), vec![("src", 0, 3), ("main.rs", 5, 7)]);
        assert_eq!(row.runs[0].style.fg, palette.ansi[4]);
        assert_eq!(row.runs[1].style.fg, palette.ansi[2]);
    }

    #[test]
    fn a_blank_of_another_colour_does_not_split_a_run() {
        // `ls` resets the colour between two blue names; the gap is blank.
        let row = render(b"\x1b[34mdocs\x1b[0m \x1b[34msrc\x1b[0m", 20);
        assert_eq!(texts(&row), vec![("docs src", 0, 8)]);
    }

    #[test]
    fn many_cells_few_runs() {
        // A full 200-column line of one style must be one run, not 200.
        let line = "y".repeat(200);
        let layout = batch_row(&cells(&line), &Colors::default(), &DARK());
        assert_eq!(layout.runs.len(), 1);
        assert_eq!(layout.runs[0].cells, 200);
    }

    #[test]
    fn bold_italic_underline_strike_are_distinct_styles() {
        let row = render(b"a\x1b[1mb\x1b[22;3mc\x1b[23;4md\x1b[24;9me\x1b[0m", 10);
        let styles: Vec<_> = row.runs.iter().map(|r| r.style).collect();
        assert_eq!(styles.len(), 5);
        assert!(styles[1].bold && !styles[1].italic);
        assert!(styles[2].italic && !styles[2].bold);
        assert!(styles[3].underline);
        assert!(styles[4].strikethrough);
    }

    #[test]
    fn underlined_blanks_stay_visible() {
        let row = render(b"\x1b[4mhi  \x1b[0m", 10);
        assert_eq!(texts(&row), vec![("hi  ", 0, 4)]);
    }

    #[test]
    fn backgrounds_merge_across_styles_and_skip_the_default() {
        // Same red background under a bold and a plain word, then default.
        let row = render(b"\x1b[41;1mab\x1b[22mcd\x1b[0m ef", 10);
        let palette = DARK();
        assert_eq!(
            row.backgrounds,
            vec![BgRect {
                col: 0,
                cells: 4,
                color: palette.ansi[1]
            }]
        );
        assert_eq!(row.runs.len(), 2, "bold and plain are separate text runs");
    }

    #[test]
    fn wide_chars_take_two_cells_in_their_own_run() {
        let row = render("ab你好c".as_bytes(), 10);
        assert_eq!(
            texts(&row),
            vec![("ab", 0, 2), ("你", 2, 2), ("好", 4, 2), ("c", 6, 1)]
        );
        assert_eq!(row.runs[1].grid, RunGrid::Wide);
        assert_eq!(row.runs[0].grid, RunGrid::Cells);
    }

    #[test]
    fn combining_marks_are_shaped_as_a_cluster() {
        let row = render("xe\u{301}y".as_bytes(), 10);
        assert_eq!(
            texts(&row),
            vec![("x", 0, 1), ("e\u{301}", 1, 1), ("y", 2, 1)]
        );
        assert_eq!(row.runs[1].grid, RunGrid::Cluster);
    }

    #[test]
    fn inverse_swaps_and_hidden_hides() {
        let palette = DARK();
        let row = render(b"\x1b[7mX\x1b[0m", 5);
        assert_eq!(row.runs[0].style.fg, palette.background);
        assert_eq!(row.backgrounds[0].color, palette.foreground);

        let hidden = render(b"\x1b[8msecret\x1b[0m", 10);
        assert!(hidden.runs.is_empty());
    }

    #[test]
    fn named_colors_come_from_the_palette() {
        let p = DARK();
        let none = Colors::default();
        assert_eq!(
            resolve_color(Color::Named(NamedColor::Red), &none, &p),
            p.ansi[1]
        );
        assert_eq!(
            resolve_color(Color::Named(NamedColor::BrightCyan), &none, &p),
            p.ansi[14]
        );
        assert_eq!(
            resolve_color(Color::Named(NamedColor::Foreground), &none, &p),
            p.foreground
        );
        assert_eq!(
            resolve_color(Color::Named(NamedColor::Background), &none, &p),
            p.background
        );
        assert_eq!(
            resolve_color(Color::Named(NamedColor::DimRed), &none, &p),
            blend(p.ansi[1], p.background, DIM_TOWARDS_BACKGROUND)
        );
    }

    #[test]
    fn indexed_colors_cover_ansi_cube_and_greys() {
        let p = DARK();
        assert_eq!(p.indexed(3), p.ansi[3]);
        assert_eq!(p.indexed(16), 0x000000);
        assert_eq!(p.indexed(196), 0xff0000);
        assert_eq!(p.indexed(46), 0x00ff00);
        assert_eq!(p.indexed(21), 0x0000ff);
        assert_eq!(p.indexed(110), 0x87afd7);
        assert_eq!(p.indexed(231), 0xffffff);
        assert_eq!(p.indexed(232), 0x080808);
        assert_eq!(p.indexed(255), 0xeeeeee);
    }

    #[test]
    fn truecolor_and_256_colour_sequences_resolve() {
        let row = render(b"\x1b[38;2;18;52;86ma\x1b[38;5;208mb\x1b[0m", 5);
        assert_eq!(row.runs[0].style.fg, 0x123456);
        assert_eq!(row.runs[1].style.fg, 0xff8700);
    }

    #[test]
    fn program_palette_overrides_win() {
        let mut overrides = Colors::default();
        overrides[NamedColor::Red] = Some(Rgb {
            r: 0x12,
            g: 0x34,
            b: 0x56,
        });
        overrides[200] = Some(Rgb { r: 1, g: 2, b: 3 });
        let p = DARK();
        assert_eq!(
            resolve_color(Color::Named(NamedColor::Red), &overrides, &p),
            0x123456
        );
        assert_eq!(resolve_color(Color::Indexed(200), &overrides, &p), 0x010203);
    }

    #[test]
    fn dim_moves_the_foreground_towards_the_background() {
        let p = DARK();
        let row = render(b"\x1b[2mfaint\x1b[0m", 10);
        assert_eq!(
            row.runs[0].style.fg,
            blend(p.foreground, p.background, DIM_TOWARDS_BACKGROUND)
        );
    }

    #[test]
    fn theme_palettes_take_the_theme_colours() {
        for dark in [true, false] {
            let theme = if dark {
                ThemeColor::dark()
            } else {
                ThemeColor::light()
            };
            let p = Palette::from_theme(&theme, dark);
            assert_eq!(p.foreground, pack(theme.foreground));
            assert_eq!(p.background, pack(theme.background));
            assert_eq!(p.cursor, pack(theme.caret));
            // Theme red in its normal or bright slot, per mode.
            let red = pack(theme.red);
            assert_eq!(if dark { p.ansi[1] } else { p.ansi[9] }, red);
            // Blacks and whites from the fallback.
            let fallback = Palette::fallback(dark);
            for i in [0, 7, 8, 15] {
                assert_eq!(p.ansi[i], fallback.ansi[i]);
            }
            // Bright is never darker than normal, in either mode.
            let luma = |c: Rgb24| ((c >> 16) & 0xff) + ((c >> 8) & 0xff) + (c & 0xff);
            for i in 1..=6 {
                assert!(luma(p.ansi[8 + i]) >= luma(p.ansi[i]));
            }
        }
    }

    #[test]
    fn pack_round_trips_through_hsla() {
        for color in [0x000000, 0xffffff, 0x123456, 0xcd3131, 0x87afd7] {
            assert_eq!(pack(to_hsla(color)), color);
        }
    }
}
