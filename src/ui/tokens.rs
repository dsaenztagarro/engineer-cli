// Generated from design/tokens.toml by tests/tokens.rs. Do not edit:
// `UPDATE_TOKENS=1 cargo test --test tokens` regenerates it, and CI fails when it drifts.
//
// One xterm-256 index per decision token. Options are not emitted, because a
// surface names where a colour applies, never the hue itself.

// Every decision is emitted, including the terminal's own foreground and ground,
// which nothing paints.
#![allow(dead_code)]

pub const ACCENT: u8 = 105; // #8787ff, periwinkle
pub const ACCENT_DIM: u8 = 103; // #8787af, heather
pub const RULE: u8 = 244; // #808080, grey
pub const TEXT_PRIMARY: u8 = 253; // #dadada, ink
pub const TEXT_SECONDARY: u8 = 244; // #808080, grey
pub const TEXT_INVERSE: u8 = 233; // #121212, paper
pub const SURFACE: u8 = 233; // #121212, paper
pub const ERROR: u8 = 167; // #d75f5f, red
pub const NOTICE_SUCCESS: u8 = 108; // #87af87, sage
pub const NOTICE_WARNING: u8 = 179; // #d7af5f, amber
