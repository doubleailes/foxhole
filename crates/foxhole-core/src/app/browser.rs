//! Browser tool: the Nomad Network node list and micron page viewport —
//! navigation, form-field editing, link following, history, and folding fetch
//! results back into the view.

use super::*;

impl App {
    /// Browser: two panes (node list / page viewport), switched with Tab.
    /// Nodes — Up/Down select, Enter/`g` open the node's index. Page — Up/Down
    /// move the element cursor, type into a focused field, Enter follow a link,
    /// Backspace back. `r` reloads (unless a field is being edited).
    pub(super) fn handle_browser_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab => {
                self.browser_pane = match self.browser_pane {
                    BrowserPane::Nodes => BrowserPane::Page,
                    BrowserPane::Page => BrowserPane::Nodes,
                };
            }
            KeyCode::Char('r') if !self.editing_field() => self.reload(),
            _ => match self.browser_pane {
                BrowserPane::Nodes => self.handle_browser_nodes_key(key),
                BrowserPane::Page => self.handle_browser_page_key(key),
            },
        }
    }

    /// Node-list pane keys.
    fn handle_browser_nodes_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.browser_selected = self.browser_selected.saturating_sub(1),
            KeyCode::Down => {
                if self.browser_selected + 1 < self.nomad_nodes.len() {
                    self.browser_selected += 1;
                }
            }
            KeyCode::Enter | KeyCode::Char('g') => {
                if let Some(node) = self.nomad_nodes.get(self.browser_selected) {
                    let id = node.identity.clone();
                    self.fetch_page(id, "/page/index.mu".to_string(), Vec::new(), true);
                    self.browser_pane = BrowserPane::Page; // focus the page to read/follow
                }
            }
            _ => {}
        }
    }

    /// Page-viewport pane keys: element navigation, field editing, link follow.
    fn handle_browser_page_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.move_element(-1),
            KeyCode::Down => self.move_element(1),
            // A focused text field captures typing; otherwise these act on links.
            KeyCode::Char(c) if self.focused_field_name().is_some() => self.field_push(c),
            KeyCode::Backspace if self.focused_field_name().is_some() => self.field_pop(),
            KeyCode::Enter => self.follow_link(),
            KeyCode::Backspace => self.go_back(),
            _ => {}
        }
    }

    /// Move the page-element cursor by `delta`, clamped.
    fn move_element(&mut self, delta: isize) {
        if let Some(p) = &mut self.page {
            let n = p.elements.len();
            if n == 0 {
                return;
            }
            let cur = p.element_sel as isize;
            p.element_sel = cur.saturating_add(delta).clamp(0, n as isize - 1) as usize;
        }
    }

    /// Name of the focused element if it is a text field.
    fn focused_field_name(&self) -> Option<String> {
        let p = self.page.as_ref()?;
        match p.elements.get(p.element_sel)? {
            crate::micron::Element::Field { name, .. } => Some(name.clone()),
            _ => None,
        }
    }

    /// Whether the operator is editing a page text field (so `r` types instead
    /// of reloading).
    fn editing_field(&self) -> bool {
        self.active == Tool::Browser
            && self.browser_pane == BrowserPane::Page
            && self.focused_field_name().is_some()
    }

    /// Append a char to the focused field's value (end-insert editing).
    fn field_push(&mut self, c: char) {
        if let Some(name) = self.focused_field_name()
            && let Some(p) = &mut self.page
        {
            p.field_values.entry(name).or_default().push(c);
        }
    }

    /// Delete the last char of the focused field's value.
    fn field_pop(&mut self) {
        if let Some(name) = self.focused_field_name()
            && let Some(p) = &mut self.page
        {
            p.field_values.entry(name).or_default().pop();
        }
    }

    /// Reload the current page (no history push).
    fn reload(&mut self) {
        if let Some(p) = &self.page {
            let (node, path) = (p.node.clone(), p.path.clone());
            self.fetch_page(node, path, Vec::new(), false);
        }
    }

    /// Follow the focused link, submitting its form fields if it has any.
    fn follow_link(&mut self) {
        let Some(crate::micron::Element::Link { target, fields }) = self
            .page
            .as_ref()
            .and_then(|p| p.elements.get(p.element_sel))
        else {
            return;
        };
        let (target, fields) = (target.clone(), fields.clone());
        match self.resolve_link(&target) {
            Some((identity, path)) => {
                let form = self.collect_form(&fields);
                self.fetch_page(identity, path, form, true);
            }
            None => {
                // Unsupported scheme or an undiscovered node — surface it inline.
                if let Some(p) = &mut self.page {
                    p.status = PageStatus::Error(format!("cannot follow link: {target}"));
                }
            }
        }
    }

    /// Collect a link's form submission per NomadNet: `*` → every field;
    /// `name` → `field_<name>`; `k=v` → `var_<k>`. (Checkbox/radio deferred.)
    fn collect_form(&self, link_fields: &[String]) -> Vec<(String, String)> {
        let Some(p) = &self.page else {
            return Vec::new();
        };
        let all = link_fields.iter().any(|f| f == "*");
        let mut out = Vec::new();
        // Literal `k=v` variables embedded in the link.
        for f in link_fields {
            if let Some((k, v)) = f.split_once('=') {
                out.push((format!("var_{k}"), v.to_string()));
            }
        }
        // Field values (all, or the named ones).
        for el in &p.elements {
            if let crate::micron::Element::Field { name, .. } = el
                && (all || link_fields.iter().any(|f| f == name))
            {
                let value = p.field_values.get(name).cloned().unwrap_or_default();
                out.push((format!("field_{name}"), value));
            }
        }
        out
    }

    /// Resolve a micron link `url` to `(node identity, page path)`.
    /// `:/path` → the current page's node; `<dest>:/path` or `<dest>` → the
    /// discovered node with that destination hash (`/page/index.mu` default).
    /// Returns `None` for unsupported schemes or unknown destinations.
    fn resolve_link(&self, url: &str) -> Option<(String, String)> {
        // Out of scope (Phase 2): LXMF and partial schemes.
        if url.contains('@') || url.starts_with("p:") {
            return None;
        }
        let (host, path) = match url.split_once(':') {
            Some((h, p)) => (h, p.to_string()),
            // No ':' — only a bare 32-hex destination is valid (default page).
            None => (url, "/page/index.mu".to_string()),
        };
        let path = if path.is_empty() {
            "/page/index.mu".to_string()
        } else {
            path
        };
        if host.is_empty() {
            // Relative link — stay on the current page's node.
            return self.page.as_ref().map(|p| (p.node.clone(), path));
        }
        // Absolute link — `host` is a destination hash; map it to a known node.
        let host = host.to_lowercase();
        self.nomad_nodes
            .iter()
            .find(|n| n.dest == host)
            .map(|n| (n.identity.clone(), path))
    }

    /// Go back to the previous page in history (no-op when empty).
    fn go_back(&mut self) {
        if let Some((node, path)) = self.history.pop() {
            self.fetch_page(node, path, Vec::new(), false);
        }
    }

    /// Queue a page fetch and show the fetching state. `fields` is the form
    /// submission (empty for a plain GET). When `push_history`, the current loaded
    /// page is pushed onto the back stack first. Skips a duplicate fetch already
    /// in flight for the same page.
    fn fetch_page(
        &mut self,
        identity: String,
        path: String,
        fields: Vec<(String, String)>,
        push_history: bool,
    ) {
        let already = matches!(
            &self.page,
            Some(p) if p.node == identity && p.path == path && matches!(p.status, PageStatus::Fetching)
        );
        if already {
            return;
        }
        if push_history && let Some(p) = &self.page {
            self.history.push((p.node.clone(), p.path.clone()));
        }
        self.page_scroll.to_top(); // each navigation opens at the top
        self.commands.push_back(NetCommand::FetchPage {
            identity: identity.clone(),
            path: path.clone(),
            fields,
        });
        self.page = Some(Page {
            node: identity,
            path,
            status: PageStatus::Fetching,
            elements: Vec::new(),
            element_sel: 0,
            field_values: HashMap::new(),
        });
    }

    /// Record/refresh a discovered Nomad Network node (dedupe by identity hash;
    /// `last_seen` is the announce timestamp). Mirrors [`App::upsert_peer`].
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn upsert_nomad(
        &mut self,
        identity: String,
        dest: String,
        name: Option<String>,
        last_seen: u64,
    ) {
        if let Some(node) = self.nomad_nodes.iter_mut().find(|n| n.identity == identity) {
            if name.is_some() {
                node.name = name;
            }
            node.dest = dest;
            node.last_seen = node.last_seen.max(last_seen);
        } else {
            self.nomad_nodes.push(NomadNode {
                identity,
                dest,
                name,
                last_seen,
            });
        }
    }

    /// Fold a page-fetch result into the Browser view, if it matches the page
    /// the operator is currently looking at. On success, extract its focusable
    /// elements and seed each text field's value from its default.
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn set_page(&mut self, identity: String, path: String, body: Result<String, String>) {
        // Ignore stale results for a page the operator has navigated away from.
        if !matches!(&self.page, Some(p) if p.node == identity && p.path == path) {
            return;
        }
        let (status, elements, field_values) = match body {
            Ok(src) => {
                let elements = crate::micron::elements(&src);
                let mut field_values = HashMap::new();
                for el in &elements {
                    if let crate::micron::Element::Field { name, default, .. } = el {
                        field_values
                            .entry(name.clone())
                            .or_insert_with(|| default.clone());
                    }
                }
                (PageStatus::Loaded(src), elements, field_values)
            }
            Err(e) => (PageStatus::Error(e), Vec::new(), HashMap::new()),
        };
        self.page = Some(Page {
            node: identity,
            path,
            status,
            elements,
            element_sel: 0,
            field_values,
        });
    }
}
