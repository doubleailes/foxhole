>FoxHole — off-grid, keyboard-only LXMF comms terminal

A monochrome tactical terminal for end-to-end-encrypted LXMF messaging over
Reticulum. No mouse, no telemetry, off-grid by design. Everything below is
reachable from the keyboard alone.

>>Orientation

The UI has two focus tiers, mirroring Nomad Network:

  - `!TOOLS`! are the top-level tabs in the menu strip. Only one tool is shown
    at a time; switch with Ctrl+N / Ctrl+P.
  - `!PANES`! are the bordered regions inside a tool. The active pane has a
    reversed border; cycle them with Tab. Keys act on the active pane, so check
    which border is highlighted first.

>>Tools  (Ctrl+N / Ctrl+P to switch)

  `!Conversations`!   Send and receive LXMF messages. Peer list + thread +
                   compose box.
  `!Network`!         Discovered peers and propagation nodes; set the active
                   propagation node, probe paths.
  `!Browser`!         Read Nomad Network pages written in micron markup; follow
                   links and submit form fields.
  `!Log`!             Timestamped (UTC) system and diagnostic scrollback.
  `!Interfaces`!      Live Reticulum interface status, rnstatus-style.
  `!Notes`!           Ten-slot scratch buffer for hashes, grid refs, anything.
  `!Guide`!           This manual.

>>Global keys  (work in any tool)

  `!Ctrl+N / Ctrl+P`!   Next / previous tool (tab)
  `!Ctrl+O`!            New conversation by LXMF address
  `!Ctrl+K`!            BURN — destroy all session data (confirm: BURN)
  `!Ctrl+Q`!            Quit
  `!PgUp / PgDn`!       Scroll the focused text pane
  `!Home / End`!        Jump to top / bottom of the focused pane

>>Conversations

  `!Tab`!               Cycle panes: Peers -> Thread -> Transmit
  `!Up / Down`!         Select a peer (Peers pane)
  `!t`!                 Cycle the selected peer's trust level (Peers pane)
  `!(type)`!            Edit the draft (Transmit pane)
  `!Backspace`!         Delete a character from the draft
  `!Ctrl+T`!            Edit the message title (toggle title / body)
  `!Ctrl+S`!            Send the draft to the selected peer
  `!Ctrl+G`!            Share / revoke a hazard zone to this peer (CoT intel)
  `!Ctrl+X`!            Purge the compose buffer (discard title + draft)
  `!Ctrl+R`!            Sync now from the propagation node (on demand)

>>Network

  `!Tab / Left / Right`! Switch column: Peers <-> Nodes
  `!Up / Down`!         Move the selection within a column
  `!Enter`!             Peers: open the chat.  Nodes: set propagation node
  `!p`!                 rnpath-style path probe of the focused selection
  `!s`!                 Sync now from the propagation node
  `!m`!                 Show the selection's address as a mnemonic phrase
  `!t`!                 Cycle the selected peer's trust level (Peers column)

>>Browser

  `!Tab`!               Switch column: node list <-> page view
  `!Up / Down`!         Node list: select a node.  Page: move link/field
  `!Enter`!             Open the selected node / follow a link / submit a form
  `!g`!                 Node list: open (go to) the selected node
  `!r`!                 Reload the current page (when not editing a field)
  `!(type)`!            Edit the focused page input field
  `!Backspace`!         Delete in a field, else go back to the previous page

>>World Map

  `!Arrows`!            Pan the viewport
  `!+ / -`!             Zoom in / out
  `!Tab / [ / ]`!       Cycle plotted markers (centres on each)
  `!Enter / c`!         Centre on the selected marker
  `!r`!                 Reset the viewport to the whole globe
  `!i`!                 Review incoming (staged) CoT intel: accept / discard
  `!a`!                 Author a new marker / zone at the map centre
  `!e`!                 Edit the selected intel object in place
  `!x`!                 Remove the selected intel object (local only)

  Received intel is tinted by affiliation (friendly / hostile / neutral /
  unknown); local `*zones.conf`* hazard areas are tagged LOCAL. Unvetted intel
  from unknown/untrusted peers is staged until you accept it (`!i`!). Author your
  own markers/zones with `!a`! (edit `!e`!, remove `!x`! the selected one). Share a
  local hazard zone to a peer with Ctrl+G in the Conversations tool (and `!r`! in
  that picker revokes it — peers drop the object from their map).

>>Message status

Outbound messages carry a delivery marker that updates as the network confirms
each stage:

  `![sending]`!         Handed to the router, not yet on the wire
  `![sent]`!            Transmitted toward the destination
  `![delivered]`!       Confirmed received by the peer (direct link)
  `![propagated]`!      Stored on a propagation node for later pickup
  `![failed]`!          Could not be delivered (no path / no key / dropped)

>>Networking

Direct delivery needs a live path to the peer. When the peer is offline, route
through a `!propagation node`!: pick one in the Network tool (Enter on a node),
then Ctrl+R / s to sync. The active node persists across sessions.

If the menu shows networking offline, the binary was built without the `!net`!
feature — rebuild with: cargo run --features net

>>Notes

A ten-slot scratch buffer (`!0`!–`!9`!) for stashing a hash, a grid reference, a
frequency, or any short string without copy/paste. Slots persist across
restarts (plaintext under the config dir; a BURN destroys them with everything
else).

  `!Up / Down`!         Select a slot
  `!(type)`!            Append to the selected slot
  `!Backspace`!         Delete a character
  `!Ctrl+X`!            Clear the selected slot

>>Trust levels

Each peer carries an operator-assigned trust level, shown as a colour-coded
glyph at the start of its roster row (Conversations and Network tools). Press
`!t`! on the selected peer to cycle it; the choice persists across sessions.

  `!+`!  TRUSTED       vetted by you
  `!?`!  UNKNOWN       not yet assessed (default)
  `!-`!  UNTRUSTED     known, but not vouched for
  `!!`!  COMPROMISED   treat all traffic as hostile

Trust is advisory — a reminder of who you've checked. It is not used for any
cryptographic decision.

>>Mnemonic addresses

A destination hash is 32 hex characters — awkward to read over a radio. Press
`!m`! in the Network tool to render the selected address as a 12-word phrase
(with a checksum word) you can read aloud to verify or share. Conversely, the
New Conversation prompt (Ctrl+O) accepts either 32 hex characters or a 12-word
mnemonic phrase in the address field.

>>Security  /  BURN

Ctrl+K opens the burn confirmation. Type BURN and press Enter to zero-overwrite
and unlink every file under the config directory, then quit. This destroys the
identity key, conversation history, and settings. The destroyed key is what
makes the on-disk stores permanently undecryptable — it cannot be undone.
