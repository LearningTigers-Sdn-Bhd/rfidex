//! The badge layout and its planner: which text goes where, at what size.
//!
//! A port of event-printing's `pdf_generator.py`. Everything here is pure: the
//! only thing it needs from the outside is a way to measure text, so the whole
//! planner is tested with a fake ruler and no fonts. Units are points (1/72
//! inch) with the origin at the top left, y growing downward.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

pub const VALID_ELEMENTS: [&str; 8] = [
    "name",
    "role",
    "ticket_role",
    "company",
    "title",
    "country",
    "table_no",
    "qr",
];
pub const CUSTOM_PREFIX: &str = "custom_";
pub const MAX_CUSTOM_FIELDS: usize = 6;
const MIN_SCALE: f64 = 0.5;
const MAX_SCALE: f64 = 2.0;
const MAX_OFFSET_MM: f64 = 10.0;
const MAX_VERTICAL_OFFSET_MM: f64 = 80.0;

const MM_TO_PT: f64 = 72.0 / 25.4;
const SIDE_MARGIN: f64 = 0.2 * 72.0;
const BOTTOM_MARGIN: f64 = 0.06 * 72.0;
const FIT_SAFETY: f64 = 0.03 * 72.0;
const QR_NATURAL: f64 = 1.15 * 72.0;
const QR_MIN: f64 = 0.55 * 72.0;
const GAP_QR: f64 = 0.12 * 72.0;
const GAP_TEXT: f64 = 0.07 * 72.0;
const FIT_FILL_TARGET: f64 = 0.80;
const FIT_WIDTH_LIMIT: f64 = 0.92;
const SCALE_STEP: f64 = 0.05;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Paper {
    pub width_mm: f64,
    pub height_mm: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomField {
    pub label: String,
    #[serde(default)]
    pub backend_key: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Offset {
    #[serde(default)]
    pub dx_mm: f64,
    #[serde(default)]
    pub dy_mm: f64,
}

/// The same JSON shape event-printing saves, so its layouts import unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub paper: Paper,
    pub elements: Vec<String>,
    #[serde(default)]
    pub custom_fields: BTreeMap<String, CustomField>,
    #[serde(default)]
    pub element_scales: BTreeMap<String, f64>,
    #[serde(default)]
    pub element_bolds: BTreeMap<String, bool>,
    #[serde(default)]
    pub element_offsets: BTreeMap<String, Offset>,
    #[serde(default)]
    pub vertical_offset_mm: f64,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            paper: Paper {
                width_mm: 100.0,
                height_mm: 80.0,
            },
            elements: ["name", "role", "company", "qr"].map(String::from).to_vec(),
            custom_fields: BTreeMap::new(),
            element_scales: BTreeMap::new(),
            element_bolds: BTreeMap::new(),
            element_offsets: BTreeMap::new(),
            vertical_offset_mm: 0.0,
        }
    }
}

fn round_to(value: f64, places: i32) -> f64 {
    let factor = 10f64.powi(places);
    (value * factor).round() / factor
}

fn default_bold(el: &str) -> bool {
    matches!(el, "name" | "role" | "table_no")
}

impl Layout {
    /// The layout this one is allowed to be, or `None` when it cannot be used.
    /// Drops unknown elements and out-of-range values the way event-printing
    /// does, so a saved layout can never make a badge fail to render.
    pub fn sanitize(self) -> Option<Layout> {
        let (w, h) = (self.paper.width_mm, self.paper.height_mm);
        if !(20.0..=500.0).contains(&w) || !(20.0..=500.0).contains(&h) {
            return None;
        }
        let mut custom_fields = BTreeMap::new();
        for (id, field) in self.custom_fields {
            if !id.starts_with(CUSTOM_PREFIX) || custom_fields.len() >= MAX_CUSTOM_FIELDS {
                continue;
            }
            let id: String = id.chars().take(CUSTOM_PREFIX.len() + 12).collect();
            let label: String = field.label.trim().chars().take(60).collect();
            if label.is_empty() {
                continue;
            }
            let backend_key: String = field.backend_key.trim().chars().take(60).collect();
            custom_fields
                .entry(id)
                .or_insert(CustomField { label, backend_key });
        }
        let mut elements: Vec<String> = Vec::new();
        for el in self.elements {
            let known = VALID_ELEMENTS.contains(&el.as_str()) || custom_fields.contains_key(&el);
            if known && !elements.contains(&el) {
                elements.push(el);
            }
        }
        if elements.is_empty() {
            return None;
        }
        let known =
            |el: &String| VALID_ELEMENTS.contains(&el.as_str()) || custom_fields.contains_key(el);
        let element_scales = self
            .element_scales
            .into_iter()
            .filter(|(el, s)| known(el) && s.is_finite() && (s - 1.0).abs() >= 1e-9)
            .map(|(el, s)| (el, round_to(s.clamp(MIN_SCALE, MAX_SCALE), 2)))
            .collect();
        let element_bolds = self
            .element_bolds
            .into_iter()
            .filter(|(el, b)| known(el) && *b != default_bold(el))
            .collect();
        let element_offsets = self
            .element_offsets
            .into_iter()
            .filter(|(el, o)| known(el) && o.dx_mm.is_finite() && o.dy_mm.is_finite())
            .map(|(el, o)| {
                let clamp = |v: f64| round_to(v.clamp(-MAX_OFFSET_MM, MAX_OFFSET_MM), 1);
                (
                    el,
                    Offset {
                        dx_mm: clamp(o.dx_mm),
                        dy_mm: clamp(o.dy_mm),
                    },
                )
            })
            .filter(|(_, o)| o.dx_mm != 0.0 || o.dy_mm != 0.0)
            .collect();
        let vertical = if self.vertical_offset_mm.is_finite() {
            round_to(
                self.vertical_offset_mm
                    .clamp(-MAX_VERTICAL_OFFSET_MM, MAX_VERTICAL_OFFSET_MM),
                1,
            )
        } else {
            0.0
        };
        Some(Layout {
            paper: self.paper,
            elements,
            custom_fields,
            element_scales,
            element_bolds,
            element_offsets,
            vertical_offset_mm: vertical,
        })
    }
}

/// What a badge shows. `ticket_id` is what the QR code encodes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ticket {
    pub ticket_id: String,
    pub name: Option<String>,
    pub company: Option<String>,
    pub title: Option<String>,
    pub country: Option<String>,
    pub table_no: Option<String>,
    pub ticket_type: String,
    pub role: Option<String>,
    /// Keyed by custom element id (`custom_ab12cd`).
    pub custom: BTreeMap<String, String>,
}

/// Measures text. The real ruler shapes with fonts; tests use a fixed width
/// per character.
pub trait Measure {
    fn width(&self, text: &str, bold: bool, size_pt: f64) -> f64;
}

/// One thing to paint. `y` is from the top of the paper.
#[derive(Clone, Debug, PartialEq)]
pub enum Draw {
    Text {
        text: String,
        bold: bool,
        size_pt: f64,
        center_x: f64,
        baseline_y: f64,
    },
    Qr {
        x: f64,
        y: f64,
        size: f64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub width_pt: f64,
    pub height_pt: f64,
    pub draws: Vec<Draw>,
}

#[derive(Clone, Debug)]
enum Kind {
    Text {
        bold: bool,
        lines: Vec<String>,
        line_h: f64,
    },
    Qr,
}

#[derive(Clone, Debug)]
struct Item {
    el: String,
    kind: Kind,
    size: f64,
    scale: f64,
}

fn text_for_element(el: &str, t: &Ticket) -> Option<String> {
    let text = match el {
        "name" => t.name.clone(),
        "role" => Some(normalize_role(&t.ticket_type)),
        "ticket_role" => t.role.clone(),
        "company" => t.company.clone(),
        "title" => t.title.clone(),
        "country" => t.country.clone(),
        "table_no" => t.table_no.as_ref().map(|n| format!("TABLE {n}")),
        _ if el.starts_with(CUSTOM_PREFIX) => t.custom.get(el).cloned(),
        _ => None,
    };
    text.filter(|s| !s.trim().is_empty())
}

fn normalize_role(role: &str) -> String {
    let lower = role.to_lowercase();
    if lower.contains("conference") || lower.contains("delegate") {
        "DELEGATE".to_string()
    } else {
        role.to_string()
    }
}

/// Greedy word wrap. A word wider than the line on its own (a long name with
/// no spaces, as Chinese names are written) is broken between characters
/// rather than left to run off the badge.
fn wrap(m: &dyn Measure, text: &str, bold: bool, size: f64, max_w: f64) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let attempt = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if m.width(&attempt, bold, size) <= max_w {
            current = attempt;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if m.width(word, bold, size) <= max_w {
            current = word.to_string();
            continue;
        }
        for cluster in word.graphemes(true) {
            let attempt = format!("{current}{cluster}");
            if !current.is_empty() && m.width(&attempt, bold, size) > max_w {
                lines.push(std::mem::take(&mut current));
            }
            current.push_str(cluster);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// The most balanced two-line split of `words` that fits both lines.
fn balanced_split(
    m: &dyn Measure,
    words: &[&str],
    bold: bool,
    size: f64,
    max_w: f64,
) -> Option<Vec<String>> {
    let mut best: Option<(f64, Vec<String>)> = None;
    for i in 1..words.len() {
        let (a, b) = (words[..i].join(" "), words[i..].join(" "));
        let (wa, wb) = (m.width(&a, bold, size), m.width(&b, bold, size));
        if wa <= max_w && wb <= max_w {
            let score = (wa - wb).abs();
            if best.as_ref().is_none_or(|(s, _)| score < *s) {
                best = Some((score, vec![a, b]));
            }
        }
    }
    best.map(|(_, lines)| lines)
}

/// Largest size first, even when that means two lines: at each size try one
/// line, then a balanced split, before stepping down.
fn fit_lines(
    m: &dyn Measure,
    text: &str,
    sizes: &[f64],
    bold: bool,
    max_w: f64,
) -> (f64, Vec<String>) {
    let upper = text.to_uppercase();
    let upper = upper.trim();
    let words: Vec<&str> = upper.split_whitespace().collect();
    for &size in sizes {
        if m.width(upper, bold, size) <= max_w {
            return (size, vec![upper.to_string()]);
        }
        if words.len() >= 2 {
            if let Some(lines) = balanced_split(m, &words, bold, size, max_w) {
                return (size, lines);
            }
        }
    }
    (11.0, wrap(m, upper, bold, 11.0, max_w))
}

const NAME_SIZES: [f64; 8] = [26.0, 24.0, 22.0, 20.0, 18.0, 16.0, 14.0, 12.0];
const ROLE_SIZES: [f64; 6] = [22.0, 20.0, 18.0, 16.0, 14.0, 12.0];

/// Company-style shrink: 13pt, then 11, then 9 as the line count grows.
fn shrink_wrap(m: &dyn Measure, text: &str, bold: bool, max_w: f64) -> (f64, Vec<String>) {
    let upper = text.to_uppercase();
    let mut size = 13.0;
    let mut lines = wrap(m, &upper, bold, size, max_w);
    if lines.len() > 3 {
        size = 9.0;
        lines = wrap(m, &upper, bold, size, max_w);
    } else if lines.len() > 2 {
        size = 11.0;
        lines = wrap(m, &upper, bold, size, max_w);
    }
    (size, lines)
}

/// Re-wrap at another size without adding lines: growth must never add wrap
/// lines (re-wrapping reads worse than slightly smaller type).
fn lines_at_size(
    m: &dyn Measure,
    text: &str,
    bold: bool,
    size: f64,
    max_w: f64,
    budget: usize,
) -> Option<Vec<String>> {
    let upper = text.to_uppercase();
    let upper = upper.trim();
    if m.width(upper, bold, size) <= max_w {
        return Some(vec![upper.to_string()]);
    }
    let words: Vec<&str> = upper.split_whitespace().collect();
    if words.len() >= 2 && budget >= 2 {
        if let Some(lines) = balanced_split(m, &words, bold, size, max_w) {
            return Some(lines);
        }
    }
    if budget >= 1 && !words.is_empty() {
        let widest = words
            .iter()
            .map(|w| m.width(w, bold, size))
            .fold(0.0, f64::max);
        if widest <= max_w {
            return Some(wrap(m, upper, bold, size, max_w));
        }
    }
    None
}

fn measure_element(
    m: &dyn Measure,
    el: &str,
    t: &Ticket,
    max_w: f64,
    scale: f64,
    bold: Option<bool>,
) -> Option<Item> {
    if el == "qr" {
        return Some(Item {
            el: el.to_string(),
            kind: Kind::Qr,
            size: QR_NATURAL * scale,
            scale,
        });
    }
    let text = text_for_element(el, t)?;
    // Grow the box with the type: same wraps, bigger glyphs.
    let fit_width = max_w / scale;
    let bold = bold.unwrap_or_else(|| default_bold(el));
    let (size, lines) = match el {
        "name" => fit_lines(m, &text, &NAME_SIZES, bold, fit_width),
        "role" | "table_no" => fit_lines(m, &text, &ROLE_SIZES, bold, fit_width),
        _ => shrink_wrap(m, &text, bold, fit_width),
    };
    if lines.is_empty() {
        return None;
    }
    let size = size * scale;
    Some(Item {
        el: el.to_string(),
        kind: Kind::Text {
            bold,
            lines,
            line_h: size + 3.0,
        },
        size,
        scale,
    })
}

fn gap_between(prev: &Item, cur: &Item) -> f64 {
    if matches!(prev.kind, Kind::Qr) || matches!(cur.kind, Kind::Qr) {
        GAP_QR
    } else {
        GAP_TEXT
    }
}

fn block_height(items: &[Item]) -> f64 {
    let mut total = 0.0;
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            total += gap_between(&items[i - 1], it);
        }
        total += match &it.kind {
            Kind::Qr => it.size,
            Kind::Text { lines, line_h, .. } => lines.len() as f64 * line_h,
        };
    }
    total
}

/// Overflow: the QR code shrinks first (it stays scannable well below its
/// natural size), then the largest text steps down a point at a time.
fn shrink_to_fit(items: &mut [Item], usable_h: f64) {
    for _ in 0..400 {
        if block_height(items) <= usable_h {
            return;
        }
        if let Some(qr) = items
            .iter_mut()
            .find(|it| matches!(it.kind, Kind::Qr) && it.size > QR_MIN)
        {
            qr.size = (qr.size - 0.05 * 72.0).max(QR_MIN);
            continue;
        }
        let mut biggest: Option<usize> = None;
        for (i, it) in items.iter().enumerate() {
            if matches!(it.kind, Kind::Text { .. })
                && it.size > 8.0
                && biggest.is_none_or(|b| it.size > items[b].size)
            {
                biggest = Some(i);
            }
        }
        let Some(b) = biggest else { return };
        let size = (items[b].size - 1.0).max(8.0);
        items[b].size = size;
        if let Kind::Text { line_h, .. } = &mut items[b].kind {
            *line_h = size + 3.0;
        }
    }
}

fn growth_cap(el: &str, scale: f64) -> f64 {
    let base = match el {
        "name" => 44.0,
        "role" => 36.0,
        "table_no" => 30.0,
        _ => 20.0,
    };
    base * scale
}

/// Sparse badges: grow the type until the block fills a share of the height.
/// The block stops at `FIT_FILL_TARGET` of the height, lines keep
/// `FIT_WIDTH_LIMIT` of the width, each element stops at its cap, and a step
/// that would add lines or overflow is rolled back.
fn auto_fit(items: &mut Vec<Item>, m: &dyn Measure, t: &Ticket, h: f64, max_w: f64) {
    if !items.iter().any(|it| matches!(it.kind, Kind::Text { .. })) {
        return;
    }
    let budget = h * FIT_FILL_TARGET;
    while block_height(items) < budget {
        let mut grew = false;
        let mut trial: Vec<Item> = Vec::with_capacity(items.len());
        for it in items.iter() {
            let Kind::Text { bold, lines, .. } = &it.kind else {
                trial.push(it.clone());
                continue;
            };
            let cap = growth_cap(&it.el, it.scale);
            if it.size >= cap {
                trial.push(it.clone());
                continue;
            }
            let grown = (it.size * (1.0 + SCALE_STEP)).min(cap);
            let new_lines = text_for_element(&it.el, t)
                .and_then(|text| lines_at_size(m, &text, *bold, grown, max_w, lines.len()));
            let fits = new_lines.filter(|ls| {
                !ls.is_empty()
                    && ls
                        .iter()
                        .all(|l| m.width(l, *bold, grown) <= max_w * FIT_WIDTH_LIMIT)
            });
            let Some(new_lines) = fits else {
                trial.push(it.clone());
                continue;
            };
            grew = true;
            trial.push(Item {
                el: it.el.clone(),
                kind: Kind::Text {
                    bold: *bold,
                    lines: new_lines,
                    line_h: grown + 3.0,
                },
                size: grown,
                scale: it.scale,
            });
        }
        if !grew || block_height(&trial) > budget {
            break;
        }
        *items = trial;
    }
}

/// Where everything on the badge goes.
pub fn plan(layout: &Layout, ticket: &Ticket, m: &dyn Measure) -> Plan {
    let w = layout.paper.width_mm * MM_TO_PT;
    let h = layout.paper.height_mm * MM_TO_PT;
    let max_w = w - 2.0 * SIDE_MARGIN;

    let mut items: Vec<Item> = layout
        .elements
        .iter()
        .filter_map(|el| {
            let scale = layout.element_scales.get(el).copied().unwrap_or(1.0);
            measure_element(
                m,
                el,
                ticket,
                max_w,
                scale,
                layout.element_bolds.get(el).copied(),
            )
        })
        .collect();

    auto_fit(&mut items, m, ticket, h, max_w);
    shrink_to_fit(&mut items, h - BOTTOM_MARGIN - FIT_SAFETY);

    let block_h = block_height(&items);
    let usable_h = h - BOTTOM_MARGIN;
    // The same maths as the PDF: y measured up from the bottom edge. The
    // block is centred above the bottom margin, then shifted by the user's
    // vertical offset and kept on the paper.
    let mut start_y = if block_h >= usable_h {
        h
    } else {
        BOTTOM_MARGIN + (usable_h + block_h) / 2.0
    };
    if layout.vertical_offset_mm != 0.0 {
        start_y -= layout.vertical_offset_mm * MM_TO_PT;
        start_y = start_y.min(h).max(BOTTOM_MARGIN + block_h);
    }

    let mut draws = Vec::new();
    let mut current_y = start_y;
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            current_y -= gap_between(&items[i - 1], it);
        }
        // The nudge applies at the draw call only, never to `current_y`, so
        // moving one element never shoves the ones after it.
        let offset = layout
            .element_offsets
            .get(&it.el)
            .copied()
            .unwrap_or_default();
        let (dx, dy) = (offset.dx_mm * MM_TO_PT, offset.dy_mm * MM_TO_PT);
        match &it.kind {
            Kind::Qr => {
                draws.push(Draw::Qr {
                    x: (w - it.size) / 2.0 + dx,
                    y: h - (current_y + dy),
                    size: it.size,
                });
                current_y -= it.size;
            }
            Kind::Text {
                bold,
                lines,
                line_h,
            } => {
                for line in lines {
                    draws.push(Draw::Text {
                        text: line.clone(),
                        bold: *bold,
                        size_pt: it.size,
                        center_x: w / 2.0 + dx,
                        baseline_y: h - (current_y - line_h + dy),
                    });
                    current_y -= line_h;
                }
            }
        }
    }
    Plan {
        width_pt: w,
        height_pt: h,
        draws,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character is `0.55 em` wide: enough to test wrapping without fonts.
    struct Mono;
    impl Measure for Mono {
        fn width(&self, text: &str, _bold: bool, size_pt: f64) -> f64 {
            text.chars().count() as f64 * 0.55 * size_pt
        }
    }

    fn ticket() -> Ticket {
        Ticket {
            ticket_id: "9f1c2b3a-0000-4000-8000-000000000001".into(),
            name: Some("Aina Binti Ahmad".into()),
            company: Some("Borneo Expo".into()),
            ticket_type: "Visitor".into(),
            ..Ticket::default()
        }
    }

    fn texts(plan: &Plan) -> Vec<(&str, f64)> {
        plan.draws
            .iter()
            .filter_map(|d| match d {
                Draw::Text { text, size_pt, .. } => Some((text.as_str(), *size_pt)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_long_name_takes_the_biggest_size_even_on_two_lines() {
        let (size, lines) = fit_lines(
            &Mono,
            "Alexander Montgomery Smith",
            &NAME_SIZES,
            true,
            254.7,
        );
        assert_eq!(size, 26.0);
        assert_eq!(lines, vec!["ALEXANDER", "MONTGOMERY SMITH"]);
    }

    #[test]
    fn a_short_name_stays_on_one_line() {
        let (size, lines) = fit_lines(&Mono, "Ann Lee", &NAME_SIZES, true, 254.7);
        assert_eq!((size, lines), (26.0, vec!["ANN LEE".to_string()]));
    }

    #[test]
    fn a_name_written_without_spaces_is_broken_between_characters() {
        let name = "王".repeat(40);
        let lines = wrap(&Mono, &name, false, 11.0, 100.0);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| Mono.width(l, false, 11.0) <= 100.0));
        assert_eq!(lines.concat(), name);
    }

    #[test]
    fn the_badge_stays_on_the_paper_and_centred() {
        let layout = Layout::default();
        let planned = plan(&layout, &ticket(), &Mono);
        assert!((planned.width_pt - 283.46).abs() < 0.1);
        for draw in &planned.draws {
            match draw {
                Draw::Text {
                    center_x,
                    baseline_y,
                    ..
                } => {
                    assert!((center_x - planned.width_pt / 2.0).abs() < 1e-9);
                    assert!(*baseline_y > 0.0 && *baseline_y < planned.height_pt);
                }
                Draw::Qr { x, y, size } => {
                    assert!(*x >= 0.0 && x + size <= planned.width_pt);
                    assert!(*y >= 0.0 && y + size <= planned.height_pt);
                }
            }
        }
        assert!(planned.draws.iter().any(|d| matches!(d, Draw::Qr { .. })));
    }

    #[test]
    fn a_ticket_without_a_company_prints_no_company_line() {
        let mut t = ticket();
        t.company = None;
        let planned = plan(&Layout::default(), &t, &Mono);
        let lines = texts(&planned);
        assert!(lines.iter().all(|(text, _)| !text.contains("BORNEO")));
    }

    #[test]
    fn the_vertical_offset_moves_the_whole_block_down() {
        let base = plan(&Layout::default(), &ticket(), &Mono);
        let layout = Layout {
            vertical_offset_mm: 5.0,
            ..Layout::default()
        };
        let moved = plan(&layout, &ticket(), &Mono);
        for (a, b) in base.draws.iter().zip(&moved.draws) {
            let (ya, yb) = match (a, b) {
                (Draw::Text { baseline_y: a, .. }, Draw::Text { baseline_y: b, .. }) => (a, b),
                (Draw::Qr { y: a, .. }, Draw::Qr { y: b, .. }) => (a, b),
                _ => panic!("same shapes"),
            };
            assert!((yb - ya - 5.0 * MM_TO_PT).abs() < 1e-9);
        }
    }

    #[test]
    fn nudging_one_element_leaves_the_others_where_they_were() {
        let base = plan(&Layout::default(), &ticket(), &Mono);
        let mut layout = Layout::default();
        layout.element_offsets.insert(
            "name".into(),
            Offset {
                dx_mm: 3.0,
                dy_mm: 0.0,
            },
        );
        let nudged = plan(&layout, &ticket(), &Mono);
        let x = |d: &Draw| match d {
            Draw::Text { center_x, .. } => *center_x,
            Draw::Qr { x, .. } => *x,
        };
        let name_lines = base
            .draws
            .iter()
            .zip(&nudged.draws)
            .filter(|(a, b)| (x(a) - x(b)).abs() > 1e-9)
            .count();
        assert!(name_lines >= 1);
        let last = base.draws.len() - 1; // the QR
        assert_eq!(x(&base.draws[last]), x(&nudged.draws[last]));
    }

    #[test]
    fn a_sparse_badge_never_balloons_past_the_role_cap() {
        let layout = Layout {
            elements: vec!["role".into()],
            ..Layout::default()
        };
        let mut t = ticket();
        t.ticket_type = "VVIP".into();
        let sizes: Vec<f64> = texts(&plan(&layout, &t, &Mono))
            .iter()
            .map(|(_, s)| *s)
            .collect();
        assert!(
            !sizes.is_empty() && sizes.iter().all(|s| *s <= 36.0 + 1e-9),
            "{sizes:?}"
        );
    }

    #[test]
    fn overflow_shrinks_the_qr_before_the_text() {
        let mut layout = Layout::default();
        layout.paper.height_mm = 40.0;
        let planned = plan(&layout, &ticket(), &Mono);
        let qr = planned
            .draws
            .iter()
            .find_map(|d| match d {
                Draw::Qr { size, .. } => Some(*size),
                _ => None,
            })
            .unwrap();
        assert!((QR_MIN - 1e-9..QR_NATURAL).contains(&qr), "{qr}");
    }

    #[test]
    fn sanitize_drops_what_cannot_print_and_clamps_the_rest() {
        let mut layout = Layout {
            elements: vec!["name".into(), "bogus".into(), "name".into(), "qr".into()],
            vertical_offset_mm: 500.0,
            ..Layout::default()
        };
        layout.element_scales.insert("name".into(), 9.0);
        layout.element_scales.insert("qr".into(), 1.0);
        layout.element_bolds.insert("name".into(), true); // the default: dropped
        layout.element_bolds.insert("company".into(), true);
        let clean = layout.sanitize().unwrap();
        assert_eq!(clean.elements, vec!["name", "qr"]);
        assert_eq!(clean.element_scales.get("name"), Some(&2.0));
        assert!(!clean.element_scales.contains_key("qr"));
        assert!(!clean.element_bolds.contains_key("name"));
        assert_eq!(clean.element_bolds.get("company"), Some(&true));
        assert_eq!(clean.vertical_offset_mm, 80.0);

        let mut tiny = Layout::default();
        tiny.paper.width_mm = 5.0;
        assert!(tiny.sanitize().is_none());
        let empty = Layout {
            elements: vec!["bogus".into()],
            ..Layout::default()
        };
        assert!(empty.sanitize().is_none());
    }

    #[test]
    fn a_delegate_or_conference_ticket_prints_as_delegate() {
        assert_eq!(normalize_role("Conference Pass"), "DELEGATE");
        assert_eq!(normalize_role("Visitor"), "Visitor");
    }

    #[test]
    fn event_printing_layouts_load_unchanged() {
        let json = r#"{"paper":{"width_mm":104.0,"height_mm":155.0},
          "elements":["name","custom_ab12cd","qr"],
          "custom_fields":{"custom_ab12cd":{"label":"Sponsor","backend_key":"sponsor"}},
          "element_scales":{"name":1.2},"element_bolds":{"company":true},
          "element_offsets":{"qr":{"dx_mm":1.5,"dy_mm":-2.0}},
          "vertical_offset_mm":3.5,"future_field":1}"#;
        let layout: Layout = serde_json::from_str(json).unwrap();
        let clean = layout.clone().sanitize().unwrap();
        assert_eq!(clean, layout);
    }
}
