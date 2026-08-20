//! Our own LXMF endpoint: the on-disk identity, the `lxmf.delivery`
//! destination it registers, and the framing that turns bytes on the wire into
//! an [`LxMessage`] (and back).
//!
//! Every decode/build path needs the same two things — the identity (to
//! decrypt / sign) and our destination hash (which senders may strip from the
//! payload) — so they live together here instead of being threaded through each
//! call.

use std::path::Path;

use tokio::sync::mpsc;

use lxmf_core::constants::DeliveryMethod;
use lxmf_core::message::LxMessage;
use rns_identity::destination::{DestType, Destination, Direction};
use rns_identity::identity::Identity;
use rns_transport::messages::{OutboundRequest, TransportMessage};

use foxhole_core::app::{NetEvent, Outbound};

use super::codec::parse_hash;
use super::now_secs;
use super::telemetry;

/// LXMF inbox aspect — the full dotted destination name.
pub(crate) const LXMF_DELIVERY: &str = "lxmf.delivery";

/// This terminal's LXMF identity and inbox.
pub(crate) struct Endpoint {
    identity: Identity,
    delivery: Destination,
    /// Our `lxmf.delivery` destination hash — the address peers send to.
    pub(crate) hash: [u8; 16],
    display_name: String,
}

impl Endpoint {
    /// Load (or create and persist) the identity at `id_path` and derive the
    /// `lxmf.delivery` destination from it.
    pub(crate) fn open(id_path: &Path, display_name: &str) -> Result<Self, String> {
        let identity = if id_path.exists() {
            Identity::from_file(id_path).map_err(|e| format!("load identity: {e:?}"))?
        } else {
            // Create the config dir 0700 first so the identity key never lands in a
            // world-traversable directory, even briefly.
            if let Some(dir) = id_path.parent() {
                foxhole_core::storage::create_dir_private(dir)
                    .map_err(|e| format!("create config dir: {e}"))?;
            }
            let id = Identity::new();
            id.to_file(id_path)
                .map_err(|e| format!("save identity: {e:?}"))?;
            // The identity's private key is the root secret (it derives every store
            // key); make sure it is owner-only on disk.
            restrict_to_owner(id_path);
            id
        };
        let delivery = Destination::new(
            Some(&identity),
            Direction::In,
            DestType::Single,
            LXMF_DELIVERY,
        )
        .map_err(|e| format!("delivery destination: {e:?}"))?;
        let hash = delivery.hash;
        Ok(Self {
            identity,
            delivery,
            hash,
            display_name: display_name.to_string(),
        })
    }

    /// The underlying identity, for the stack components that take it directly
    /// (link manager, link delivery, propagation client, store-key derivation).
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Our Ed25519 signing key, or an error if this identity is public-only.
    pub(crate) fn signing_key(&self) -> Result<rns_crypto::ed25519::Ed25519PrivateKey, String> {
        self.identity
            .get_signing_key()
            .ok_or_else(|| "identity has no signing key".to_string())
    }

    /// Decode one inbound opportunistic LXMF packet. Mirrors `lxmd`'s
    /// `handle_inbound_packet` + `decrypt_inbound`: strip the Reticulum header,
    /// decrypt with our identity, re-prepend the dest hash (Python strips it for
    /// opportunistic delivery), then unpack. Returns `None` for anything that
    /// isn't a decodable LXMF message (e.g. link packets) — those are ignored.
    pub(crate) fn decode_opportunistic(&self, raw: &[u8]) -> Option<LxMessage> {
        let (_header, header_len) = rns_wire::header::PacketHeader::unpack(raw).ok()?;
        let payload = raw.get(header_len..)?;
        if payload.is_empty() {
            return None;
        }
        let plaintext = self.identity.decrypt(payload, None, false).ok()?;
        LxMessage::unpack(&self.with_dest_prefix(&plaintext)).ok()
    }

    /// Decode an LXMF payload delivered over a link (already decrypted by the
    /// link manager). Mirrors lxmd's `handle_link_delivered_data`: re-prepend the
    /// dest hash if the sender stripped it, then unpack.
    pub(crate) fn decode_link(&self, data: &[u8]) -> Option<LxMessage> {
        LxMessage::unpack(&self.with_dest_prefix(data)).ok()
    }

    /// Decode an LXMF message downloaded from a propagation node. Mirrors lxmd's
    /// `handle_propagation_downloaded_data`: if the blob is addressed to us,
    /// strip the dest hash and decrypt with our identity; then unpack.
    pub(crate) fn decode_propagated(&self, data: &[u8]) -> Option<LxMessage> {
        if data.len() < 16 {
            return None;
        }
        let unpack_data = if data[..16] == self.hash {
            match self.identity.decrypt(&data[16..], None, false) {
                Ok(plaintext) => self.with_dest_prefix(&plaintext),
                Err(_) => data.to_vec(),
            }
        } else {
            data.to_vec()
        };
        LxMessage::unpack(&unpack_data).ok()
    }

    /// Ensure a decrypted payload starts with our destination hash — the form
    /// `LxMessage::unpack` expects. Senders may or may not include it.
    fn with_dest_prefix(&self, plaintext: &[u8]) -> Vec<u8> {
        if plaintext.len() >= 16 && plaintext[..16] == self.hash {
            return plaintext.to_vec();
        }
        let mut d = self.hash.to_vec();
        d.extend_from_slice(plaintext);
        d
    }

    /// Build a signed LXMF message from a UI compose entry, preferring
    /// **Direct** (link) delivery — the priority the user wants and the method
    /// nomadnet uses. The router falls back to Opportunistic only as a last
    /// resort (see `Dispatcher::handle_delivery_result`). The compose target
    /// (`out.peer`) is a hex hash.
    pub(crate) fn build_message(&self, out: &Outbound) -> Result<LxMessage, String> {
        let dest = parse_hash(&out.peer)?;
        let mut msg = LxMessage::new(
            dest,
            self.hash,
            &out.title,
            &out.body,
            DeliveryMethod::Direct,
        );
        // Attach a shared CoT event as the sanctioned intel custom fields (§5),
        // before signing so they're covered by the signature. The type tag is
        // `cot/xml`; the data is the UTF-8 event bytes.
        if let Some(xml) = &out.cot_xml {
            msg.set_field(
                lxmf_core::constants::FIELD_CUSTOM_TYPE,
                foxhole_cot::CONTENT_TAG_XML.as_bytes().to_vec(),
            );
            msg.set_field(
                lxmf_core::constants::FIELD_CUSTOM_DATA,
                xml.as_bytes().to_vec(),
            );
        }
        self.seal(msg)
    }

    /// Build a signed, telemetry-only LXMF reply to `dest`, carrying our
    /// `lat`/`lon`. Mirrors [`Endpoint::build_message`]: empty title/body, the
    /// telemetry rides in `FIELD_TELEMETRY`.
    pub(crate) fn build_telemetry_reply(
        &self,
        dest: [u8; 16],
        lat: f64,
        lon: f64,
    ) -> Result<LxMessage, String> {
        let mut msg = LxMessage::new(dest, self.hash, "", "", DeliveryMethod::Direct);
        let blob = telemetry::pack_location(lat, lon, now_secs() as u64);
        msg.set_field(lxmf_core::constants::FIELD_TELEMETRY, blob);
        self.seal(msg)
    }

    /// Sign a freshly built message and compute its hash (the id everything
    /// downstream — status tracking, router queue, delivery results — keys on).
    fn seal(&self, mut msg: LxMessage) -> Result<LxMessage, String> {
        let signing_key = self.signing_key()?;
        msg.sign(&signing_key).map_err(|e| format!("sign: {e}"))?;
        msg.compute_hash().map_err(|e| format!("hash: {e}"))?;
        Ok(msg)
    }

    /// Build and transmit an announce for our delivery destination.
    pub(crate) async fn announce(
        &mut self,
        transport: &mpsc::Sender<TransportMessage>,
        events: &mpsc::Sender<NetEvent>,
    ) {
        let app_data = lxmf_core::handlers::get_announce_app_data(Some(&self.display_name), None);
        let packet = self.delivery.announce_packet(
            &self.identity,
            Some(&app_data),
            None,
            false,
            None,
            now_secs(),
        );
        match packet {
            Ok(raw) => {
                let _ = transport
                    .send(TransportMessage::Outbound(OutboundRequest {
                        raw: bytes::Bytes::from(raw),
                        destination_hash: self.delivery.hash,
                    }))
                    .await;
                let _ = events
                    .send(NetEvent::Sys("[SYS] announced".to_string()))
                    .await;
            }
            Err(e) => {
                let _ = events
                    .send(NetEvent::Sys(format!("[SYS] announce failed: {e:?}")))
                    .await;
            }
        }
    }
}

/// Best-effort tighten a just-created file to owner read/write only (`0600`) on
/// Unix. Used for the identity key, whose confidentiality the whole at-rest
/// story depends on. A failure here is non-fatal (the file still exists); other
/// platforms rely on their default per-user ACLs.
#[cfg(unix)]
fn restrict_to_owner(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) {}
