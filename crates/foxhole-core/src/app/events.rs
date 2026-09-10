//! Inbound network event → application state.
//!
//! The one fold from [`NetEvent`] into [`App`], routing each variant to the
//! method of the tool that owns it. It lives here, with the state it mutates,
//! rather than in the binary: `main` runs the terminal and the `select!` loop
//! and holds no state rules, so it hands the whole event over and never matches
//! on its variants. That also keeps the routing in one place as variants are
//! added — a new `NetEvent` is a non-exhaustive-match error in the module that
//! owns the state, not in the runtime wiring.
//!
//! While the cold-boot splash is up the same events double as a live readiness
//! monitor: [`mark_boot_from_event`] flips the bring-up lines that a real event
//! can attest to, before the event is applied normally.

use crate::domain::{GeoPos, NetEvent};

use super::App;

#[cfg(feature = "splash")]
use super::{AppState, BootStep};

impl App {
    /// Fold one event from the network task into the UI state.
    pub fn apply_net_event(&mut self, ev: NetEvent) {
        // While the cold-boot splash is up, let the real readiness events flip
        // its bring-up lines to their reported status (live monitor).
        #[cfg(feature = "splash")]
        if self.state == AppState::Splash {
            self.mark_boot_from_event(&ev);
        }

        match ev {
            NetEvent::Sys(line) => self.push_log(line),
            NetEvent::Local(addr) => self.local_address = Some(addr),
            NetEvent::Peer { kind, hash, name } => self.upsert_peer(kind, hash, name),
            NetEvent::Message {
                source,
                title,
                content,
            } => {
                let body = if title.is_empty() {
                    content
                } else {
                    format!("{title}: {content}")
                };
                self.deliver(&source, &body);
            }
            NetEvent::Telemetry { source, lat, lon } => {
                self.set_location(&source, GeoPos::new(lat, lon));
            }
            NetEvent::Cot { source, event } => self.apply_cot(source, event),
            NetEvent::Sync(status) => self.sync_status = status,
            NetEvent::MsgStatus { id, status } => self.set_msg_status(id, status),
            NetEvent::Path { hash, hops, iface } => self.record_path(hash, hops, iface),
            NetEvent::NomadNode {
                identity,
                dest,
                name,
                last_seen,
            } => self.upsert_nomad(identity, dest, name, last_seen),
            NetEvent::Page {
                identity,
                path,
                body,
            } => self.set_page(identity, path, body),
            NetEvent::Interfaces { interfaces, links } => self.set_interfaces(interfaces, links),
            NetEvent::Voice(ev) => self.apply_voice_event(ev),
            // Handled by the runtime's persistence layer (loads history);
            // nothing to fold into UI state here.
            NetEvent::StoreKey(_) => {}
        }
    }

    /// Flip cold-boot lines to their reported status as the real bring-up
    /// events arrive: encrypted store + cache on the store key, mesh + console
    /// on the local address (which also opens the hand-off), and best-effort
    /// accents off the transport/identity banners. Steps not reached this way
    /// still appear on the timer, so a changed banner string only loses an
    /// early accent, never a line.
    #[cfg(feature = "splash")]
    fn mark_boot_from_event(&mut self, ev: &NetEvent) {
        match ev {
            NetEvent::StoreKey(_) => {
                self.mark_boot(BootStep::Store);
                self.mark_boot(BootStep::Cache);
            }
            NetEvent::Local(_) => {
                self.mark_boot(BootStep::Mesh);
                self.mark_boot(BootStep::Console);
            }
            NetEvent::Sys(line) if line.contains("transport online") => {
                self.mark_boot(BootStep::Iface);
            }
            NetEvent::Sys(line) if line.contains("identity ") => {
                self.mark_boot(BootStep::Identity);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Conversation, VoiceEvent};

    /// The fold reaches the state each variant belongs to — including the tool
    /// sub-structs, which is the whole reason it lives beside them.
    #[test]
    fn apply_net_event_routes_to_the_owning_state() {
        let mut app = App::new();

        app.apply_net_event(NetEvent::Local("deadbeef".to_string()));
        assert_eq!(app.local_address.as_deref(), Some("deadbeef"));

        app.apply_net_event(NetEvent::Voice(VoiceEvent::Local("cafe".to_string())));
        assert_eq!(app.voice.local_identity.as_deref(), Some("cafe"));
    }

    /// An empty title must not leave a bare `": "` in front of the body.
    #[test]
    fn a_titled_message_is_prefixed_and_an_untitled_one_is_not() {
        let mut app = App::new();
        app.convs.items.clear();
        app.convs.items.push(Conversation::new("aa11"));

        app.apply_net_event(NetEvent::Message {
            source: "aa11".to_string(),
            title: String::new(),
            content: "bare".to_string(),
        });
        app.apply_net_event(NetEvent::Message {
            source: "aa11".to_string(),
            title: "SITREP".to_string(),
            content: "titled".to_string(),
        });

        let bodies: Vec<&str> = app.convs.items[0]
            .messages
            .iter()
            .map(|e| e.text.as_str())
            .collect();
        assert!(bodies.contains(&"[RX] bare"), "{bodies:?}");
        assert!(bodies.contains(&"[RX] SITREP: titled"), "{bodies:?}");
    }
}
