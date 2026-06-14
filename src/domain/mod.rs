//! Shared domain model.
//!
//! The protocol-facing data types and small pure helpers that the whole program
//! agrees on: conversations and their scrollback ([`Conversation`], [`Entry`],
//! [`MsgStatus`]), the events/commands crossing the UI↔network boundary
//! ([`NetEvent`], [`NetCommand`], [`Outbound`], [`PeerKind`]), and the registries
//! the Network/Browser tools display ([`Node`], [`PathProbe`], [`NomadNode`],
//! [`Page`]).
//!
//! These carry no UI focus or navigation semantics (those live in
//! [`crate::app`]); they are the model that `store`, `net`, `ui`, and the state
//! machine all import, which is why they live apart from the controller.

use std::collections::HashMap;

/// A command from the UI down to the network task (mirrors [`NetEvent`] in the
/// other direction). Produced by Network-tab key handling, drained by `main`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetCommand {
    /// Make this hex destination hash the active propagation node (or clear it).
    SetPropagationNode(Option<String>),
    /// Pull queued messages from the configured propagation node now.
    SyncNow,
    /// Operator-initiated path probe (rnpath-style) for a hex destination hash:
    /// request a path, then report the hop count + next-hop interface.
    RequestPath(String),
    /// Fetch a Nomad Network page: `identity` is the node's hex identity hash,
    /// `path` the micron page path (e.g. `/page/index.mu`), `fields` the form
    /// submission as `(key, value)` pairs (`field_…`/`var_…`), empty for a GET.
    FetchPage {
        identity: String,
        path: String,
        fields: Vec<(String, String)>,
    },
}

/// A message accepted for transmission, carrying its destination so the
/// protocol task knows which peer to address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outbound {
    /// Correlation id linking this send to its thread entry (for status updates).
    pub id: u64,
    /// Destination peer (display name offline; hex destination hash under `net`).
    pub peer: String,
    /// The message text.
    pub body: String,
}

/// Whether a discovered peer is a messaging endpoint or a propagation relay.
// Only constructed by the `net`-gated module; offline these variants are unused.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerKind {
    /// An `lxmf.delivery` destination — a peer we can hold a conversation with.
    Delivery,
    /// An `lxmf.propagation` node — a store-and-forward relay (Network tab only).
    Propagation,
}

/// Events flowing from the network task up to the UI over an mpsc channel.
/// Defined here (not in the `net`-gated module) because the offline stub and
/// the event loop both speak it regardless of the feature. The `Peer`/`Message`
/// variants are only produced under `net`.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub enum NetEvent {
    /// A status/diagnostic line destined for the Log tab.
    Sys(String),
    /// This node's own `lxmf.delivery` address (hex) — what peers send to.
    Local(String),
    /// A peer discovered via an announce. `hash` is the hex destination hash.
    Peer {
        kind: PeerKind,
        hash: String,
        name: Option<String>,
    },
    /// An inbound LXMF message. `source` is the hex destination hash.
    Message {
        source: String,
        title: String,
        content: String,
    },
    /// Propagation-sync progress: `Some(status)` shows the pop-up, `None` hides it.
    Sync(Option<String>),
    /// The derived 64-byte conversation-store key, handed up once at startup so
    /// `main` can load history and persist new messages.
    StoreKey([u8; 64]),
    /// Delivery-status update for an outbound message (by its correlation id).
    MsgStatus { id: u64, status: MsgStatus },
    /// Result of an rnpath-style path probe for a hex destination hash:
    /// `hops`/`iface` are `None` when no path is known.
    Path {
        hash: String,
        hops: Option<u8>,
        iface: Option<String>,
    },
    /// A discovered Nomad Network node (from the recent-announce cache).
    /// `identity` is its hex identity hash (what a page fetch addresses), `dest`
    /// its hex destination hash (what links embed); `last_seen` is the announce
    /// timestamp (Unix epoch seconds, UTC).
    NomadNode {
        identity: String,
        dest: String,
        name: Option<String>,
        last_seen: u64,
    },
    /// Result of a Nomad Network page fetch: `body` is the raw micron source on
    /// success, or an error message.
    Page {
        identity: String,
        path: String,
        body: Result<String, String>,
    },
}

/// A discovered propagation node, shown in the Network tab.
pub struct Node {
    /// Hex destination hash (the registry key).
    pub hash: String,
    /// Announced display name, if any.
    pub name: Option<String>,
    /// When this node was last heard via an announce (Unix epoch **seconds,
    /// UTC**); `0` if never (e.g. seeded offline). Rendered as `--:--:--`.
    pub last_seen: u64,
}

/// The last rnpath-style probe result for a destination, shown in the Network
/// tab and logged. `hops`/`iface` are `None` when no path is known.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub struct PathProbe {
    /// When the probe resolved (Unix epoch **seconds, UTC**).
    pub at: u64,
    pub hops: Option<u8>,
    pub iface: Option<String>,
}

/// A discovered Nomad Network node, shown in the Browser tab. Keyed by the
/// node's hex **identity** hash — what a page fetch addresses.
pub struct NomadNode {
    /// Hex identity hash (`sha256(public_key)[..16]`).
    pub identity: String,
    /// Hex **destination** hash — what micron links embed; lets the Browser
    /// resolve a cross-node link back to this node's identity.
    pub dest: String,
    /// Announced node name, if any.
    pub name: Option<String>,
    /// When last heard via the announce cache (Unix epoch **seconds, UTC**).
    pub last_seen: u64,
}

impl NomadNode {
    /// What to show in the node list: the name, else a shortened identity hash.
    pub fn label(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() => n.clone(),
            _ => format!("{}\u{2026}", short_hash(&self.identity)),
        }
    }
}

/// A Nomad Network page being viewed in the Browser tab.
pub struct Page {
    /// Hex identity hash of the serving node.
    pub node: String,
    /// Micron page path (e.g. `/page/index.mu`).
    pub path: String,
    /// Fetch progress / outcome.
    pub status: PageStatus,
    /// Focusable elements (links + text fields) in document order, from
    /// `micron::elements`. Empty while fetching/errored.
    pub elements: Vec<crate::micron::Element>,
    /// The focused element index within `elements` (Page-pane navigation).
    pub element_sel: usize,
    /// Current text-field values by name (seeded from each field's default).
    pub field_values: HashMap<String, String>,
}

/// Lifecycle of a page fetch.
pub enum PageStatus {
    /// The fetch is in flight.
    Fetching,
    /// Loaded: the raw micron source (rendered by `crate::micron`).
    Loaded(String),
    /// The fetch failed; the string is a human-readable reason.
    Error(String),
}

/// Delivery state of an outbound message, shown inline in the thread. `None`
/// for inbound/system lines (no marker). The terminal states are produced by
/// the `net` delivery path.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MsgStatus {
    /// Inbound `[RX]` / `[SYS]` / informational — no status marker.
    #[default]
    None,
    /// `[TX]` queued / in flight.
    Sending,
    /// Transmitted opportunistically (single packet, no delivery proof).
    Sent,
    /// Direct (link) delivery confirmed by a proof.
    Delivered,
    /// Deposited to a propagation node; the recipient pulls it later.
    Propagated,
    /// Delivery was attempted and ultimately gave up.
    Failed,
}

/// One scrollback line: when it occurred (Unix epoch **seconds, UTC**) and its
/// text. `at == 0` marks an unknown time (rendered `--:--:--`). The timestamp is
/// captured at creation so the UI can show it without re-reading the clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub at: u64,
    pub text: String,
    /// Correlation id for outbound messages (matches an `Outbound`). 0 = none.
    /// Session-local — not persisted.
    pub id: u64,
    /// Delivery status (outbound only).
    pub status: MsgStatus,
}

impl Entry {
    /// An entry stamped with the current UTC time (no id, no status).
    pub fn now(text: String) -> Self {
        Self {
            at: now_secs(),
            text,
            id: 0,
            status: MsgStatus::None,
        }
    }
}

/// A single peer conversation: its message history and an unsent draft.
pub struct Conversation {
    /// Routing key: a display name offline, or the hex destination hash under
    /// `net`. Outbound messages address this verbatim.
    pub peer: String,
    /// Human-friendly name when known (from an announce); the peer list prefers
    /// it over the raw key.
    pub display_name: Option<String>,
    /// Scrollback (oldest first): local `[TX]` echoes and inbound `[RX]` lines,
    /// each timestamped at receipt/send.
    pub messages: Vec<Entry>,
    /// Per-conversation compose buffer, preserved across peer switches.
    pub draft: String,
    /// Inbound messages received since this conversation was last viewed.
    pub unread: usize,
    /// Manually added (Ctrl+O) — persist it even with no messages yet so a peer
    /// you entered by hand survives a restart. Transient (not serialized): its
    /// only job is to force the first save, after which the file persists.
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub pinned: bool,
    /// When this peer was last heard via an announce (Unix epoch **seconds,
    /// UTC**); `0` if never (seeded/offline). Shown in the Network tab.
    pub last_seen: u64,
}

impl Conversation {
    /// A fresh, empty conversation with the given peer key.
    pub fn new(peer: impl Into<String>) -> Self {
        Self {
            peer: peer.into(),
            display_name: None,
            messages: Vec::new(),
            draft: String::new(),
            unread: 0,
            pinned: false,
            last_seen: 0,
        }
    }

    /// What to show in the peer list: the display name, else a shortened key.
    pub fn label(&self) -> String {
        match &self.display_name {
            Some(n) if !n.is_empty() => n.clone(),
            _ if self.peer.len() > 10 => format!("{}\u{2026}", &self.peer[..10]),
            _ => self.peer.clone(),
        }
    }
}

/// Current Unix time in whole seconds (UTC); 0 if the clock predates the epoch.
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// First 8 chars of a hex hash for compact display (whole thing if shorter).
pub(crate) fn short_hash(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
}

/// Render an rnpath probe outcome: `"3 hops via AutoInterface"`, `"1 hop via …"`,
/// or `"no path"` when unresolved.
pub fn path_summary(hops: Option<u8>, iface: Option<&str>) -> String {
    match hops {
        Some(n) => {
            let unit = if n == 1 { "hop" } else { "hops" };
            format!("{n} {unit} via {}", iface.unwrap_or("?"))
        }
        None => "no path".to_string(),
    }
}

/// Reduce a typed LXMF address to canonical form: lowercase hex digits only, so
/// `a1:b2 c3`, `A1B2C3`, and `<a1b2c3>` all normalize the same. The caller
/// validates the length (a destination hash is 16 bytes → 32 hex chars).
pub(crate) fn normalize_address(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
