//! Rendering layer.
//!
//! Pure functions of `&App` → frame. Field constraints:
//!   * **7-bit ASCII only** — ratatui's default borders are Unicode line-draw
//!     glyphs that corrupt on legacy serial terminals, so every pane uses the
//!     `ASCII_BORDER` set (see [`widgets`]) (`+ - |`).
//!   * **Tactical palette** — scrollback content is tinted by category (see
//!     [`style::tag_style`]): RX/TX traffic, delivery, link/routing, config,
//!     warnings, errors, with muted timestamps. Structure (borders, active-pane
//!     `REVERSED`, titles) stays glyph-only so it still reads on a mono display.
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
mod popups;
mod style;
#[cfg(test)]
mod tests;
mod views;
mod widgets;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::app::App;

use chrome::{render_status, render_tab_strip, render_tool};
use popups::{render_burn_popup, render_mnemonic_popup, render_new_conv_popup, render_sync_popup};

/// Draw the whole interface: the tab strip, the active tool's body (fills all
/// slack), and a fixed status bar.
pub fn render(frame: &mut Frame, app: &App) {
    // The cold-boot splash owns the whole frame until it hands off to console.
    #[cfg(feature = "splash")]
    if app.state == crate::app::AppState::Splash {
        crate::splash::render(frame, app);
        return;
    }

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
