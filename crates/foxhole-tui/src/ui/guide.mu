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
  `!(type)`!            Edit the draft (Transmit pane)
  `!Backspace`!         Delete a character from the draft
  `!Ctrl+S`!            Send the draft to the selected peer
  `!Ctrl+X`!            Purge the compose buffer (discard draft)
  `!Ctrl+R`!            Sync now from the propagation node (on demand)

>>Network

  `!Tab / Left / Right`! Switch column: Peers <-> Nodes
  `!Up / Down`!         Move the selection within a column
  `!Enter`!             Peers: open the chat.  Nodes: set propagation node
  `!p`!                 rnpath-style path probe of the focused selection
  `!s`!                 Sync now from the propagation node

>>Browser

  `!Tab`!               Switch column: node list <-> page view
  `!Up / Down`!         Node list: select a node.  Page: move link/field
  `!Enter`!             Open the selected node / follow a link / submit a form
  `!g`!                 Node list: open (go to) the selected node
  `!r`!                 Reload the current page (when not editing a field)
  `!(type)`!            Edit the focused page input field
  `!Backspace`!         Delete in a field, else go back to the previous page

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

>>Security  /  BURN

Ctrl+K opens the burn confirmation. Type BURN and press Enter to zero-overwrite
and unlink every file under the config directory, then quit. This destroys the
identity key, conversation history, and settings. The destroyed key is what
makes the on-disk stores permanently undecryptable — it cannot be undone.
