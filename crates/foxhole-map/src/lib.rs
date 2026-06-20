//! FoxHole world-map domain — the logic and data behind the World Map tool,
//! extracted into a standalone, dependency-free crate so the feature is
//! self-contained and instantly testable.
//!
//! This crate is deliberately *pure*: geographic primitives ([`GeoPos`]), the
//! pan/zoom [`MapView`] viewport and what it plots ([`MapMarker`]), the
//! hazard-area overlay ([`Zone`] + [`zones`]), and the embedded capitals/cities
//! gazetteer ([`cities`]). It has no notion of the application state machine,
//! the terminal, or networking.
//!
//! The two surrounding layers bind it in:
//!   * [`foxhole-core`](../foxhole_core/index.html) holds the `App` field state,
//!     routes keys to the [`MapView`] methods here, and builds the [`MapMarker`]
//!     list from the operator's fix and peer telemetry.
//!   * `foxhole-tui` draws a [`MapView`] + markers + zones + cities onto a
//!     ratatui canvas.

pub mod cities;
pub mod geo;
pub mod view;
pub mod zones;

pub use cities::{CITIES, City, CityKind};
pub use geo::{GeoPos, wrap_lon};
pub use view::{MapMarker, MapView, MarkerKind};
pub use zones::Zone;
