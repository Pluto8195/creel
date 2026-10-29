//! Layout heuristics for render checks. The in-frame probe
//! ([`super::document::probe_script`]) measures visible elements and posts
//! plain metrics; [`classify`] turns them into warnings. Pure, so the rules
//! are unit-tested on synthetic metrics.
//!
//! These are heuristics: they miss real problems and flag fine layouts. The
//! agent is told to look at the screenshots regardless.
use mast_proto::ArtifactLayoutWarning;
use mast_proto::artifacts::artifact_limits;

pub(crate) const CLIPPED_TEXT: &str = "clippedText";
pub(crate) const OVERLAPPING_SIBLINGS: &str = "overlappingSiblings";
pub(crate) const NARROW_TEXT_COLUMN: &str = "narrowTextColumn";
pub(crate) const WIDER_THAN_PARENT: &str = "widerThanParent";
pub(crate) const FIXED_WIDTH: &str = "fixedWidth";

/// Findings of one kind per render (a stray rule can match hundreds of
/// elements; the total cap is [`artifact_limits::MAX_LAYOUT_WARNINGS`]).
const MAX_PER_KIND: usize = 8;

/// Overlap as a share of the smaller box that counts as overlapping.
const OVERLAP_SHARE: f64 = 0.2;

/// One visible element as the probe measured it. Integers (CSS px, rounded).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct ElementMetrics {
    /// `tag#id` or `tag.class:nth-child(n)`.
    pub(crate) sel: String,
    /// Whitespace-collapsed text of the element (the probe cuts it).
    pub(crate) text: String,
    /// Bounding box.
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: i32,
    pub(crate) h: i32,
    /// `scrollWidth` / `scrollHeight` / `clientWidth` / `clientHeight`.
    pub(crate) sw: i32,
    pub(crate) sh: i32,
    pub(crate) cw: i32,
    pub(crate) ch: i32,
    /// `overflow: hidden|clip` on either axis, or `text-overflow: ellipsis`.
    pub(crate) clips: bool,
    /// Computed `position`.
    pub(crate) pos: String,
    /// Index of the parent in the list; `-1` for a child of `<body>`, `-2`
    /// when the parent was not measured.
    pub(crate) parent: i32,
    /// The parent's `clientWidth`, and whether it scrolls horizontally.
    pub(crate) pcw: i32,
    pub(crate) pscroll: bool,
    /// A text block (own text, only inline children): its line count and
    /// word count.
    pub(crate) leaf: bool,
    pub(crate) lines: u32,
    pub(crate) words: u32,
    /// Authored fixed pixel `width`/`min-width` (inline or stylesheet); 0 when
    /// none.
    pub(crate) fixed: i32,
}

/// A finding before it is tagged with its viewport and theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawWarning {
    pub(crate) kind: &'static str,
    pub(crate) selector: String,
    pub(crate) text_sample: String,
}

fn sample(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .chars()
        .take(artifact_limits::MAX_WARNING_TEXT_CHARS)
        .collect()
}

fn overlay(m: &ElementMetrics) -> bool {
    m.pos == "absolute" || m.pos == "fixed"
}

/// Intersection area over the smaller box's area.
fn overlap_share(a: &ElementMetrics, b: &ElementMetrics) -> f64 {
    let width = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let height = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if width <= 0 || height <= 0 {
        return 0.0;
    }
    let smaller = (i64::from(a.w) * i64::from(a.h)).min(i64::from(b.w) * i64::from(b.h));
    if smaller <= 0 {
        return 0.0;
    }
    (i64::from(width) * i64::from(height)) as f64 / smaller as f64
}

/// Every heuristic over `metrics` for a render at `viewport_width`.
pub(crate) fn classify(metrics: &[ElementMetrics], viewport_width: u32) -> Vec<RawWarning> {
    let mut out: Vec<RawWarning> = Vec::new();
    let mut counts = std::collections::HashMap::<&'static str, usize>::new();
    let mut push = |kind: &'static str, selector: String, text: &str| {
        let count = counts.entry(kind).or_default();
        if *count < MAX_PER_KIND {
            *count += 1;
            out.push(RawWarning {
                kind,
                selector,
                text_sample: sample(text),
            });
        }
    };

    for m in metrics {
        // Clipped text: hidden overflow or an ellipsis that actually cut.
        if m.clips && !m.text.trim().is_empty() && (m.sw > m.cw + 1 || m.sh > m.ch + 1) {
            push(CLIPPED_TEXT, m.sel.clone(), &m.text);
        }
        // Narrow text column: a block of 3+ lines averaging under 2 words.
        if m.leaf && m.lines >= 3 && f64::from(m.words) / f64::from(m.lines) < 2.0 {
            push(NARROW_TEXT_COLUMN, m.sel.clone(), &m.text);
        }
        // Wider than a parent that does not scroll.
        if m.parent >= 0 && !m.pscroll && m.pcw > 0 && m.pos != "fixed" && m.sw > m.pcw + 1 {
            push(WIDER_THAN_PARENT, m.sel.clone(), &m.text);
        }
        // A fixed pixel width that cannot fit the viewport.
        if m.fixed > 0 && i64::from(m.fixed) > i64::from(viewport_width) {
            push(FIXED_WIDTH, m.sel.clone(), &m.text);
        }
    }

    // Overlapping siblings, per parent. Positioned overlays (badges, tooltips,
    // absolutely placed labels) overlap on purpose: only text over text counts.
    let mut groups = std::collections::BTreeMap::<i32, Vec<&ElementMetrics>>::new();
    for m in metrics
        .iter()
        .filter(|m| m.parent >= -1 && m.w > 0 && m.h > 0)
    {
        groups.entry(m.parent).or_default().push(m);
    }
    for siblings in groups.values() {
        for (i, a) in siblings.iter().enumerate() {
            for b in &siblings[i + 1..] {
                if (overlay(a) || overlay(b))
                    && (a.text.trim().is_empty() || b.text.trim().is_empty())
                {
                    continue;
                }
                if overlap_share(a, b) > OVERLAP_SHARE {
                    push(
                        OVERLAPPING_SIBLINGS,
                        format!("{} overlaps {}", a.sel, b.sel),
                        &a.text,
                    );
                }
            }
        }
    }
    out
}

/// Tag raw findings with where they were found, dropping repeats and
/// stopping at the total cap. `true` when the cap dropped findings.
pub(crate) fn tag(
    into: &mut Vec<ArtifactLayoutWarning>,
    raw: Vec<RawWarning>,
    viewport: &str,
    theme: &str,
) -> bool {
    let mut dropped = false;
    for warning in raw {
        if into.iter().any(|w| {
            w.kind == warning.kind
                && w.selector == warning.selector
                && w.viewport == viewport
                && w.theme == theme
        }) {
            continue;
        }
        if into.len() >= artifact_limits::MAX_LAYOUT_WARNINGS {
            dropped = true;
            continue;
        }
        into.push(ArtifactLayoutWarning {
            kind: warning.kind.to_owned(),
            selector: warning.selector,
            text_sample: warning.text_sample,
            viewport: viewport.to_owned(),
            theme: theme.to_owned(),
        });
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A visible box on its own row, so unrelated fixtures never overlap.
    fn el(sel: &str) -> ElementMetrics {
        static ROW: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
        let row = ROW.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ElementMetrics {
            y: row * 1000,
            sel: sel.into(),
            text: "text".into(),
            w: 100,
            h: 20,
            cw: 100,
            ch: 20,
            sw: 100,
            sh: 20,
            pos: "static".into(),
            parent: -1,
            ..Default::default()
        }
    }

    /// A box in the shared row at y = 0 (for overlap fixtures).
    fn at_origin(mut m: ElementMetrics) -> ElementMetrics {
        m.y = 0;
        m
    }

    fn kinds(found: &[RawWarning]) -> Vec<&'static str> {
        found.iter().map(|w| w.kind).collect()
    }

    #[test]
    fn clipped_text_needs_a_clip_and_real_overflow() {
        let mut cut = el("div.title");
        cut.clips = true;
        cut.sw = 240;
        cut.text = "A very long title that gets cut ".repeat(5);
        let mut fits = el("div.ok");
        fits.clips = true;
        let mut open = el("div.open");
        open.sw = 400; // overflows but is not clipped: other rules' business
        let found = classify(&[cut, fits, open], 1440);
        assert_eq!(kinds(&found), [CLIPPED_TEXT]);
        assert_eq!(found[0].selector, "div.title");
        assert!(found[0].text_sample.chars().count() <= 80);
        // Vertical clipping counts too.
        let mut tall = el("p.cut");
        tall.clips = true;
        tall.sh = 90;
        assert_eq!(kinds(&classify(&[tall], 1440)), [CLIPPED_TEXT]);
    }

    #[test]
    fn overlapping_siblings_need_more_than_a_fifth_of_the_smaller_box() {
        let mut a = at_origin(el("div.a"));
        a.parent = 0;
        let mut b = at_origin(el("div.b"));
        b.parent = 0;
        b.x = 50; // 50% of the 100x20 boxes overlap
        let mut c = at_origin(el("div.c"));
        c.parent = 0;
        c.x = 90; // 10%: under the threshold
        let found = classify(&[a.clone(), b.clone()], 1440);
        assert_eq!(kinds(&found), [OVERLAPPING_SIBLINGS]);
        assert_eq!(found[0].selector, "div.a overlaps div.b");
        assert!(classify(&[a.clone(), c], 1440).is_empty());
        // Different parents are not siblings.
        b.parent = 1;
        assert!(classify(&[a, b], 1440).is_empty());
    }

    #[test]
    fn positioned_overlays_overlap_on_purpose_unless_text_hits_text() {
        let mut card = at_origin(el("div.card"));
        card.parent = 0;
        let mut badge = at_origin(el("span.badge"));
        badge.parent = 0;
        badge.pos = "absolute".into();
        badge.x = 60;
        badge.text = String::new(); // an icon or dot: no text
        assert!(classify(&[card.clone(), badge.clone()], 1440).is_empty());
        // Overlay text on text is a real collision.
        badge.text = "NEW".into();
        assert_eq!(
            kinds(&classify(&[card.clone(), badge.clone()], 1440)),
            [OVERLAPPING_SIBLINGS]
        );
        // Text over an empty box is fine.
        card.text = String::new();
        assert!(classify(&[card, badge], 1440).is_empty());
    }

    #[test]
    fn narrow_text_columns_need_three_lines_under_two_words_each() {
        let mut column = el("p.narrow");
        column.leaf = true;
        column.lines = 6;
        column.words = 8; // 1.33 words per line
        let mut normal = el("p.normal");
        normal.leaf = true;
        normal.lines = 6;
        normal.words = 40;
        let mut short = el("p.short");
        short.leaf = true;
        short.lines = 2;
        short.words = 2;
        let found = classify(&[column, normal, short], 1440);
        assert_eq!(kinds(&found), [NARROW_TEXT_COLUMN]);
        assert_eq!(found[0].selector, "p.narrow");
    }

    #[test]
    fn children_wider_than_a_parent_that_does_not_scroll() {
        let mut wide = el("table.t");
        wide.parent = 0;
        wide.pcw = 300;
        wide.sw = 520;
        let mut in_scroller = wide.clone();
        in_scroller.sel = "table.scrolls".into();
        in_scroller.y += 100;
        in_scroller.pscroll = true;
        let mut fixed = wide.clone();
        fixed.sel = "div.fixed".into();
        fixed.y += 200;
        fixed.pos = "fixed".into();
        let mut body_child = wide.clone();
        body_child.sel = "main".into();
        body_child.y += 300;
        body_child.parent = -1;
        let found = classify(&[wide, in_scroller, fixed, body_child], 1440);
        assert_eq!(kinds(&found), [WIDER_THAN_PARENT]);
        assert_eq!(found[0].selector, "table.t");
    }

    #[test]
    fn fixed_pixel_widths_wider_than_the_viewport() {
        let mut banner = el("div.banner");
        banner.fixed = 960;
        let mut small = el("div.small");
        small.fixed = 600;
        assert_eq!(
            kinds(&classify(&[banner.clone(), small.clone()], 800)),
            [FIXED_WIDTH]
        );
        assert!(classify(&[banner, small], 1440).is_empty());
    }

    #[test]
    fn findings_are_capped_per_kind_and_in_total() {
        let many: Vec<_> = (0..30)
            .map(|i| {
                let mut e = el(&format!("div.n{i}"));
                e.fixed = 2000;
                e
            })
            .collect();
        assert_eq!(classify(&many, 800).len(), MAX_PER_KIND);

        let mut all = Vec::new();
        let mut dropped = false;
        for viewport in 0..10 {
            let raw: Vec<_> = (0..8)
                .map(|i| RawWarning {
                    kind: FIXED_WIDTH,
                    selector: format!("div.n{i}"),
                    text_sample: "x".into(),
                })
                .collect();
            dropped |= tag(&mut all, raw, &format!("v{viewport}"), "light");
        }
        assert_eq!(all.len(), artifact_limits::MAX_LAYOUT_WARNINGS);
        assert!(dropped);
        // The same finding in the same viewport and theme is reported once.
        let mut once = Vec::new();
        let raw = || {
            vec![RawWarning {
                kind: CLIPPED_TEXT,
                selector: "p".into(),
                text_sample: "x".into(),
            }]
        };
        tag(&mut once, raw(), "narrow", "dark");
        tag(&mut once, raw(), "narrow", "dark");
        tag(&mut once, raw(), "narrow", "light");
        assert_eq!(once.len(), 2);
    }

    #[test]
    fn text_samples_are_collapsed_and_cut_at_80_characters() {
        let text = "word   ".repeat(40);
        let cut = sample(&text);
        assert_eq!(cut.chars().count(), 80);
        assert!(!cut.contains("  "));
        assert_eq!(sample("é".repeat(100).as_str()).chars().count(), 80);
    }

    #[test]
    fn metrics_parse_from_the_probe_json_with_missing_fields() {
        let parsed: Vec<ElementMetrics> = serde_json::from_str(
            r#"[{"sel":"p.a","text":"hi","w":10,"h":5,"parent":-1,"pos":"static"},{"sel":"b"}]"#,
        )
        .unwrap();
        assert_eq!(parsed[0].w, 10);
        assert_eq!(parsed[1].parent, 0);
    }
}
