//! The chrome glyph set, swapped to pure ASCII under `--ascii`.

use ratatui::widgets::{Block, Borders};

/// Pure-ASCII box-drawing set for `--ascii` mode: `+` corners, `-`/`|` edges. Applied to every
/// bordered block so `--ascii` emits no Unicode line-drawing glyphs.
pub(crate) const ASCII_BORDER: ratatui::symbols::border::Set = ratatui::symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// The chrome glyphs (borders, header icons, hint separators, arrows) the UI draws, swapped to pure
/// ASCII under `--ascii` so the whole interface emits no Unicode. Content (log bodies, labels, values)
/// is never rewritten — only the UI's own decoration. Constructed once per render from
/// [`Options::ascii`](crate::model::Options::ascii).
pub(crate) struct Glyphs {
    pub(crate) ascii: bool,
    pub(crate) logo: &'static str,
    pub(crate) clock: &'static str,
    pub(crate) warn: &'static str,
    pub(crate) dash: &'static str,
    pub(crate) sep: &'static str,
    pub(crate) up: &'static str,
    pub(crate) down: &'static str,
    pub(crate) left: &'static str,
    pub(crate) right: &'static str,
    pub(crate) ellipsis: &'static str,
    pub(crate) vline: &'static str,
}

impl Glyphs {
    pub(crate) fn new(ascii: bool) -> Self {
        if ascii {
            Self {
                ascii,
                logo: "*",
                clock: "",
                warn: "!",
                dash: "-",
                sep: "|",
                up: "^",
                down: "v",
                left: "<",
                right: ">",
                ellipsis: "...",
                vline: "|",
            }
        } else {
            Self {
                ascii,
                logo: "⬤",
                clock: "⏲",
                warn: "⚠",
                dash: "—",
                sep: "·",
                up: "↑",
                down: "↓",
                left: "←",
                right: "→",
                ellipsis: "…",
                vline: "│",
            }
        }
    }

    /// A `Block` with `Borders::ALL`, using the ASCII border set in `--ascii` mode (the default Unicode
    /// set otherwise). All bordered panels route through here so the border style follows the mode.
    pub(crate) fn block(&self) -> Block<'static> {
        let block = Block::default().borders(Borders::ALL);
        if self.ascii {
            block.border_set(ASCII_BORDER)
        } else {
            block
        }
    }

    /// The two-glyph vertical scroll indicator (`↑↓` / `^v`).
    pub(crate) fn scroll(&self) -> String {
        format!("{}{}", self.up, self.down)
    }
}
