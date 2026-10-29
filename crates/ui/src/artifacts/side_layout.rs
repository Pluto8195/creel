//! Layout of the side-by-side options view: each option renders at its
//! design width and is scaled down to its column (or shown at 100% with a
//! horizontal scroll per column). The numbers live here so they are testable
//! without a web view; the host document's script applies them.
use gpui::App;

/// Used when an option declares no width and its content names none.
pub(crate) const DEFAULT_DESIGN_WIDTH: u32 = 1100;
pub(crate) const MIN_DESIGN_WIDTH: u32 = 320;
pub(crate) const MAX_DESIGN_WIDTH: u32 = 4000;
/// A focused column gets this many times the share of the others.
pub(crate) const FOCUS_WEIGHT: f32 = 2.2;
/// Never shrink an option below this fraction (text stays findable).
pub(crate) const MIN_SCALE: f32 = 0.1;

/// The width an option is laid out at: its manifest `designWidth`, else the
/// manifest-wide one, else a `data-mast-design-width` attribute in its
/// source, else [`DEFAULT_DESIGN_WIDTH`].
pub(crate) fn design_width(entry: Option<u32>, manifest: Option<u32>, source: &str) -> u32 {
    entry
        .filter(|w| *w > 0)
        .or_else(|| scan_attribute(source))
        .or(manifest.filter(|w| *w > 0))
        .unwrap_or(DEFAULT_DESIGN_WIDTH)
        .clamp(MIN_DESIGN_WIDTH, MAX_DESIGN_WIDTH)
}

/// The first `data-mast-design-width="1280"` (quotes optional, `px` allowed).
pub(crate) fn scan_attribute(source: &str) -> Option<u32> {
    const NAME: &str = "data-mast-design-width";
    let lower = source.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(NAME) {
        let rest = source[from + at + NAME.len()..].trim_start();
        from += at + NAME.len();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start().trim_start_matches(['"', '\'']);
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(width) = digits.parse::<u32>()
            && width > 0
        {
            return Some(width);
        }
    }
    None
}

/// Scale that fits `design` px into a `column` px wide column: never above
/// 1 (an option is not enlarged past its real size).
pub(crate) fn fit_scale(column: f32, design: u32) -> f32 {
    if !column.is_finite() || design == 0 {
        return 1.0;
    }
    (column / design as f32).clamp(MIN_SCALE, 1.0)
}

/// The flex share of each of `count` columns; `focus` is a column index.
pub(crate) fn weights(count: usize, focus: Option<usize>) -> Vec<f32> {
    (0..count)
        .map(|ix| if focus == Some(ix) { FOCUS_WEIGHT } else { 1.0 })
        .collect()
}

/// How the columns are laid out; remembered per artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SideLayout {
    /// Columns shown ("N up"), 2-4; `None` = as many as fit by default.
    pub(crate) columns: Option<usize>,
    /// Option id of the wider column.
    pub(crate) focus: Option<String>,
    /// Fit to the column (true) or real size with a horizontal scroll.
    pub(crate) fit: bool,
}

impl Default for SideLayout {
    fn default() -> Self {
        Self {
            columns: None,
            focus: None,
            fit: true,
        }
    }
}

impl SideLayout {
    pub(crate) fn encode(&self) -> String {
        format!(
            "cols={};focus={};fit={}",
            self.columns.map_or(String::new(), |c| c.to_string()),
            self.focus.as_deref().unwrap_or(""),
            u8::from(self.fit)
        )
    }

    pub(crate) fn decode(text: &str) -> Self {
        let mut layout = Self::default();
        for part in text.split(';') {
            match part.split_once('=') {
                Some(("cols", v)) => {
                    layout.columns = v.parse().ok().filter(|c| (2..=4).contains(c));
                }
                Some(("focus", v)) if !v.is_empty() => layout.focus = Some(v.to_owned()),
                Some(("fit", v)) => layout.fit = v != "0",
                _ => {}
            }
        }
        layout
    }

    pub(crate) fn recall(cx: &App, artifact_id: &str) -> Self {
        crate::settings::current(cx)
            .artifact_side_layouts
            .get(artifact_id)
            .map(|text| Self::decode(text))
            .unwrap_or_default()
    }

    pub(crate) fn remember(&self, cx: &mut App, artifact_id: &str) {
        let (id, value) = (artifact_id.to_owned(), self.encode());
        let default = *self == Self::default();
        crate::settings::update(
            crate::settings::SavePolicy::Immediate,
            cx,
            move |settings| {
                if default {
                    settings.artifact_side_layouts.remove(&id);
                } else {
                    settings.artifact_side_layouts.insert(id, value);
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_prefers_entry_then_attribute_then_manifest_then_default() {
        let html = r#"<div data-mast-design-width="1280">x</div>"#;
        assert_eq!(design_width(Some(900), Some(700), html), 900);
        assert_eq!(design_width(None, Some(700), html), 1280);
        assert_eq!(design_width(None, Some(700), "<p>"), 700);
        assert_eq!(design_width(None, None, "<p>"), DEFAULT_DESIGN_WIDTH);
        assert_eq!(design_width(Some(10), None, ""), MIN_DESIGN_WIDTH);
        assert_eq!(design_width(Some(99999), None, ""), MAX_DESIGN_WIDTH);
    }

    #[test]
    fn attribute_scan_is_forgiving() {
        assert_eq!(
            scan_attribute("<a DATA-MAST-DESIGN-WIDTH = '960px'>"),
            Some(960)
        );
        assert_eq!(
            scan_attribute("<a data-mast-design-width=1024>"),
            Some(1024)
        );
        assert_eq!(scan_attribute("<a data-mast-design-width>"), None);
        assert_eq!(
            scan_attribute(r#"<a data-mast-design-width="x" data-mast-design-width="800">"#),
            Some(800)
        );
    }

    #[test]
    fn scale_fits_but_never_enlarges() {
        assert!((fit_scale(550.0, 1100) - 0.5).abs() < 1e-6);
        assert_eq!(fit_scale(2000.0, 1100), 1.0);
        assert_eq!(fit_scale(10.0, 1100), MIN_SCALE);
        assert_eq!(fit_scale(f32::NAN, 1100), 1.0);
    }

    #[test]
    fn focus_widens_one_column() {
        assert_eq!(weights(3, None), vec![1.0, 1.0, 1.0]);
        assert_eq!(weights(3, Some(1)), vec![1.0, FOCUS_WEIGHT, 1.0]);
        // Fewer columns: a bigger share of the pane, so a bigger scale.
        let col = |n: usize| 1200.0 / n as f32;
        assert!(fit_scale(col(2), 1100) > fit_scale(col(4), 1100));
    }

    #[test]
    fn layout_round_trips() {
        let layout = SideLayout {
            columns: Some(3),
            focus: Some("B".into()),
            fit: false,
        };
        assert_eq!(SideLayout::decode(&layout.encode()), layout);
        assert_eq!(SideLayout::decode("garbage"), SideLayout::default());
        assert_eq!(SideLayout::decode("cols=9").columns, None);
    }
}
