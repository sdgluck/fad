//! One place for every colour, so the whole app reads as one thing.

use ratatui::style::{Color, Modifier, Style};

pub struct Theme {
    pub normal: Style,
    pub dim: Style,
    pub emphasis: Style,
    pub selection: Style,
    pub border: Style,
    pub border_focus: Style,
    pub dir: Style,
    pub bar: Style,
    pub bar_hot: Style,
    pub staged: Style,
    pub staged_badge: Style,
    pub mode_badge: Style,
    pub warn: Style,
    pub skipped: Style,
}

impl Default for Theme {
    fn default() -> Self {
        // Deliberately built from the terminal's own 16 colours rather than
        // fixed RGB: it inherits whatever scheme the user already lives in,
        // and stays legible on light and dark backgrounds alike.
        Theme {
            normal: Style::default(),
            dim: Style::default().fg(Color::DarkGray),
            emphasis: Style::default().add_modifier(Modifier::BOLD),
            selection: Style::default().add_modifier(Modifier::REVERSED),
            border: Style::default().fg(Color::DarkGray),
            border_focus: Style::default().fg(Color::Cyan),
            dir: Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
            bar: Style::default().fg(Color::Cyan),
            bar_hot: Style::default().fg(Color::Magenta),
            staged: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            staged_badge: Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
            mode_badge: Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            warn: Style::default().fg(Color::Yellow),
            skipped: Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        }
    }
}
