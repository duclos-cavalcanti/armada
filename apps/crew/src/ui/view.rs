use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};

pub fn block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" crew ")
}

pub fn centered_square(area: Rect) -> Rect {
    let side = area.height.saturating_sub(2).min(area.width / 2).max(1);
    let (w, h) = (side * 2, side);
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

