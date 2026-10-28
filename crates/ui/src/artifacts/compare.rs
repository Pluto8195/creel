//! The kind-aware revision compare view: pure model.
//!
//! - which comparison a kind gets ([`Flavor`]) and the modes it offers
//!   ([`CompareMode`]); the last mode chosen per kind is remembered
//!   ([`ComparePrefs`]);
//! - default revision pair (previous vs shown) and ordering;
//! - onion-skin opacity ([`OnionState`]);
//! - the single host document that shows both revisions side by side in two
//!   sandboxed frames with synchronized scrolling, plus the change-box
//!   overlay ([`compare_document`]).
//!
//! The view itself (loading, rendering, offscreen diffs) is
//! [`super::tab`]'s `compare_view`.
use gpui::App;
use zeron_proto::{ArtifactKind, ArtifactView};

use super::document::CompareFrame;
use super::pixel_diff::Region;
use crate::browser::fragment::{self, FragmentTheme, escape_attr, escape_text};

/// How a kind is compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    /// HTML fragment / page, Mermaid, image: both renders side by side.
    Visual,
    /// Markdown: both previews, changed blocks highlighted.
    Markdown,
    /// Options: pick an option, then as [`Flavor::Visual`].
    Options,
    /// Anything else: the text diff.
    Text,
}

pub(crate) fn flavor(kind: ArtifactKind) -> Flavor {
    match kind {
        ArtifactKind::HtmlFragment
        | ArtifactKind::HtmlPage
        | ArtifactKind::Mermaid
        | ArtifactKind::Image => Flavor::Visual,
        ArtifactKind::Markdown => Flavor::Markdown,
        ArtifactKind::Options => Flavor::Options,
        ArtifactKind::Unknown => Flavor::Text,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CompareMode {
    /// Rendered side by side (visual kinds, options) or rendered Markdown.
    SideBySide,
    /// The two renders stacked with an opacity slider (visual kinds).
    Onion,
    /// The text diff.
    Source,
}

impl CompareMode {
    pub(crate) fn label(self, flavor: Flavor) -> &'static str {
        match (self, flavor) {
            (Self::SideBySide, Flavor::Markdown) => "Rendered",
            (Self::SideBySide, _) => "Side by side",
            (Self::Onion, _) => "Onion skin",
            (Self::Source, _) => "Source",
        }
    }

    pub(crate) fn selector(self) -> &'static str {
        match self {
            Self::SideBySide => "artifact-compare-mode-side",
            Self::Onion => "artifact-compare-mode-onion",
            Self::Source => "artifact-compare-mode-source",
        }
    }
}

impl Flavor {
    /// The modes offered, in toolbar order.
    pub(crate) fn modes(self) -> &'static [CompareMode] {
        match self {
            Self::Visual | Self::Options => &[
                CompareMode::SideBySide,
                CompareMode::Onion,
                CompareMode::Source,
            ],
            Self::Markdown => &[CompareMode::SideBySide, CompareMode::Source],
            Self::Text => &[CompareMode::Source],
        }
    }

    /// Visual for visual kinds, rendered Markdown for Markdown, the text
    /// diff for kinds with nothing to render.
    pub(crate) fn default_mode(self) -> CompareMode {
        self.modes()[0]
    }

    /// Both revisions are rendered in the web view / offscreen.
    pub(crate) fn is_visual(self) -> bool {
        matches!(self, Self::Visual | Self::Options)
    }
}

impl CompareMode {
    /// Stable key written to the UI settings file.
    fn key(self) -> &'static str {
        match self {
            Self::SideBySide => "sideBySide",
            Self::Onion => "onion",
            Self::Source => "source",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        [Self::SideBySide, Self::Onion, Self::Source]
            .into_iter()
            .find(|mode| mode.key() == key)
    }
}

/// The last mode chosen per kind (compare views open in the mode the user
/// last used for that kind). Persisted in the UI settings file
/// (`artifactCompareModes`), so it survives restarts.
pub(crate) struct ComparePrefs;

impl ComparePrefs {
    /// The mode a new compare view of `kind` opens in.
    pub(crate) fn mode_for(kind: ArtifactKind, cx: &App) -> CompareMode {
        let flavor = flavor(kind);
        crate::settings::current(cx)
            .artifact_compare_modes
            .get(kind.as_str())
            .and_then(|key| CompareMode::from_key(key))
            .filter(|mode| flavor.modes().contains(mode))
            .unwrap_or_else(|| flavor.default_mode())
    }

    pub(crate) fn remember(kind: ArtifactKind, mode: CompareMode, cx: &mut App) {
        crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
            settings
                .artifact_compare_modes
                .insert(kind.as_str().to_owned(), mode.key().to_owned());
        });
    }
}

/// A list scroll position as a 0..=1 fraction of its `count` items: the
/// item at the top plus how far into it (`frac`). Item based, not pixel
/// based: the list only measures items near the viewport, so its pixel
/// range grows as it is scrolled.
pub(crate) fn scroll_ratio(item_ix: usize, frac: f32, count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    ((item_ix as f32 + frac.clamp(0.0, 1.0)) / count as f32).clamp(0.0, 1.0)
}

/// The inverse of [`scroll_ratio`] for a list of `count` items: the item at
/// the top and the fraction into it.
pub(crate) fn scroll_target(ratio: f32, count: usize) -> (usize, f32) {
    if count == 0 {
        return (0, 0.0);
    }
    let position = ratio.clamp(0.0, 1.0) * count as f32;
    let ix = (position.floor() as usize).min(count - 1);
    (ix, (position - ix as f32).clamp(0.0, 1.0))
}

/// Which side of the compare a picker or pane belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Left,
    Right,
}

/// The revision a side shows: `0` is the current file (changed since its
/// last recorded revision); the request omits `revision` for it.
pub(crate) fn revision_param(revision: u32) -> Option<u32> {
    (revision != 0).then_some(revision)
}

/// `(older, newer)` for `shown` against `other`: `0` (the current file) is
/// always newest.
pub(crate) fn ordered(shown: u32, other: u32) -> (u32, u32) {
    if shown == 0 || shown > other {
        (other, shown)
    } else {
        (shown, other)
    }
}

/// The default pair: the recorded revision before `shown`, against `shown`
/// (`0` = the current file). `None` when there is nothing earlier.
pub(crate) fn default_pair(artifact: &ArtifactView, shown: u32) -> Option<(u32, u32)> {
    let ceiling = if shown == 0 {
        artifact.current_revision.saturating_add(1)
    } else {
        shown
    };
    let previous = artifact
        .revisions
        .iter()
        .map(|revision| revision.id)
        .filter(|id| *id < ceiling && *id != 0)
        .max()?;
    Some((previous, shown))
}

/// Onion-skin state: how opaque the newer render is over the older.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct OnionState {
    opacity: f32,
}

impl Default for OnionState {
    fn default() -> Self {
        Self { opacity: 0.5 }
    }
}

impl OnionState {
    pub(crate) fn opacity(&self) -> f32 {
        self.opacity
    }

    pub(crate) fn set(&mut self, opacity: f32) {
        self.opacity = if opacity.is_nan() {
            0.5
        } else {
            opacity.clamp(0.0, 1.0)
        };
    }

    pub(crate) fn step(&mut self, delta: f32) {
        self.set(self.opacity + delta);
    }

    pub(crate) fn percent(&self) -> u32 {
        (self.opacity * 100.0).round() as u32
    }
}

/// In each frame: report the scroll position (as a fraction of the
/// scrollable height) to the host, and follow the host's. The scroll event
/// of a followed scroll is swallowed so the two never ping-pong.
pub(crate) const SYNC_FRAME_SCRIPT: &str = r##"(() => {
  let following = false;
  const root = () => document.scrollingElement || document.documentElement;
  const range = () => Math.max(0, root().scrollHeight - root().clientHeight);
  addEventListener("scroll", () => {
    if (following) return;
    const max = range();
    try { parent.postMessage({ zeronSync: { ratio: max > 0 ? root().scrollTop / max : 0 } }, "*"); } catch (_) {}
  }, { passive: true });
  // Zoom: CSS zoom on the page root, mirrored in the other pane by the host.
  // Keep the maths in step with `find::FRAME_SCRIPT` / `zoom::anchor_offset`.
  let zoom = 1, pinchFrom = 1;
  const setZoom = (to, ax, ay, relay) => {
    to = Math.round(Math.min(8, Math.max(0.25, to)) * 10000) / 10000;
    if (!(to > 0) || to === zoom) return;
    const se = root();
    const x = ax == null ? innerWidth / 2 : ax, y = ay == null ? innerHeight / 2 : ay;
    const k = to / zoom;
    const left = Math.max(0, (se.scrollLeft + x) * k - x), top = Math.max(0, (se.scrollTop + y) * k - y);
    zoom = to;
    document.documentElement.style.zoom = String(to);
    following = true;
    se.scrollLeft = left; se.scrollTop = top;
    requestAnimationFrame(() => { following = false; });
    if (relay) try { parent.postMessage({ zeronSyncZoom: { level: to, x, y } }, "*"); } catch (_) {}
  };
  addEventListener("wheel", (e) => {
    if (!(e.ctrlKey || e.metaKey)) return;
    e.preventDefault();
    setZoom(zoom * Math.exp(-e.deltaY * 0.01), e.clientX, e.clientY, true);
  }, { passive: false });
  addEventListener("gesturestart", (e) => { e.preventDefault(); pinchFrom = zoom; }, { passive: false });
  addEventListener("gesturechange", (e) => { e.preventDefault(); setZoom(pinchFrom * e.scale, e.clientX, e.clientY, true); }, { passive: false });
  addEventListener("gestureend", (e) => e.preventDefault(), { passive: false });
  addEventListener("message", (event) => {
    const z = event.data && event.data.zeronSyncZoomTo;
    if (z && typeof z.level === "number") return setZoom(z.level, z.x, z.y, false);
    const to = event.data && event.data.zeronSyncTo;
    if (!to || typeof to.ratio !== "number") return;
    following = true;
    root().scrollTop = to.ratio * range();
    requestAnimationFrame(() => { following = false; });
  });
})();
"##;

/// In the host: relay one frame's scroll and zoom to the other, take zoom
/// commands from the tab (`__zeronFrameCmd`, the same entry point as a
/// single page) and report the zoom where the tab polls for it.
pub(crate) const SYNC_HOST_SCRIPT: &str = r##"window.__zeronFrame = { zoom: 1000 };
const zeronFrames = () => Array.from(document.querySelectorAll("iframe"));
window.__zeronFrameCmd = (o) => {
  if (!o || typeof o.zeronZoom !== "number") return;
  window.__zeronFrame = Object.assign({}, window.__zeronFrame, { zoom: Math.round(o.zeronZoom * 1000) });
  zeronFrames().forEach((frame) => frame.contentWindow && frame.contentWindow.postMessage({ zeronSyncZoomTo: { level: o.zeronZoom } }, "*"));
};
addEventListener("message", (event) => {
  const frames = zeronFrames();
  const from = frames.findIndex((frame) => frame.contentWindow === event.source);
  if (from < 0) return;
  const other = frames[1 - from];
  const zoom = event.data && event.data.zeronSyncZoom;
  if (zoom && typeof zoom.level === "number") {
    window.__zeronFrame = Object.assign({}, window.__zeronFrame, { zoom: Math.round(zoom.level * 1000) });
    if (other && other.contentWindow) other.contentWindow.postMessage({ zeronSyncZoomTo: zoom }, "*");
    return;
  }
  const ratio = event.data && event.data.zeronSync && event.data.zeronSync.ratio;
  if (typeof ratio !== "number") return;
  if (other && other.contentWindow) other.contentWindow.postMessage({ zeronSyncTo: { ratio } }, "*");
});"##;

/// The CSS height of an onion-skin stack for renders `width` x `height`
/// pixels (the snapshot scale is `width` over the viewport width): the whole
/// render, however tall, not just the base viewport.
pub(crate) fn onion_height(width: u32, height: u32) -> f32 {
    let scale = width as f32 / super::document::COMPARE_VIEWPORT_WIDTH as f32;
    if scale.is_finite() && scale >= 1.0 {
        (height as f32 / scale).ceil().max(1.0)
    } else {
        (height as f32).max(1.0)
    }
}

/// Translucent boxes at `regions` (render pixels; `scale` is render pixels
/// per CSS pixel, 2 on a Retina snapshot) inside a frame whose top-left sits
/// at `origin` (CSS pixels) in the render. Boxes outside the frame are
/// dropped, the rest clipped to it. Positioned against the frame document,
/// so they scroll with the content.
pub(crate) fn overlay_html(
    regions: &[Region],
    scale: f32,
    origin: (u32, u32),
    frame_width: u32,
) -> String {
    let scale = if scale.is_finite() && scale >= 1.0 {
        scale
    } else {
        1.0
    };
    let down = |value: u32| (value as f32 / scale).floor() as u32;
    let up = |value: u32| (value as f32 / scale).ceil() as u32;
    let boxes: String = regions
        .iter()
        .filter_map(|region| {
            let left = down(region.x).max(origin.0);
            let right = up(region.x + region.width).min(origin.0 + frame_width);
            let top = down(region.y).max(origin.1);
            let bottom = up(region.y + region.height);
            (right > left && bottom > top).then(|| {
                format!(
                    "<div data-zeron-change style=\"position:absolute;left:{}px;top:{}px;width:{}px;height:{}px;\
background:rgba(255,64,64,.22);outline:2px solid rgba(255,64,64,.85);border-radius:2px\"></div>",
                    left - origin.0,
                    top - origin.1,
                    right - left,
                    bottom - top,
                )
            })
        })
        .collect();
    if boxes.is_empty() {
        String::new()
    } else {
        format!(
            "<div data-zeron-changes style=\"position:absolute;left:0;top:0;width:0;height:0;\
z-index:2147483647;pointer-events:none\">{boxes}</div>"
        )
    }
}

/// The one host document for both panes: two sandboxed frames at the same
/// width in a row, labelled, with synchronized scrolling.
pub(crate) fn compare_document(
    left: &CompareFrame,
    right: &CompareFrame,
    labels: (&str, &str),
    theme: &FragmentTheme,
) -> String {
    let background = theme
        .vars
        .iter()
        .find(|(name, _)| *name == "--background")
        .map_or("transparent", |(_, value)| value.as_str());
    let pane = |frame: &CompareFrame, label: &str| {
        format!(
            "<div class=\"pane\" style=\"width:{width}px\"><div class=\"label\">{label}</div>\
<iframe sandbox=\"allow-scripts\" referrerpolicy=\"no-referrer\" title=\"{title}\" srcdoc=\"{srcdoc}\"></iframe></div>",
            width = frame.width,
            label = escape_text(label),
            title = escape_attr(label),
            srcdoc = escape_attr(&frame.html),
        )
    };
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"referrer\" content=\"no-referrer\">\n\
<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\n\
<title>Compare revisions</title>\n<style>:root{{color-scheme:{scheme};background:{background}}}\
html,body{{margin:0;height:100%}}\
body{{box-sizing:border-box;padding:8px 12px 12px;display:flex;gap:12px;justify-content:center;overflow-x:auto;font:600 11px/20px system-ui,sans-serif}}\
.pane{{flex:none;display:flex;flex-direction:column;height:100%;min-width:0}}\
.label{{flex:none;height:20px;opacity:.7;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}\
iframe{{flex:1;min-height:0;width:100%;border:0;background:transparent;box-shadow:0 0 0 1px rgba(128,128,128,.35)}}\
</style>\n<script>{SYNC_HOST_SCRIPT}</script>\n</head>\n<body>\n{left_pane}\n{right_pane}\n</body>\n</html>\n",
        csp = escape_attr(&fragment::host_csp()),
        scheme = if theme.dark { "dark" } else { "light" },
        left_pane = pane(left, labels.0),
        right_pane = pane(right, labels.1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::tests::{artifact, id};

    #[test]
    fn each_kind_gets_its_flavor_default_and_modes() {
        use ArtifactKind::*;
        for kind in [HtmlFragment, HtmlPage, Mermaid, Image] {
            assert_eq!(flavor(kind), Flavor::Visual, "{kind:?}");
            assert_eq!(flavor(kind).default_mode(), CompareMode::SideBySide);
            assert!(flavor(kind).modes().contains(&CompareMode::Onion));
        }
        assert_eq!(flavor(Markdown), Flavor::Markdown);
        assert_eq!(
            flavor(Markdown).modes(),
            &[CompareMode::SideBySide, CompareMode::Source]
        );
        assert_eq!(flavor(Options), Flavor::Options);
        assert!(flavor(Options).is_visual());
        assert_eq!(flavor(Unknown).default_mode(), CompareMode::Source);
    }

    #[test]
    fn default_pair_is_previous_vs_shown() {
        let a = artifact(&id(1), "Plan", ArtifactKind::Markdown);
        // Revisions 1 and 2 recorded; current is 2.
        assert_eq!(default_pair(&a, 2), Some((1, 2)));
        assert_eq!(default_pair(&a, 1), None);
        // The current file past the last recorded revision.
        assert_eq!(default_pair(&a, 0), Some((2, 0)));
    }

    #[test]
    fn ordering_puts_the_current_file_last() {
        assert_eq!(ordered(2, 1), (1, 2));
        assert_eq!(ordered(1, 2), (1, 2));
        assert_eq!(ordered(0, 2), (2, 0));
        assert_eq!(revision_param(0), None);
        assert_eq!(revision_param(3), Some(3));
    }

    #[test]
    fn onion_opacity_clamps_and_steps() {
        let mut onion = OnionState::default();
        assert_eq!(onion.percent(), 50);
        onion.set(1.7);
        assert_eq!(onion.opacity(), 1.0);
        onion.set(-1.0);
        assert_eq!(onion.opacity(), 0.0);
        onion.set(f32::NAN);
        assert_eq!(onion.opacity(), 0.5);
        onion.step(0.25);
        assert_eq!(onion.percent(), 75);
        onion.step(0.5);
        assert_eq!(onion.percent(), 100);
    }

    #[test]
    fn overlay_translates_clips_and_drops_boxes() {
        let regions = [
            // Inside the frame (origin 16,16; width 488).
            Region {
                x: 36,
                y: 46,
                width: 40,
                height: 10,
            },
            // Hangs off the frame's left edge: clipped.
            Region {
                x: 0,
                y: 20,
                width: 30,
                height: 10,
            },
            // Entirely in the host padding: dropped.
            Region {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
        ];
        let html = overlay_html(&regions, 1.0, (16, 16), 488);
        assert!(
            html.contains("left:20px;top:30px;width:40px;height:10px"),
            "{html}"
        );
        assert!(
            html.contains("left:0px;top:4px;width:14px;height:10px"),
            "{html}"
        );
        assert_eq!(html.matches("data-zeron-change ").count(), 2);
        assert_eq!(overlay_html(&[], 1.0, (16, 16), 488), "");
        // A Retina snapshot (2 pixels per CSS pixel).
        let retina = [Region {
            x: 72,
            y: 92,
            width: 80,
            height: 20,
        }];
        let html = overlay_html(&retina, 2.0, (16, 16), 488);
        assert!(
            html.contains("left:20px;top:30px;width:40px;height:10px"),
            "{html}"
        );
    }

    #[test]
    fn compare_document_has_two_sandboxed_synced_frames() {
        let theme = FragmentTheme::from_theme(&crate::theme::Theme::default());
        let frame = |html: &str| CompareFrame {
            html: html.into(),
            width: 488,
            origin: (16, 16),
        };
        let doc = compare_document(
            &frame("<p>a</p>"),
            &frame("<p>b</p>"),
            ("r1", "r2 <now>"),
            &theme,
        );
        assert_eq!(doc.matches("sandbox=\"allow-scripts\"").count(), 2);
        assert_eq!(doc.matches("<iframe").count(), 2);
        assert!(doc.contains("zeronSyncTo"));
        assert!(doc.contains("r2 &lt;now&gt;"));
        assert!(doc.contains("&lt;p&gt;a&lt;/p&gt;"));
    }
}
