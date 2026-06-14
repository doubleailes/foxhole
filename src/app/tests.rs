//! Unit tests for the `App` state machine and key routing.

use super::*;
use crossterm::event::KeyEventState;

/// Build a press event with no modifiers.
fn press(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Build a Ctrl+<char> press event.
fn ctrl(c: char) -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Type a string into the focused Transmit buffer.
fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle_key(press(KeyCode::Char(c)));
    }
}

/// Just the text of each entry (timestamps are non-deterministic).
fn texts(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|e| e.text.as_str()).collect()
}

/// A fresh app forced into the boot splash (tests otherwise start running).
#[cfg(feature = "splash")]
fn booting() -> App {
    let mut app = App::new();
    app.state = AppState::Splash;
    app.boot = Boot::new();
    app
}

#[cfg(feature = "splash")]
#[test]
fn any_key_dismisses_splash() {
    let mut app = booting();
    app.handle_key(press(KeyCode::Char('x')));
    assert_eq!(app.state, AppState::Running);
}

#[cfg(feature = "splash")]
#[test]
fn boot_reveals_in_order_then_hands_off() {
    let mut app = booting();
    // At tick 0 only the first line shows; the last is still pending.
    assert!(app.boot_done(BootStep::Boot));
    assert!(!app.boot_done(BootStep::Console));
    // The clock must reach the console within the hard cap (via the timed
    // path offline, or the cap under `net` where no real address arrives).
    for _ in 0..=boot::MAX_TICKS {
        if app.state == AppState::Running {
            break;
        }
        app.tick_splash();
    }
    assert_eq!(app.state, AppState::Running);
    assert!(BootStep::ALL.iter().all(|&s| app.boot_done(s)));
}

#[cfg(feature = "splash")]
#[test]
fn local_address_marks_console_and_opens_handoff() {
    let mut app = booting();
    app.mark_boot(BootStep::Console); // what NetEvent::Local triggers
    assert!(app.boot_done(BootStep::Console));
    for _ in 0..=boot::MAX_TICKS {
        if app.state == AppState::Running {
            break;
        }
        app.tick_splash();
    }
    assert_eq!(app.state, AppState::Running);
}

#[test]
fn ctrl_n_p_cycle_tools() {
    let mut app = App::new();
    assert_eq!(app.active, Tool::Conversations);
    app.handle_key(ctrl('n'));
    assert_eq!(app.active, Tool::Network);
    app.handle_key(ctrl('p'));
    assert_eq!(app.active, Tool::Conversations);
    // Wrap backwards from the first tab to the last.
    app.handle_key(ctrl('p'));
    assert_eq!(app.active, Tool::Guide);
}

#[test]
fn tab_cycles_peerlist_thread_transmit() {
    let mut app = App::new();
    assert_eq!(app.focus, Pane::Transmit);
    app.handle_key(press(KeyCode::Tab));
    assert_eq!(app.focus, Pane::PeerList);
    app.handle_key(press(KeyCode::Tab));
    assert_eq!(app.focus, Pane::Thread);
    app.handle_key(press(KeyCode::Tab));
    assert_eq!(app.focus, Pane::Transmit);
}

#[test]
fn up_down_changes_selection_only_in_peerlist() {
    let mut app = App::new();
    assert_eq!(app.selected, 0);

    // Transmit focused: Up/Down do not move the selection.
    app.handle_key(press(KeyCode::Down));
    assert_eq!(
        app.selected, 0,
        "selection only moves with PeerList focused"
    );

    // Focus the peer list, then navigate (clamped at the ends).
    app.focus = Pane::PeerList;
    app.handle_key(press(KeyCode::Down));
    assert_eq!(app.selected, 1);
    app.handle_key(press(KeyCode::Down));
    assert_eq!(app.selected, 2);
    app.handle_key(press(KeyCode::Down)); // clamp at bottom (3 demo peers)
    assert_eq!(app.selected, 2);
    app.handle_key(press(KeyCode::Up));
    assert_eq!(app.selected, 1);
}

#[test]
fn selecting_marks_conversation_read() {
    let mut app = App::new();
    app.deliver("bob", "ping"); // bob is index 1, not selected -> unread
    assert_eq!(app.conversations[1].unread, 1);

    app.focus = Pane::PeerList;
    app.handle_key(press(KeyCode::Down)); // select bob
    assert_eq!(app.selected, 1);
    assert_eq!(app.conversations[1].unread, 0, "viewing clears unread");
}

#[test]
fn drafts_are_per_conversation() {
    let mut app = App::new(); // Transmit focused, alice (0) selected
    type_str(&mut app, "to-alice");
    assert_eq!(app.conversations[0].draft, "to-alice");

    // Switch to bob via the peer list; bob's draft is independent/empty.
    app.focus = Pane::PeerList;
    app.handle_key(press(KeyCode::Down));
    assert_eq!(app.conversations[1].draft, "");

    app.focus = Pane::Transmit;
    type_str(&mut app, "to-bob");
    assert_eq!(app.conversations[1].draft, "to-bob");
    assert_eq!(
        app.conversations[0].draft, "to-alice",
        "alice's draft preserved"
    );
}

#[test]
fn typing_only_edits_when_transmit_focused() {
    let mut app = App::new();
    type_str(&mut app, "hi");
    assert_eq!(app.selected_conv().unwrap().draft, "hi");

    app.focus = Pane::Thread;
    app.handle_key(press(KeyCode::Char('x')));
    assert_eq!(
        app.selected_conv().unwrap().draft,
        "hi",
        "thread pane must not capture text"
    );
}

#[test]
fn typing_is_ignored_outside_conversations() {
    let mut app = App::new();
    app.active = Tool::Network;
    type_str(&mut app, "h");
    app.handle_key(press(KeyCode::Tab));
    assert!(
        app.conversations[0].draft.is_empty(),
        "non-Conversations tools take no compose input"
    );
    assert_eq!(
        app.focus,
        Pane::Transmit,
        "Tab must not move focus off Conversations"
    );
}

#[test]
fn transmit_targets_selected_peer() {
    let mut app = App::new();
    // Select bob (index 1) and give alice a stray draft to prove isolation.
    app.conversations[0].draft = "stray".to_string();
    app.selected = 1;
    app.conversations[1].draft = "  hello bob  ".to_string();

    app.handle_key(ctrl('s'));

    assert_eq!(app.outbound.len(), 1);
    let out = app.outbound.front().unwrap();
    assert_eq!(out.peer, "bob");
    assert_eq!(out.body, "hello bob");
    assert_eq!(
        app.conversations[1].messages.last().unwrap().text,
        "[TX] hello bob"
    );
    assert!(app.conversations[1].draft.is_empty(), "sent draft cleared");
    assert_eq!(
        app.conversations[0].draft, "stray",
        "other drafts untouched"
    );
}

#[test]
fn transmit_ignores_blank_draft() {
    let mut app = App::new();
    app.conversations[0].draft = "   ".to_string();
    app.handle_key(ctrl('s'));
    assert!(app.outbound.is_empty());
}

#[test]
fn purge_clears_selected_draft_only() {
    let mut app = App::new();
    app.conversations[0].draft = "secret".to_string();
    app.conversations[1].draft = "keep".to_string();
    app.handle_key(ctrl('x'));
    assert!(app.conversations[0].draft.is_empty());
    assert_eq!(app.conversations[1].draft, "keep");
    assert!(app.outbound.is_empty());
}

#[test]
fn deliver_routes_to_peer_and_increments_unread() {
    let mut app = App::new();
    // Unknown peer -> conversation is created.
    app.deliver("dave", "first contact");
    let dave = app.conversations.iter().find(|c| c.peer == "dave").unwrap();
    assert_eq!(texts(&dave.messages), vec!["[RX] first contact"]);
    assert_eq!(dave.unread, 1, "unread bumps when not selected");

    // A message to the currently selected peer does not bump unread.
    app.deliver("alice", "yo");
    assert_eq!(app.conversations[0].peer, "alice");
    assert_eq!(app.conversations[0].unread, 0, "selected peer stays read");
}

#[test]
fn push_log_routes_sys_to_syslog() {
    let mut app = App::new();
    app.push_log("[SYS] online".to_string());
    app.push_log("hello there".to_string());
    assert_eq!(texts(&app.syslog), vec!["[SYS] online"]);
    // Non-SYS line lands in the "(direct)" conversation as inbound.
    let direct = app
        .conversations
        .iter()
        .find(|c| c.peer == "(direct)")
        .unwrap();
    assert_eq!(texts(&direct.messages), vec!["[RX] hello there"]);
}

#[test]
fn ctrl_q_requests_quit() {
    let mut app = App::new();
    assert!(!app.should_quit);
    app.handle_key(ctrl('q'));
    assert!(app.should_quit);
}

/// Type a string into the open burn modal.
fn type_burn(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle_key(press(KeyCode::Char(c)));
    }
}

#[test]
fn ctrl_k_opens_burn_and_token_confirms() {
    let mut app = App::new();
    app.handle_key(ctrl('k'));
    assert!(app.burn_confirm.is_some(), "burn modal opened");
    assert!(!app.burn && !app.should_quit);

    type_burn(&mut app, BURN_TOKEN);
    app.handle_key(press(KeyCode::Enter));
    assert!(app.burn, "burn confirmed");
    assert!(app.should_quit, "and quitting");
    assert!(app.burn_confirm.is_none(), "modal closed");
}

#[test]
fn wrong_burn_token_does_not_burn() {
    let mut app = App::new();
    app.handle_key(ctrl('k'));
    type_burn(&mut app, "burn"); // lowercase — not the token
    app.handle_key(press(KeyCode::Enter));
    assert!(!app.burn, "no burn for the wrong token");
    assert!(!app.should_quit);
    assert!(app.burn_confirm.as_ref().unwrap().error, "error flagged");
    // Editing clears the error; the modal stays open until Esc or the token.
    app.handle_key(press(KeyCode::Backspace));
    assert!(!app.burn_confirm.as_ref().unwrap().error);
}

#[test]
fn esc_cancels_burn() {
    let mut app = App::new();
    app.handle_key(ctrl('k'));
    type_burn(&mut app, BURN_TOKEN);
    app.handle_key(press(KeyCode::Esc));
    assert!(app.burn_confirm.is_none(), "cancelled");
    assert!(!app.burn && !app.should_quit, "nothing burned");
}

/// A propagation node with a given hash/name (no last-seen).
fn node(hash: &str, name: Option<&str>) -> Node {
    Node {
        hash: hash.to_string(),
        name: name.map(str::to_string),
        last_seen: 0,
    }
}

#[test]
fn network_tab_selects_and_sets_propagation_node() {
    let mut app = App::new();
    app.active = Tool::Network;
    app.net_col = NetColumn::Nodes; // focus the right column
    let n0 = "aa".repeat(16); // 32 hex chars = 16 bytes
    let n1 = "bb".repeat(16);
    app.nodes = vec![node(&n0, Some("n0")), node(&n1, None)];

    // Down moves the selection and clamps at the last row.
    app.handle_key(press(KeyCode::Down));
    assert_eq!(app.node_selected, 1);
    app.handle_key(press(KeyCode::Down));
    assert_eq!(app.node_selected, 1, "clamped at bottom");
    app.handle_key(press(KeyCode::Up));
    assert_eq!(app.node_selected, 0);

    // Enter activates the highlighted node (config + queued command).
    app.handle_key(press(KeyCode::Enter));
    assert_eq!(app.config.propagation_node.as_deref(), Some(n0.as_str()));
    assert_eq!(
        app.commands.pop_front(),
        Some(NetCommand::SetPropagationNode(Some(n0)))
    );

    // `s` queues a sync.
    app.handle_key(press(KeyCode::Char('s')));
    assert_eq!(app.commands.pop_front(), Some(NetCommand::SyncNow));
}

#[test]
fn network_node_column_inert_with_no_nodes() {
    let mut app = App::new();
    app.active = Tool::Network;
    app.net_col = NetColumn::Nodes;
    app.handle_key(press(KeyCode::Down));
    app.handle_key(press(KeyCode::Enter));
    assert_eq!(app.node_selected, 0);
    assert!(app.config.propagation_node.is_none());
    assert!(
        app.commands.is_empty(),
        "Enter on an empty node list does nothing"
    );
}

#[test]
fn network_columns_toggle_and_navigate_independently() {
    let mut app = App::new();
    app.active = Tool::Network;
    app.nodes = vec![node(&"aa".repeat(16), None), node(&"bb".repeat(16), None)];
    // Defaults to the Peers column.
    assert_eq!(app.net_col, NetColumn::Peers);

    // Up/Down move the peer cursor (seeded with 3 conversations).
    app.handle_key(press(KeyCode::Down));
    assert_eq!(app.selected, 1);
    assert_eq!(app.node_selected, 0, "node cursor untouched while on Peers");

    // Tab / Left / Right switch the focused column.
    app.handle_key(press(KeyCode::Tab));
    assert_eq!(app.net_col, NetColumn::Nodes);
    app.handle_key(press(KeyCode::Down));
    assert_eq!(app.node_selected, 1);
    assert_eq!(app.selected, 1, "peer cursor untouched while on Nodes");
    app.handle_key(press(KeyCode::Left));
    assert_eq!(app.net_col, NetColumn::Peers);
    app.handle_key(press(KeyCode::Right));
    assert_eq!(app.net_col, NetColumn::Nodes);
}

#[test]
fn enter_on_peer_opens_its_conversation() {
    let mut app = App::new();
    app.active = Tool::Network;
    app.net_col = NetColumn::Peers;
    app.selected = 1; // "bob"
    app.handle_key(press(KeyCode::Enter));
    assert_eq!(app.active, Tool::Conversations);
    assert_eq!(app.focus, Pane::Transmit);
    assert_eq!(app.selected, 1);
}

#[test]
fn p_queues_path_probe_for_focused_selection() {
    let mut app = App::new();
    app.active = Tool::Network;
    let n0 = "cc".repeat(16);
    app.nodes = vec![node(&n0, None)];

    // On the Peers column: probes the selected peer's key.
    app.net_col = NetColumn::Peers;
    app.selected = 0;
    let peer = app.conversations[0].peer.clone();
    app.handle_key(press(KeyCode::Char('p')));
    assert_eq!(
        app.commands.pop_front(),
        Some(NetCommand::RequestPath(peer))
    );

    // On the Nodes column: probes the selected node's hash.
    app.net_col = NetColumn::Nodes;
    app.handle_key(press(KeyCode::Char('p')));
    assert_eq!(app.commands.pop_front(), Some(NetCommand::RequestPath(n0)));
}

#[test]
fn upsert_peer_stamps_last_seen() {
    let mut app = App::new();
    let peer = "dd".repeat(16);
    app.upsert_peer(PeerKind::Delivery, peer.clone(), None);
    let conv = app.conversations.iter().find(|c| c.peer == peer).unwrap();
    assert!(conv.last_seen > 0, "delivery peer stamped");

    let nodehash = "ee".repeat(16);
    app.upsert_peer(PeerKind::Propagation, nodehash.clone(), None);
    let n = app.nodes.iter().find(|n| n.hash == nodehash).unwrap();
    assert!(n.last_seen > 0, "propagation node stamped");
}

#[test]
fn record_path_stores_probe_and_logs() {
    let mut app = App::new();
    let hash = "ff".repeat(16);
    app.record_path(hash.clone(), Some(3), Some("AutoInterface".to_string()));
    let p = app.path_probes.get(&hash).expect("probe stored");
    assert_eq!(p.hops, Some(3));
    assert!(
        app.syslog
            .iter()
            .any(|e| e.text.contains("[RT] PATH") && e.text.contains("3 hops via AutoInterface")),
        "an [RT] log line was emitted"
    );
}

#[test]
fn upsert_nomad_dedupes_and_keeps_newest_last_seen() {
    let mut app = App::new();
    let id = "11".repeat(16);
    let dest = "aa".repeat(16);
    app.upsert_nomad(id.clone(), dest.clone(), Some("hub".to_string()), 100);
    app.upsert_nomad(id.clone(), dest.clone(), None, 250); // newer, no name update
    assert_eq!(app.nomad_nodes.len(), 1);
    let n = &app.nomad_nodes[0];
    assert_eq!(n.name.as_deref(), Some("hub"));
    assert_eq!(n.dest, dest);
    assert_eq!(n.last_seen, 250);
}

#[test]
fn browser_enter_queues_index_fetch() {
    let mut app = App::new();
    app.active = Tool::Browser;
    let id = "22".repeat(16);
    app.upsert_nomad(id.clone(), "bb".repeat(16), Some("node".to_string()), 1);
    app.browser_selected = 0;
    app.handle_key(press(KeyCode::Enter));
    assert_eq!(
        app.commands.pop_front(),
        Some(NetCommand::FetchPage {
            identity: id.clone(),
            path: "/page/index.mu".to_string(),
            fields: Vec::new(),
        })
    );
    let page = app.page.as_ref().expect("page set to fetching");
    assert_eq!(page.node, id);
    assert!(matches!(page.status, PageStatus::Fetching));
    // Opening a node focuses the page pane.
    assert_eq!(app.browser_pane, BrowserPane::Page);
}

#[test]
fn set_page_folds_ok_and_err_for_current_page() {
    let mut app = App::new();
    let id = "33".repeat(16);
    let path = "/page/index.mu".to_string();
    let viewing = || Page {
        node: "33".repeat(16),
        path: "/page/index.mu".to_string(),
        status: PageStatus::Fetching,
        elements: Vec::new(),
        element_sel: 0,
        field_values: HashMap::new(),
    };

    app.page = Some(viewing());
    app.set_page(id.clone(), path.clone(), Ok(">Hello".to_string()));
    assert!(matches!(
        app.page.as_ref().unwrap().status,
        PageStatus::Loaded(_)
    ));

    app.page = Some(viewing());
    app.set_page(id, path, Err("timeout".to_string()));
    assert!(matches!(
        app.page.as_ref().unwrap().status,
        PageStatus::Error(_)
    ));

    // A result for a page we're no longer viewing is ignored.
    app.page = Some(viewing());
    app.set_page(
        "99".repeat(16),
        "/other.mu".to_string(),
        Ok("x".to_string()),
    );
    assert!(matches!(
        app.page.as_ref().unwrap().status,
        PageStatus::Fetching
    ));
}

#[test]
fn set_page_extracts_elements_and_seeds_fields() {
    let mut app = App::new();
    let id = "44".repeat(16);
    let path = "/page/index.mu".to_string();
    app.page = Some(Page {
        node: id.clone(),
        path: path.clone(),
        status: PageStatus::Fetching,
        elements: Vec::new(),
        element_sel: 0,
        field_values: HashMap::new(),
    });
    app.set_page(
        id,
        path,
        Ok("`[Home`:/page/a.mu] `<user`alice>".to_string()),
    );
    let p = app.page.as_ref().unwrap();
    // A link element followed by a text field element.
    assert!(matches!(
        p.elements.as_slice(),
        [
            crate::micron::Element::Link { .. },
            crate::micron::Element::Field { .. }
        ]
    ));
    // The field value is seeded from its default.
    assert_eq!(
        p.field_values.get("user").map(String::as_str),
        Some("alice")
    );
}

/// A Browser viewing a loaded page on `node`, with the given link targets as
/// its (link) elements.
fn browsing(node: &str, path: &str, links: Vec<String>) -> App {
    let elements = links
        .into_iter()
        .map(|target| crate::micron::Element::Link {
            target,
            fields: Vec::new(),
        })
        .collect();
    let mut app = App::new();
    app.active = Tool::Browser;
    app.browser_pane = BrowserPane::Page;
    app.page = Some(Page {
        node: node.to_string(),
        path: path.to_string(),
        status: PageStatus::Loaded(String::new()),
        elements,
        element_sel: 0,
        field_values: HashMap::new(),
    });
    app
}

#[test]
fn tab_toggles_browser_pane() {
    let mut app = App::new();
    app.active = Tool::Browser;
    assert_eq!(app.browser_pane, BrowserPane::Nodes);
    app.handle_key(press(KeyCode::Tab));
    assert_eq!(app.browser_pane, BrowserPane::Page);
    app.handle_key(press(KeyCode::Tab));
    assert_eq!(app.browser_pane, BrowserPane::Nodes);
}

#[test]
fn page_pane_element_cursor_clamps() {
    let node = "55".repeat(16);
    let mut app = browsing(
        &node,
        "/page/index.mu",
        vec![":/a.mu".to_string(), ":/b.mu".to_string()],
    );
    app.handle_key(press(KeyCode::Down));
    assert_eq!(app.page.as_ref().unwrap().element_sel, 1);
    app.handle_key(press(KeyCode::Down)); // clamp at last
    assert_eq!(app.page.as_ref().unwrap().element_sel, 1);
    app.handle_key(press(KeyCode::Up));
    assert_eq!(app.page.as_ref().unwrap().element_sel, 0);
}

#[test]
fn relative_link_follows_on_current_node() {
    let node = "66".repeat(16);
    let mut app = browsing(&node, "/page/index.mu", vec![":/page/about.mu".to_string()]);
    app.handle_key(press(KeyCode::Enter)); // follow link 0
    assert_eq!(
        app.commands.pop_front(),
        Some(NetCommand::FetchPage {
            identity: node.clone(),
            path: "/page/about.mu".to_string(),
            fields: Vec::new(),
        })
    );
    // The previous page was pushed to history.
    assert_eq!(app.history.last().unwrap().1, "/page/index.mu");
}

#[test]
fn absolute_link_resolves_known_dest_to_identity() {
    let here = "77".repeat(16);
    let other_id = "88".repeat(16);
    let other_dest = "99".repeat(16);
    let url = format!("{other_dest}:/page/index.mu");
    let mut app = browsing(&here, "/page/index.mu", vec![url]);
    app.upsert_nomad(other_id.clone(), other_dest.clone(), None, 1);
    app.handle_key(press(KeyCode::Enter));
    assert_eq!(
        app.commands.pop_front(),
        Some(NetCommand::FetchPage {
            identity: other_id,
            path: "/page/index.mu".to_string(),
            fields: Vec::new(),
        })
    );
}

#[test]
fn link_to_unknown_dest_errors_without_fetching() {
    let here = "aa".repeat(16);
    let url = format!("{}:/page/x.mu", "bc".repeat(16)); // never discovered
    let mut app = browsing(&here, "/page/index.mu", vec![url]);
    app.handle_key(press(KeyCode::Enter));
    assert!(app.commands.is_empty(), "no fetch for an unknown node");
    assert!(matches!(
        app.page.as_ref().unwrap().status,
        PageStatus::Error(_)
    ));
}

#[test]
fn backspace_pops_history_and_refetches() {
    let node = "cc".repeat(16);
    let mut app = browsing(&node, "/page/two.mu", vec![]);
    app.history.push((node.clone(), "/page/one.mu".to_string()));
    app.handle_key(press(KeyCode::Backspace));
    assert_eq!(
        app.commands.pop_front(),
        Some(NetCommand::FetchPage {
            identity: node,
            path: "/page/one.mu".to_string(),
            fields: Vec::new(),
        })
    );
    assert!(app.history.is_empty(), "history was popped (not re-pushed)");
}

/// A Browser page with the given pre-built elements, focused on the page pane.
fn browsing_elements(node: &str, elements: Vec<crate::micron::Element>) -> App {
    let mut app = App::new();
    app.active = Tool::Browser;
    app.browser_pane = BrowserPane::Page;
    app.page = Some(Page {
        node: node.to_string(),
        path: "/page/index.mu".to_string(),
        status: PageStatus::Loaded(String::new()),
        elements,
        element_sel: 0,
        field_values: HashMap::new(),
    });
    app
}

#[test]
fn typing_into_focused_field_edits_its_value() {
    let mut app = browsing_elements(
        &"dd".repeat(16),
        vec![crate::micron::Element::Field {
            name: "q".to_string(),
            default: String::new(),
        }],
    );
    app.handle_key(press(KeyCode::Char('h')));
    app.handle_key(press(KeyCode::Char('i')));
    assert_eq!(
        app.page
            .as_ref()
            .unwrap()
            .field_values
            .get("q")
            .map(String::as_str),
        Some("hi")
    );
    app.handle_key(press(KeyCode::Backspace));
    assert_eq!(
        app.page
            .as_ref()
            .unwrap()
            .field_values
            .get("q")
            .map(String::as_str),
        Some("h")
    );
}

#[test]
fn submit_link_collects_field_and_var_values() {
    let node = "ee".repeat(16);
    let mut app = browsing_elements(
        &node,
        vec![
            crate::micron::Element::Field {
                name: "q".to_string(),
                default: String::new(),
            },
            crate::micron::Element::Link {
                target: ":/page/search.mu".to_string(),
                fields: vec!["v=1".to_string(), "*".to_string()],
            },
        ],
    );
    app.handle_key(press(KeyCode::Char('x'))); // type into the field
    app.handle_key(press(KeyCode::Down)); // move to the submit link
    app.handle_key(press(KeyCode::Enter));
    let cmd = app.commands.pop_front().expect("a fetch was queued");
    match cmd {
        NetCommand::FetchPage {
            identity,
            path,
            fields,
        } => {
            assert_eq!(identity, node);
            assert_eq!(path, "/page/search.mu");
            assert_eq!(
                fields,
                vec![
                    ("var_v".to_string(), "1".to_string()),
                    ("field_q".to_string(), "x".to_string()),
                ]
            );
        }
        other => panic!("expected FetchPage, got {other:?}"),
    }
}

#[test]
fn scroll_top_paging_clamps_to_content() {
    let s = Scroll::top();
    assert_eq!(s.visible(30, 10), 0, "opens at the top");
    s.page_down(); // step == cached viewport (10)
    assert_eq!(s.visible(30, 10), 10);
    s.page_down();
    s.page_down(); // past the end → clamps at max (30-10)
    assert_eq!(s.visible(30, 10), 20);
    s.to_top();
    assert_eq!(s.visible(30, 10), 0);
}

#[test]
fn scroll_bottom_follows_then_releases_then_resticks() {
    let s = Scroll::bottom();
    assert_eq!(s.visible(30, 10), 20, "starts at the bottom");
    assert_eq!(s.visible(50, 10), 40, "follows new content while stuck");
    s.page_up(); // releases follow, up one viewport from 40
    assert_eq!(s.visible(50, 10), 30);
    assert_eq!(s.visible(60, 10), 30, "no longer yanked to the bottom");
    s.to_bottom();
    assert_eq!(s.visible(60, 10), 50, "End re-engages follow");
}

#[test]
fn pagekeys_scroll_focused_pane() {
    let mut app = App::new();
    app.active = Tool::Browser;
    app.browser_pane = BrowserPane::Page;
    app.page_scroll.visible(100, 10); // prime the viewport step
    app.handle_key(press(KeyCode::PageDown));
    assert_eq!(app.page_scroll.visible(100, 10), 10);
    app.handle_key(press(KeyCode::End));
    assert_eq!(app.page_scroll.visible(100, 10), 90);
    app.handle_key(press(KeyCode::Home));
    assert_eq!(app.page_scroll.visible(100, 10), 0);
}

#[test]
fn active_scroll_follows_focus() {
    let mut app = App::new();
    app.active = Tool::Log;
    assert!(app.active_scroll().is_some());
    app.active = Tool::Network;
    assert!(
        app.active_scroll().is_none(),
        "node columns aren't a text pane"
    );
    app.active = Tool::Browser;
    app.browser_pane = BrowserPane::Nodes;
    assert!(app.active_scroll().is_none(), "node list isn't scrollable");
    app.browser_pane = BrowserPane::Page;
    assert!(app.active_scroll().is_some());
}

#[test]
fn fetch_page_resets_scroll_to_top() {
    let node = "dd".repeat(16);
    let mut app = browsing(&node, "/page/index.mu", vec![]);
    app.page_scroll.visible(100, 10);
    app.page_scroll.page_down(); // scrolled down
    app.browser_pane = BrowserPane::Nodes;
    app.upsert_nomad(node.clone(), "ee".repeat(16), None, 1);
    app.browser_selected = 0;
    app.handle_key(press(KeyCode::Enter)); // open index → resets to top
    assert_eq!(app.page_scroll.visible(100, 10), 0);
}

#[test]
fn start_conversation_validates_and_normalizes() {
    let mut app = App::new();
    let before = app.conversations.len();
    // Colons / spaces / case are tolerated → 32 hex chars.
    assert!(app.start_conversation("A1:b2:c3:d4 e5 f6:00:11:22:33:44:55:66:77:88:99", "Bravo-6"));
    assert_eq!(app.conversations.len(), before + 1);
    let conv = app.conversations.last().unwrap();
    assert_eq!(conv.peer, "a1b2c3d4e5f600112233445566778899");
    assert_eq!(conv.display_name.as_deref(), Some("Bravo-6"));
    assert!(conv.pinned);
    assert_eq!(app.selected, app.conversations.len() - 1);
    assert_eq!(app.active, Tool::Conversations);
    assert_eq!(app.focus, Pane::Transmit);
    assert!(app.dirty.iter().any(|p| p == &conv.peer));
}

#[test]
fn start_conversation_rejects_bad_address() {
    let mut app = App::new();
    let before = app.conversations.len();
    assert!(!app.start_conversation("", ""));
    assert!(!app.start_conversation("abcd", ""), "too short");
    assert!(!app.start_conversation(&"z".repeat(32), ""), "not hex");
    assert_eq!(app.conversations.len(), before);
}

#[test]
fn start_conversation_reuses_existing_and_updates_alias() {
    let mut app = App::new();
    let addr = "ff".repeat(16);
    assert!(app.start_conversation(&addr, ""));
    let n = app.conversations.len();
    assert!(app.start_conversation(&addr, "Renamed"));
    assert_eq!(app.conversations.len(), n, "no duplicate thread");
    let conv = app.conversations.iter().find(|c| c.peer == addr).unwrap();
    assert_eq!(conv.display_name.as_deref(), Some("Renamed"));
}

#[test]
fn new_conv_modal_open_type_confirm() {
    let mut app = App::new();
    app.handle_key(ctrl('o'));
    assert!(app.new_conv.is_some(), "Ctrl+O opens the popup");

    // Modal captures input: address, Tab to alias, then a name.
    type_str(&mut app, &"aa".repeat(16));
    app.handle_key(press(KeyCode::Tab));
    type_str(&mut app, "Alpha");
    app.handle_key(press(KeyCode::Enter));

    assert!(app.new_conv.is_none(), "Enter closes on success");
    let conv = app
        .conversations
        .iter()
        .find(|c| c.peer == "aa".repeat(16))
        .unwrap();
    assert_eq!(conv.display_name.as_deref(), Some("Alpha"));
}

#[test]
fn new_conv_esc_cancels_and_invalid_shows_error() {
    let mut app = App::new();
    app.handle_key(ctrl('o'));
    app.handle_key(press(KeyCode::Char('a')));
    app.handle_key(press(KeyCode::Esc));
    assert!(app.new_conv.is_none(), "Esc cancels");

    app.handle_key(ctrl('o'));
    app.handle_key(press(KeyCode::Char('z'))); // non-hex → normalizes to empty
    app.handle_key(press(KeyCode::Enter));
    assert!(
        app.new_conv.as_ref().is_some_and(|nc| nc.error),
        "stays open with error"
    );
}

#[test]
fn transmit_stamps_id_and_sending_status() {
    let mut app = App::new();
    app.conversations[0].draft = "hi".to_string();
    app.handle_key(ctrl('s'));
    let entry = app.conversations[0].messages.last().unwrap();
    assert!(entry.id > 0);
    assert_eq!(entry.status, MsgStatus::Sending);
    assert_eq!(
        app.outbound.back().unwrap().id,
        entry.id,
        "Outbound shares the id"
    );
}

#[test]
fn set_msg_status_updates_matching_entry_and_marks_dirty() {
    let mut app = App::new();
    app.conversations[0].draft = "yo".to_string();
    app.handle_key(ctrl('s'));
    let id = app.conversations[0].messages.last().unwrap().id;
    app.dirty.clear();

    app.set_msg_status(id, MsgStatus::Delivered);
    assert_eq!(
        app.conversations[0].messages.last().unwrap().status,
        MsgStatus::Delivered
    );
    let peer = app.conversations[0].peer.clone();
    assert!(app.dirty.iter().any(|p| p == &peer));

    app.set_msg_status(999_999, MsgStatus::Failed); // unknown id: no-op
}
