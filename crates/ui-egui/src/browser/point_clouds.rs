//! The Point Clouds folder of the Browser (#5151): an eye per cloud and for the folder, a menu,
//! and the Edit Point Cloud card: the file's units, the size of the dots (when some points are
//! dots), how the cloud is coloured (its colours, one colour, a ramp over a property or the
//! palette of a label, with its legend), and its filters (a range on a track, played forward with
//! ▶, or the values of a label picked in its legend). The live edits of one opening are one
//! undo step.

use serde_json::{Value, json};
use solidcraft_engine::doc::point_cloud::{COORDINATES, ColorBy, Filter, Keep, MAX_FILTERS, Names, PLAIN, PointCloud, UNITS, byte, label_color};
use solidcraft_engine::view::POINT_CLOUDS_FOLDER;

use super::{Actions, Row, draw_row, is_open, panel_pos, toggle, toggle_folder};
use crate::SolveApp;
use crate::context_menu::{Item, Target};
use crate::ui_kit;

#[path = "range_track.rs"]
mod range_track;
use range_track::range_track;

/// Filters the GPU applies; the others are kept but not drawn.
pub const DRAWN_FILTERS: usize = solidcraft_gpu::view::FILTERS;

/// The Edit Point Cloud card: the cloud, the undo depth when it opened, what it worked out once
/// for the cloud's data (the automatic height range, the number of dots), and a filter playing.
#[derive(Clone, Debug)]
pub struct CloudPanel {
    pub cloud: u64,
    pub(crate) depth: usize,
    auto: Option<(usize, [u64; 16], Option<[f64; 2]>)>,
    dots: Option<([usize; 3], usize)>,
    /// Each filter's playing speed, in units per second.
    speeds: [Option<f64>; MAX_FILTERS],
    pub play: Option<Play>,
}

/// A range filter moving forward on its own: its end (when it starts at the beginning of the
/// span), or the whole range. It is written to the design once, when it stops.
#[derive(Clone, Debug)]
pub struct Play {
    pub filter: usize,
    started: f64,
    from: [f64; 2],
    speed: f64,
    unroll: bool,
    /// The range now.
    pub now: [f64; 2],
}

/// How long playing a filter's whole span takes by default, in seconds.
const PLAY_SECONDS: f64 = 45.0;

impl CloudPanel {
    pub fn new(app: &SolveApp, cloud: u64) -> Self {
        CloudPanel { cloud, depth: app.session.undo.len(), auto: None, dots: None, speeds: [None; MAX_FILTERS], play: None }
    }
    /// The cloud's automatic height range, worked out again only when its points or placement
    /// change.
    fn auto_range(&mut self, c: &PointCloud) -> Option<[f64; 2]> {
        let key = (std::sync::Arc::as_ptr(c.points.values()).cast::<f32>() as usize, std::array::from_fn(|i| c.transform[i / 4][i % 4].to_bits()));
        match &self.auto {
            Some((p, t, r)) if (*p, *t) == key => *r,
            _ => {
                let r = c.auto_height_range();
                self.auto = Some((key.0, key.1, r));
                r
            }
        }
    }
    /// How many points are drawn as dots, worked out again only when the cloud's data change.
    fn dots(&mut self, c: &PointCloud) -> usize {
        let key = [
            std::sync::Arc::as_ptr(c.points.values()).cast::<f32>() as usize,
            std::sync::Arc::as_ptr(&c.cells.segments.0).cast::<u32>() as usize,
            std::sync::Arc::as_ptr(&c.cells.triangles.0).cast::<u32>() as usize,
        ];
        match self.dots {
            Some((k, n)) if k == key => n,
            _ => {
                let n = c.isolated_points();
                self.dots = Some((key, n));
                n
            }
        }
    }
    /// The range filter `k` of this card's cloud shows now, while it plays.
    pub fn playing(&self, cloud: u64, k: usize) -> Option<[f64; 2]> {
        self.play.as_ref().filter(|p| self.cloud == cloud && p.filter == k).map(|p| p.now)
    }
}

/// The ways the card colours the points, as the dropdown lists them.
const COLOR_BY: [(ColorBy, &str); 3] = [(ColorBy::File, "File Colors"), (ColorBy::Single, "Single Color"), (ColorBy::Height, "Height")];

/// The names of `property` when it is filtered by a set of values: a label whose values span
/// fewer than the 32 the view's mask holds. Any other property is filtered by a range.
fn label<'a>(c: &'a PointCloud, property: &str) -> Option<&'a Names> {
    let names = c.cells.names(property)?;
    let (lo, hi) = names.values.iter().fold((i32::MAX, i32::MIN), |(lo, hi), (v, _)| (lo.min(*v), hi.max(*v)));
    (i64::from(hi) - i64::from(lo) < 32).then_some(names)
}

/// The filters as `pointcloud.edit` takes them.
fn filters_json(filters: &[Filter]) -> Vec<Value> {
    filters.iter().map(|f| serde_json::to_value(f).unwrap_or(Value::Null)).collect()
}

/// The filters with `value` of the label `property` shown or hidden: in its set filter, or in a
/// new one keeping every named value but it.
fn toggled(c: &PointCloud, names: &Names, value: i32) -> Vec<Value> {
    let mut filters = c.filters.clone();
    match filters.iter_mut().find(|f| f.property == names.property && matches!(f.keep, Keep::Values { .. })) {
        Some(Filter { keep: Keep::Values { values }, .. }) => {
            if let Some(k) = values.iter().position(|v| *v == value) {
                values.remove(k);
            } else {
                values.push(value);
            }
        }
        _ => filters.push(Filter {
            property: names.property.clone(),
            keep: Keep::Values { values: names.values.iter().map(|(v, _)| *v).filter(|v| *v != value).collect() },
        }),
    }
    filters_json(&filters)
}

/// A label's legend: a swatch and a name per value, dimmed when a filter hides it. The value
/// clicked, to show or hide.
fn legend(ui: &mut egui::Ui, c: &PointCloud, names: &Names) -> Option<i32> {
    let kept = c.filters.iter().find_map(|f| match &f.keep {
        Keep::Values { values } if f.property == names.property => Some(values.clone()),
        _ => None,
    });
    let mut clicked = None;
    for (v, name) in &names.values {
        let shown = kept.as_ref().is_none_or(|k| k.contains(v));
        let [r, g, b] = label_color(f64::from(*v));
        let swatch = if shown { egui::Color32::from_rgb(r, g, b) } else { egui::Color32::from_rgba_unmultiplied(r, g, b, 40) };
        let response = ui
            .horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 3.0, swatch);
                let text = egui::RichText::new(name.as_str());
                ui.add(egui::Label::new(if shown { text } else { text.weak().strikethrough() }).sense(egui::Sense::click()))
            })
            .inner;
        if response.clicked() {
            clicked = Some(*v);
        }
    }
    clicked
}

pub(super) fn rows(app: &mut SolveApp, ui: &mut egui::Ui, depth: usize, acts: &mut Actions) {
    let clouds: Vec<(u64, String, bool)> = app.session.doc.point_clouds.iter().map(|c| (c.id, c.name.clone(), c.visible)).collect();
    if clouds.is_empty() {
        return;
    }
    let key = "point_clouds";
    let open = is_open(app, key, true);
    // The folder's eye is its own: each cloud keeps its eye while the folder hides them all.
    let shown = !app.session.folder_hidden(POINT_CLOUDS_FOLDER);
    let r = draw_row(
        ui,
        ui.id().with(key),
        &Row { depth, fold: Some(open), eye: Some(shown), icon: "folder", label: "Point Clouds", dim: !shown, ..Default::default() },
    );
    if r.fold || r.clicked {
        toggle(app, key, true);
    }
    if r.eye {
        toggle_folder(app, POINT_CLOUDS_FOLDER);
    }
    if !open {
        return;
    }
    for (id, name, visible) in clouds {
        let r = draw_row(
            ui,
            ui.id().with(("point_cloud", id)),
            &Row { depth: depth + 1, eye: Some(visible), icon: "point", label: &name, name: true, dim: !visible || !shown, ..Default::default() },
        );
        if r.eye {
            let _ = app.run("pointcloud.edit", json!({ "cloud": id, "visible": !visible }));
        }
        if r.double {
            app.tree.cloud_panel = Some(CloudPanel::new(app, id));
        }
        if let Some(p) = r.secondary {
            acts.menu = Some((p, Target::PointCloud { id }));
        }
    }
}

/// The menu of a point cloud.
pub fn items(app: &SolveApp, id: u64) -> Vec<Item> {
    let visible = app.session.doc.point_clouds.iter().find(|c| c.id == id).is_some_and(|c| c.visible);
    vec![
        Item::action("ui.editPointCloud", "Edit Point Cloud", "edit").with(json!({ "cloud": id })),
        Item::sep(),
        Item::action("pointcloud.delete", "Delete", "delete").with(json!({ "cloud": id })),
        Item::sep(),
        Item::action("pointcloud.edit", "Show/Hide", "eye").key("V").with(json!({ "cloud": id, "visible": !visible })),
    ]
}

pub(crate) fn panel(app: &mut SolveApp, ctx: &egui::Context) {
    let Some(mut m) = app.tree.cloud_panel.clone() else { return };
    let Some(c) = app.session.doc.point_clouds.iter().find(|c| c.id == m.cloud).cloned() else {
        app.tree.cloud_panel = None;
        return;
    };
    let mut edit: Option<Value> = None;
    let mut reload = false;
    let title = crate::i18n::format("browser.edit_point_cloud_title", &[("name", &c.name)]);
    let auto = if c.color_by == ColorBy::Height { m.auto_range(&c) } else { None };
    let dots = m.dots(&c);
    let now = ctx.input(|i| i.time);
    // A playing filter moves on; at the end of its span it stops, and is written once.
    let mut stop_play = false;
    if let Some(p) = m.play.as_mut()
        && let Some(span) = c.filters.get(p.filter).and_then(|f| c.span(&f.property))
    {
        let integer = c.filters.get(p.filter).is_some_and(|f| c.cells.integer(&f.property));
        let d = p.speed * (now - p.started);
        let snap = |v: f64| if integer { v.floor() } else { v };
        p.now = if p.unroll {
            [p.from[0], snap((p.from[1] + d).min(span[1]))]
        } else {
            let width = p.from[1] - p.from[0];
            let start = snap((p.from[0] + d).min(span[1] - width));
            [start, start + width]
        };
        stop_play = p.now[1] >= span[1];
        ctx.request_repaint();
    }
    let at = panel_pos(app);
    let (closed, action) = ui_kit::Window::new(egui::Id::new("sc_point_cloud_panel"), "point", &title)
        .placed(egui::Align2::LEFT_TOP, at)
        .fields()
        .actions("", crate::i18n::tr("ui.close"), None)
        .show(ctx, true, |ui| {
            ui_kit::row(ui, crate::i18n::tr_label("Points"), |ui| ui.label(c.len().to_string()));
            // The dots' size, where some points are dots: those no segment or triangle ends on.
            if dots > 0 {
                if dots < c.len() {
                    ui_kit::row(ui, crate::i18n::tr_label("Dots"), |ui| ui.label(dots.to_string()));
                }
                let mut size = c.point_size;
                if ui_kit::row(ui, crate::i18n::tr("ui.point_cloud_size"), |ui| ui_kit::slider(ui, &mut size, 0.5..=16.0, Default::default()))
                    .changed()
                {
                    edit = Some(json!({ "cloud": c.id, "point_size": size }));
                }
            }
            let units: Vec<&str> = UNITS.iter().map(|(u, _)| *u).collect();
            let was = units.iter().position(|u| *u == c.units).unwrap_or(0);
            let mut unit = was;
            // A dropdown's response doesn't report the pick: compare the index.
            ui_kit::row(ui, crate::i18n::tr("ui.point_cloud_units"), |ui| ui_kit::dropdown(ui, "point_cloud_units", &mut unit, &units));
            if unit != was
                && let Some(u) = units.get(unit)
            {
                edit = Some(json!({ "cloud": c.id, "units": u }));
            }
            // The ways of colouring, then every property of the file.
            let properties: Vec<&str> = c.property_names().filter(|n| !COORDINATES.contains(n)).collect();
            let labels: Vec<&str> = COLOR_BY.iter().map(|(_, l)| *l).chain(properties.iter().copied()).collect();
            let was = match c.color_by {
                ColorBy::Property => {
                    c.color_property.as_deref().and_then(|n| properties.iter().position(|p| *p == n)).map_or(0, |k| COLOR_BY.len() + k)
                }
                by => COLOR_BY.iter().position(|(b, _)| *b == by).unwrap_or(0),
            };
            let mut by = was;
            ui_kit::row(ui, crate::i18n::tr("ui.point_cloud_color_by"), |ui| ui_kit::dropdown(ui, "point_cloud_color_by", &mut by, &labels));
            if by != was {
                let name = COLOR_BY.get(by).map(|(b, _)| b.name()).or_else(|| properties.get(by - COLOR_BY.len()).copied());
                if let Some(name) = name {
                    edit = Some(json!({ "cloud": c.id, "color_by": name }));
                }
            }
            let colored = c.colored_property();
            let legend_shown = colored.and_then(|n| label(&c, n));
            match c.color_by {
                ColorBy::File if c.colors.is_empty() => {
                    ui_kit::row(ui, "", |ui| ui.weak(crate::i18n::tr("ui.point_cloud_no_file_colors")));
                }
                ColorBy::File => {}
                ColorBy::Single => {
                    let mut rgb = c.color.unwrap_or(PLAIN).map(byte);
                    if ui_kit::row(ui, crate::i18n::tr_label("Colour"), |ui| ui_kit::color_field(ui, &mut rgb)).changed() {
                        edit = Some(json!({ "cloud": c.id, "color": rgb.map(|v| f64::from(v) / 255.0) }));
                    }
                }
                ColorBy::Height | ColorBy::Property => {
                    let name = colored.unwrap_or_default();
                    if let Some(names) = legend_shown {
                        // A label: its legend, which also shows and hides its values.
                        if let Some(v) = legend(ui, &c, names) {
                            edit = Some(json!({ "cloud": c.id, "filters": toggled(&c, names, v) }));
                        }
                    } else if !c.cells.integer(name)
                        && let Some(span) = if c.color_by == ColorBy::Height { c.span("z").or(auto) } else { c.span(name) }
                    {
                        // A ramp over what is shown, unless its range is set.
                        let automatic = c.height_range.is_none();
                        if ui_kit::checkbox_row(ui, "point_cloud_auto_range", crate::i18n::tr("ui.point_cloud_auto_range"), automatic) {
                            edit = Some(json!({ "cloud": c.id, "color_range": if automatic { json!(c.color_range()) } else { Value::Null } }));
                        }
                        let now = c.color_range().unwrap_or(span);
                        if let Some(range) = range_track(ui, "point_cloud_color_range", now, span, false) {
                            edit = Some(json!({ "cloud": c.id, "color_range": range }));
                        }
                    }
                }
            }
            // Filters: only what every one of them keeps is shown.
            let mut filters = c.filters.clone();
            let mut changed = false;
            let mut removed = None;
            for (k, f) in c.filters.iter().enumerate() {
                let span = c.span(&f.property);
                let integer = c.cells.integer(&f.property);
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr_dyn(&format!("Filter: {}", f.property)));
                    if let (Some(range), Some(span)) = (f.range(), span) {
                        let playing = m.play.as_ref().is_some_and(|p| p.filter == k);
                        if ui_kit::button(ui, if playing { "⏸" } else { "▶" }, ui_kit::Button::Small).clicked() {
                            if playing {
                                stop_play = true;
                            } else {
                                let speed = m.speeds.get(k).copied().flatten().unwrap_or((span[1] - span[0]) / PLAY_SECONDS);
                                m.play = Some(Play { filter: k, started: now, from: range, speed, unroll: range[0] <= span[0], now: range });
                            }
                        }
                        let mut speed = m.speeds.get(k).copied().flatten().unwrap_or((span[1] - span[0]) / PLAY_SECONDS);
                        let step = speed * 0.01;
                        if ui.add(egui::DragValue::new(&mut speed).speed(step).range(1e-9..=f64::MAX).suffix("/s").max_decimals(2)).changed()
                            && let Some(slot) = m.speeds.get_mut(k)
                        {
                            *slot = Some(speed);
                        }
                    }
                    if ui_kit::button(ui, "×", ui_kit::Button::Small).clicked() {
                        removed = Some(k);
                    }
                });
                if k >= DRAWN_FILTERS {
                    ui.weak(crate::i18n::tr_dyn(&format!("Not drawn: the view applies {DRAWN_FILTERS} filters at most")));
                }
                match (&f.keep, span) {
                    (Keep::Values { .. }, _) => match label(&c, &f.property) {
                        Some(names) if legend_shown.is_some_and(|l| l.property == names.property) => {
                            ui.weak(crate::i18n::tr_label("Shown and hidden in the legend above"));
                        }
                        Some(names) => {
                            if let Some(v) = legend(ui, &c, names) {
                                edit = Some(json!({ "cloud": c.id, "filters": toggled(&c, names, v) }));
                            }
                        }
                        None => {}
                    },
                    (Keep::Range { range }, Some(span)) => {
                        let shown = m.playing(c.id, k).unwrap_or(*range);
                        if let Some(range) = range_track(ui, &format!("point_cloud_filter_{k}"), shown, span, integer)
                            && let Some(slot) = filters.get_mut(k)
                        {
                            slot.keep = Keep::Range { range };
                            changed = true;
                            // Moving it by hand stops it playing.
                            if m.play.as_ref().is_some_and(|p| p.filter == k) {
                                m.play = None;
                            }
                        }
                    }
                    (Keep::Range { .. }, None) => {}
                }
            }
            if let Some(k) = removed {
                filters.remove(k);
                changed = true;
                m.play = None;
            }
            let all: Vec<&str> = c.property_names().collect();
            if c.filters.len() < MAX_FILTERS {
                let mut add_labels = vec![crate::i18n::tr_label("Add filter…")];
                add_labels.extend(all.iter().copied());
                let mut pick = 0;
                ui_kit::row(ui, crate::i18n::tr_label("Filter by"), |ui| ui_kit::dropdown(ui, "point_cloud_add_filter", &mut pick, &add_labels));
                if let Some(name) = all.get(pick.wrapping_sub(1)) {
                    // A label keeps all its values at first, anything else its whole span.
                    let keep = match (label(&c, name), c.span(name)) {
                        (Some(names), _) => Some(Keep::Values { values: names.values.iter().map(|(v, _)| *v).collect() }),
                        (None, Some(span)) => Some(Keep::Range { range: span }),
                        (None, None) => None,
                    };
                    if let Some(keep) = keep {
                        filters.push(Filter { property: name.to_string(), keep });
                        changed = true;
                    }
                }
            }
            if changed {
                edit = Some(json!({ "cloud": c.id, "filters": filters_json(&filters) }));
            }
            if !c.source.is_empty() && ui_kit::button(ui, crate::i18n::tr_label("Reload from file"), ui_kit::Button::Normal).clicked() {
                reload = true;
            }
        });
    // A filter that stops playing is written where it stopped.
    if stop_play && let Some(p) = m.play.take() {
        let mut filters = c.filters.clone();
        if let Some(f) = filters.get_mut(p.filter) {
            f.keep = Keep::Range { range: p.now };
            edit = Some(json!({ "cloud": c.id, "filters": filters_json(&filters) }));
        }
    }
    if reload {
        let _ = app.run("pointcloud.reload", json!({ "cloud": c.id }));
    }
    if let Some(p) = edit
        && app.run("pointcloud.edit", p).is_ok()
    {
        // One undo step for the whole edit.
        if app.session.undo.len() > m.depth + 1 {
            let keep = app.session.undo.get(m.depth).cloned();
            app.session.undo.truncate(m.depth);
            app.session.undo.extend(keep);
        }
    }
    app.tree.cloud_panel = if closed || action == ui_kit::Action::Cancel { None } else { Some(m) };
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn a_cloud_has_a_menu_and_a_card_that_closes() {
        let mut h = crate::scenario::Harness::new();
        h.app.home.open = false;
        let dir = std::env::temp_dir().join(format!("solidcraft-browser-cloud-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scan.xyz");
        std::fs::write(
            &path,
            "0 0 0 255 0 0
1 0 0 0 255 0
",
        )
        .unwrap();
        let id = h.app.run("pointcloud.insert", json!({"path": path.to_string_lossy()})).unwrap()["cloud"].as_u64().unwrap();
        let items = super::items(&h.app, id);
        assert!(items.iter().any(|i| i.id == "pointcloud.delete"));
        h.app.tree.cloud_panel = Some(super::CloudPanel::new(&h.app, id));
        h.frames(4);
        assert!(h.widget("Close").is_some());
        // Each way of colouring shows its own rows.
        for by in ["single", "height", "file"] {
            h.app.run("pointcloud.edit", json!({"cloud": id, "color_by": by})).unwrap();
            h.frames(2);
        }
        h.app.run("pointcloud.edit", json!({"cloud": id, "color_by": "height"})).unwrap();
        h.frames(2);
        assert!(h.widget(crate::i18n::tr("ui.point_cloud_auto_range")).is_some());
        h.step(&json!({"widget": "Close"})).unwrap();
        h.frames(2);
        assert!(h.app.tree.cloud_panel.is_none());
        assert_eq!(h.app.session.doc.point_clouds.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
