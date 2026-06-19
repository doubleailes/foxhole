#!/usr/bin/env python3
"""Inject a CoT-bearing LXMF message onto a Reticulum network.

Reference sender + dev fixture for foxhole's CoT-over-Reticulum intel ingest
(docs/intel-sharing.md, Appendix A). Two jobs:

- Dev fixture (offline): ``--dry-run`` prints the CoT <event> XML — paste it
  into a foxhole-cot decoder unit test (the capture-as-fixture discipline used
  for the telemetry formats).
- Live ingest test: with ``--to <foxhole-hash>`` it delivers the event over
  Reticulum, so the P2 ingest path can be exercised without a second foxhole or
  ATAK. The sender uses a fresh identity, so foxhole sees it as an *Unknown*
  peer — handy for testing the trust-gating / staging path.

It pins the on-wire framing from the design note (§5): FIELD_CUSTOM_TYPE (0xFB)
= "cot/xml", FIELD_CUSTOM_DATA (0xFC) = the UTF-8 event bytes, plus a
human-readable summary in the message body.

  # live send (marker)
  python cot_inject.py --to e0f0216ad3841468f909ff12fdbb250e \
      --type a-f-G-U-C --callsign OP-1 --lat 48.86 --lon 2.29

  # live send (hostile hazard zone, 400 km radius, 6 h stale)
  python cot_inject.py --to <hash> --type a-h-G-U-C --callsign "AO ALPHA" \
      --lat 50.40 --lon 30.50 --radius 400000 --stale 21600 \
      --remarks "shelling reported"

  # just print the CoT XML (no network) -> copy into a decoder test
  python cot_inject.py --dry-run --type a-u-G --callsign MARK-1 --lat 0 --lon 0

Prereqs: ``pip install rns lxmf``, and a Reticulum config that shares a network
with the foxhole node (the recipient must have announced so a path is known).
"""
import argparse, time
from datetime import datetime, timedelta, timezone
from xml.sax.saxutils import escape, quoteattr


def cot_event(uid, cot_type, lat, lon, callsign, stale_s, remarks, radius_m):
    now = datetime.now(timezone.utc)
    iso = lambda t: t.strftime("%Y-%m-%dT%H:%M:%S.000Z")
    detail = f"<contact callsign={quoteattr(callsign)}/>"
    if remarks:
        detail += f"<remarks>{escape(remarks)}</remarks>"
    if radius_m:  # a point with a radius == a circular zone
        detail += f'<shape><ellipse major="{radius_m}" minor="{radius_m}" angle="0"/></shape>'
    return (
        '<?xml version="1.0" standalone="yes"?>'
        f"<event version=\"2.0\" uid={quoteattr(uid)} type={quoteattr(cot_type)} "
        f'how="h-g-i-g-o" time="{iso(now)}" start="{iso(now)}" '
        f'stale="{iso(now + timedelta(seconds=stale_s))}">'
        f'<point lat="{lat:.6f}" lon="{lon:.6f}" hae="0.0" ce="9999999.0" le="9999999.0"/>'
        f"<detail>{detail}</detail></event>"
    ).encode("utf-8")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--to", help="recipient lxmf.delivery hash (hex)")
    ap.add_argument("--type", default="a-u-G", help="CoT type code")
    ap.add_argument("--callsign", default="MARK-1")
    ap.add_argument("--lat", type=float, required=True)
    ap.add_argument("--lon", type=float, required=True)
    ap.add_argument("--radius", type=int, default=0, help="zone radius in metres (0 = point)")
    ap.add_argument("--remarks", default="")
    ap.add_argument("--stale", type=int, default=21600, help="seconds until stale")
    ap.add_argument("--uid", default=None)
    ap.add_argument("--config", default=None, help="Reticulum config dir")
    ap.add_argument("--dry-run", action="store_true", help="print the CoT XML and exit")
    a = ap.parse_args()

    uid = a.uid or f"foxhole-{a.callsign}-{int(time.time())}"
    xml = cot_event(uid, a.type, a.lat, a.lon, a.callsign, a.stale, a.remarks, a.radius or None)
    summary = f"INTEL: {a.callsign} ({a.type}) @ {a.lat:.4f},{a.lon:.4f}" + (
        f" r{a.radius}m" if a.radius else "")

    if a.dry_run:
        print(xml.decode())
        return
    if not a.to:
        ap.error("--to is required unless --dry-run")

    # Accept a hash with the usual separators (colons/spaces/angle brackets) and
    # validate it before touching the network, so a placeholder or typo gives a
    # clear message instead of a hex traceback.
    hex_to = "".join(c for c in a.to if c in "0123456789abcdefABCDEF")
    if len(hex_to) != 32:
        ap.error(
            f"--to must be a 32-hex-char lxmf.delivery hash, got {a.to!r} "
            "(it's the address foxhole shows in its status bar / Log tab; "
            "don't paste the literal <foxhole-hash> placeholder)"
        )

    import RNS, LXMF  # constants are 0xFB / 0xFC if your LXMF doesn't export the names
    RNS.Reticulum(a.config)
    router = LXMF.LXMRouter(storagepath="./.cot_inject_lxm")
    source = router.register_delivery_identity(RNS.Identity(), display_name="cot-inject")
    router.announce(source.hash)

    dest_hash = bytes.fromhex(hex_to)
    if not RNS.Transport.has_path(dest_hash):
        RNS.Transport.request_path(dest_hash)
        deadline = time.time() + 15
        while not RNS.Transport.has_path(dest_hash) and time.time() < deadline:
            time.sleep(0.1)
    recipient = RNS.Identity.recall(dest_hash)
    if recipient is None:
        raise SystemExit("no path/identity yet — has the recipient announced?")

    dest = RNS.Destination(recipient, RNS.Destination.OUT, RNS.Destination.SINGLE,
                           "lxmf", "delivery")
    lxm = LXMF.LXMessage(dest, source, summary, "", fields={
        LXMF.FIELD_CUSTOM_TYPE: "cot/xml",
        LXMF.FIELD_CUSTOM_DATA: xml,
    }, desired_method=LXMF.LXMessage.DIRECT)
    router.handle_outbound(lxm)

    done = (LXMF.LXMessage.SENT, LXMF.LXMessage.DELIVERED, LXMF.LXMessage.FAILED)
    deadline = time.time() + 30
    while lxm.state not in done and time.time() < deadline:
        time.sleep(0.1)
    print("uid:", uid, "state:", lxm.state)


if __name__ == "__main__":
    main()
