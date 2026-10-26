//! A range on one track: a handle at each end to move that end, the band between them to move the
//! range whole, the mouse wheel to zoom the track around the pointer (a double click shows the
//! whole span again), and the two ends as numbers to type. Whole-numbered values move by one.

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, vec2};

use crate::theme::Tokens;

/// What a drag moves.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    Low,
    High,
    Band,
}

/// What the track keeps between frames: the part of the span it shows, and what a drag moves.
#[derive(Clone, Copy, Debug, Default)]
struct State {
    view: Option<[f64; 2]>,
    grab: Option<Grab>,
}

/// The track's narrowest view, as a fraction of the span.
const MIN_VIEW: f64 = 1e-6;
/// Zoom per unit of wheel scroll.
const ZOOM: f64 = 0.004;
/// How close to a handle a press grabs it, in points.
const REACH: f32 = 8.0;

fn snap(v: f64, integer: bool) -> f64 {
    if integer { v.round() } else { v }
}

/// The range within `span` after this frame's input, when it changed.
pub(super) fn range_track(ui: &mut Ui, key: &str, [lo, hi]: [f64; 2], span: [f64; 2], integer: bool) -> Option<[f64; 2]> {
    let t = Tokens::get();
    let id = ui.id().with(("range_track", key));
    let mut state: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let full = [span[0], span[1].max(span[0] + 1e-9)];
    let [a, b] = state.view.unwrap_or(full);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), t.control_height), Sense::click_and_drag());
    let track = rect.shrink2(vec2(REACH, 0.0));
    let x_of = |v: f64| track.left() + ((v - a) / (b - a)).clamp(0.0, 1.0) as f32 * track.width();
    let v_of = |x: f32| a + f64::from((x - track.left()) / track.width().max(1.0)) * (b - a);
    let per_point = (b - a) / f64::from(track.width().max(1.0));
    let mut range = [lo, hi];

    if response.double_clicked() {
        state.view = None;
    }
    // The wheel zooms around the pointer, within the span.
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0
            && let Some(p) = response.hover_pos()
        {
            let at = v_of(p.x);
            let k = (-f64::from(scroll) * ZOOM).exp();
            let width = ((b - a) * k).clamp((full[1] - full[0]) * MIN_VIEW, full[1] - full[0]);
            let left = (at - (at - a) * width / (b - a)).clamp(full[0], full[1] - width);
            state.view = Some([left, left + width]);
        }
    }
    if response.drag_started()
        && let Some(p) = response.interact_pointer_pos()
    {
        let (xl, xh) = (x_of(lo), x_of(hi));
        state.grab = Some(if (p.x - xh).abs() <= REACH && (p.x - xh).abs() <= (p.x - xl).abs() {
            Grab::High
        } else if (p.x - xl).abs() <= REACH {
            Grab::Low
        } else if p.x > xl && p.x < xh {
            Grab::Band
        } else if p.x < xl {
            Grab::Low
        } else {
            Grab::High
        });
    }
    if response.dragged()
        && let (Some(grab), Some(p)) = (state.grab, response.interact_pointer_pos())
    {
        let at = v_of(p.x).clamp(full[0], full[1]);
        range = match grab {
            Grab::Low => [snap(at, integer).min(hi), hi],
            Grab::High => [lo, snap(at, integer).max(lo)],
            Grab::Band => {
                let d = f64::from(response.drag_delta().x) * per_point;
                let width = hi - lo;
                let start = snap((lo + d).clamp(full[0], full[1] - width), integer);
                [start, start + width]
            }
        };
    }
    if response.drag_stopped() {
        state.grab = None;
    }

    // The track, the shown span's part, the band and the handles.
    let painter = ui.painter_at(rect);
    let mid = rect.center().y;
    painter.line_segment([Pos2::new(track.left(), mid), Pos2::new(track.right(), mid)], Stroke::new(3.0, t.control_fill));
    let (xl, xh) = (x_of(range[0]), x_of(range[1]));
    painter.rect_filled(Rect::from_min_max(Pos2::new(xl, mid - 3.0), Pos2::new(xh, mid + 3.0)), 2.0, t.form_primary);
    for (x, v) in [(xl, range[0]), (xh, range[1])] {
        // A handle outside the zoomed view sits at the edge, hollow.
        let inside = v >= a && v <= b;
        let fill = if inside { t.form_primary } else { Color32::TRANSPARENT };
        painter.circle(Pos2::new(x, mid), 6.0, fill, Stroke::new(1.5, t.form_primary));
    }
    if state.view.is_some() {
        painter.text(rect.right_top(), egui::Align2::RIGHT_TOP, "zoom", egui::FontId::proportional(t.font_label * 0.75), t.control_border);
    }

    // The two ends as numbers.
    let step = if integer { 1.0 } else { per_point };
    ui.horizontal(|ui| {
        let decimals = if integer { 0 } else { (-(per_point.max(1e-12)).log10()).ceil().clamp(0.0, 6.0) as usize };
        let mut l = range[0];
        let mut h = range[1];
        let fl = ui.add(egui::DragValue::new(&mut l).speed(step).range(full[0]..=h).fixed_decimals(decimals));
        ui.label("…");
        let fh = ui.add(egui::DragValue::new(&mut h).speed(step).range(l..=full[1]).fixed_decimals(decimals));
        if fl.changed() || fh.changed() {
            range = [snap(l, integer), snap(h, integer)];
        }
    });

    ui.data_mut(|d| d.insert_temp(id, state));
    (range != [lo, hi]).then_some(range)
}
