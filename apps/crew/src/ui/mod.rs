mod input;
mod term;
mod view;

use anyhow::Result;

use crate::config::Config;

pub use term::TerminalState;

pub fn run(t: &mut TerminalState, c: &Config) ->Result<()> {
    loop {
        t.terminal.draw(|f| {
            let block = view::block();
            let square = view::centered_square(f.area());
            f.render_widget(block, square);
        })?;

        if let Some(ev) = TerminalState::poll(c.tick_ms)? {
            if let Some(keycode) = TerminalState::is_keypress(ev) {
                if input::is_quit(keycode) {
                    break;
                }
            }
        }
    }

    return Ok(());
}
