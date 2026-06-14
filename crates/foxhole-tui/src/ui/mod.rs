//! Rendering layer.
//!
//! Pure functions of `&App` → frame. Field constraints:
//!   * **Tactical frame** — panels are drawn with Unicode box-drawing: resting
//!     panes wear the heavy [`widgets::FRAME_BORDER`] (`┏━┓┃┗┛`) and the focused
//!     pane the double-ruled [`widgets::FOCUS_BORDER`] (`╔═╗║╚╝`), so focus reads
//!     structurally and not by colour alone. This trades the old strict 7-bit
//!     ASCII chrome (which targeted line-printer gear) for the heavier
//!     command-console look — it assumes a UTF-8 terminal.
//!   * **Truecolor tactical theme** — a dark field-night surface ([`style::BG`])
//!     with phosphor-green panels: resting borders dim, the focused border lit,
//!     filled title nameplates, brass callsign/active-tab keys, instrument-cluster
//!     status chips, and a colour-graded `▰▰▱▱` signal meter. Assumes a modern
//!     UTF-8 + 24-bit terminal (Raspberry Pi OS Bookworm's default and friends).
//!   * **Degrades cleanly** — colour only ever *reinforces* hierarchy; focus and
//!     structure still read with colour stripped, carried by border weight (heavy
//!     vs. double), `REVERSED`/bold nameplates, and the `▶` selection chevron.
//!     Scrollback content is also tinted by category (see [`style::tag_style`]):
//!     RX/TX traffic, delivery, link/routing, config, warnings, errors.
//!
//! Layout has two tiers, mirroring Nomadnet: a tab strip selects the active
//! [`Tool`](crate::app::Tool), whose body fills the middle; a shared status bar
//! pins the bottom.
//!
//! The file is split into a shared toolkit ([`style`] colours/text helpers,
//! [`widgets`] bordered panes + scroll), the chrome/overlays ([`chrome`],
//! [`popups`]), and one module per tool body ([`conversations`], [`network`],
//! [`browser`], [`views`]).

mod browser;
mod chrome;
mod conversations;
mod network;
mod notes;
mod popups;
mod style;
#[cfg(test)]
mod tests;
mod views;
mod widgets;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::widgets::Block;

use crate::app::App;

use chrome::{render_status, render_tab_strip, render_tool};
use popups::{render_burn_popup, render_mnemonic_popup, render_new_conv_popup, render_sync_popup};
use style::base_style;

/// Draw the whole interface: the tab strip, the active tool's body (fills all
/// slack), and a fixed status bar.
pub fn render(frame: &mut Frame, app: &App) {
    // The cold-boot splash owns the whole frame until it hands off to console.
    #[cfg(feature = "splash")]
    if app.state == crate::app::AppState::Splash {
        crate::splash::render(frame, app);
        return;
    }

    // Paint the field-night background under everything so the whole console
    // reads as one dark tactical surface, including the gaps between panels.
    frame.render_widget(Block::default().style(base_style()), frame.area());

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Tab strip
            Constraint::Min(3),    // Active tool body — takes all slack
            Constraint::Length(3), // Status / Metadata bar (1 text row + borders)
        ])
        .split(frame.area());

    render_tab_strip(frame, app, chunks[0]);
    render_tool(frame, app, chunks[1]);
    render_status(frame, app, chunks[2]);

    // A propagation sync, when running, overlays a small centered pop-up.
    if let Some(ref status) = app.sync_status {
        render_sync_popup(frame, status);
    }
    // The New Conversation modal is on top of everything when open.
    if let Some(ref nc) = app.new_conv {
        render_new_conv_popup(frame, nc);
    }
    // The read-only mnemonic phrase modal.
    if let Some(ref m) = app.mnemonic_view {
        render_mnemonic_popup(frame, m);
    }
    // The burn notice sits above all else — it's the most consequential action.
    if let Some(ref b) = app.burn_confirm {
        render_burn_popup(frame, b);
    }
}
