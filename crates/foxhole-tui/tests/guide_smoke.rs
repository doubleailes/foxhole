//! Regression guard for the embedded Guide page (`src/ui/guide.mu`): it must
//! stay valid micron that renders without leaking command bytes, and — being a
//! static manual — expose no focusable links or fields.
#[test]
fn guide_renders_cleanly() {
    let src = include_str!("../src/ui/guide.mu");
    let lines = foxhole_tui::micron::render(src, 80, None, &std::collections::HashMap::new());
    assert!(
        lines.len() > 40,
        "expected a substantial page, got {}",
        lines.len()
    );

    for l in &lines {
        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        // No micron command bytes should leak into rendered text.
        for leak in ["`!", "`F", "`f", "`*", "`_", "`["] {
            assert!(!text.contains(leak), "control leak {leak:?} in: {text:?}");
        }
    }

    // The Browser's element walk should find no links/fields (static page).
    assert!(foxhole_tui::micron::elements(src).is_empty());
}
