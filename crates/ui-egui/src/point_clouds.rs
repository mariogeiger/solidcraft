//! Point clouds (#5151): with the GPU viewport their points, segments and triangles are drawn in
//! the 3D pass (depth-tested with the model), coloured and filtered by the shaders; the CPU
//! viewport draws some of their points over its image.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use egui::{Painter, Shape};
use solidcraft_engine::doc::point_cloud::{COORDINATES, ColorBy, On, PLAIN, PointCloud, byte};
use solidcraft_gpu::view::{Cell, CloudStyle, ColorMode, GpuCloud, NO_FILTER, cloud_cell};

use crate::SolveApp;
use crate::viewport::Proj;

/// Most points the CPU viewport draws of all clouds together.
const MAX_CPU_POINTS: usize = 100_000;

/// What a cloud's records are made from: they are made again only when one of these changes.
/// Its colour range, filter ranges and dot size are its style, read by the shaders.
#[derive(Clone, PartialEq)]
struct Key {
    data: Vec<usize>,
    len: usize,
    origin: [u64; 3],
    transform: [u64; 16],
    color_by: ColorBy,
    color: Option<[u32; 3]>,
    value: Option<String>,
    filtered: [Option<String>; 2],
}

impl Key {
    fn of(c: &PointCloud) -> Key {
        let mut data = vec![
            Arc::as_ptr(c.points.values()).cast::<f32>() as usize,
            Arc::as_ptr(&c.colors.0).cast::<u8>() as usize,
            Arc::as_ptr(&c.cells.segments.0).cast::<u32>() as usize,
            Arc::as_ptr(&c.cells.triangles.0).cast::<u32>() as usize,
        ];
        data.extend(c.cells.columns.iter().map(|k| Arc::as_ptr(&k.values.0).cast::<f32>() as usize));
        Key {
            data,
            len: c.len(),
            origin: [c.origin.x, c.origin.y, c.origin.z].map(f64::to_bits),
            transform: std::array::from_fn(|i| c.transform[i / 4][i % 4].to_bits()),
            color_by: c.color_by,
            color: c.color.map(|k| k.map(f32::to_bits)),
            value: c.colored_property().map(str::to_string),
            filtered: std::array::from_fn(|k| c.filters.get(k).map(|f| f.property.clone())),
        }
    }
}

/// Records of a cloud's dots, segments and triangles.
type Records = [Arc<Vec<u8>>; 3];

thread_local! {
    /// Records by cloud id: what they were made from, their version and bytes.
    static RECORDS: RefCell<HashMap<u64, (Key, u64, Records)>> = RefCell::new(HashMap::new());
    static VERSION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Where a property's values come from on one kind of element.
#[derive(Clone, Copy)]
enum Source<'a> {
    /// A coordinate of each corner in the design.
    Coordinate(usize),
    /// A column on the points: each corner's.
    Corners(&'a [f32]),
    /// A column on the element itself.
    Element(&'a [f32]),
}

fn source<'a>(c: &'a PointCloud, name: Option<&str>, on: On) -> Option<Source<'a>> {
    let name = name?;
    if let Some(k) = COORDINATES.iter().position(|n| *n == name) {
        return Some(Source::Coordinate(k));
    }
    if let Some(col) = c.cells.column(name, on) {
        return Some(Source::Element(&col.values.0));
    }
    c.cells.column(name, On::Point).map(|col| Source::Corners(&col.values.0))
}

fn records(c: &PointCloud) -> Records {
    let world: Vec<[f32; 3]> = (0..c.len())
        .map(|i| {
            // Placed in double precision: the cloud's origin may be far from the design's.
            let w = c.world_point(i).unwrap_or_default();
            [w.x as f32, w.y as f32, w.z as f32]
        })
        .collect();
    let plain = PLAIN.map(byte);
    let single = c.color.unwrap_or(PLAIN).map(byte);
    let own = |i: u32| match c.color_by {
        ColorBy::Single => single,
        ColorBy::File => c.colors.get(i as usize).unwrap_or(plain),
        ColorBy::Height | ColorBy::Property => plain,
    };
    let names = [c.colored_property(), c.filters.first().map(|f| f.property.as_str()), c.filters.get(1).map(|f| f.property.as_str())];
    let cells = &c.cells;
    let width = cells.column("width", On::Segment).map(|k| &k.values.0[..]);
    let height = cells.column("height", On::Segment).map(|k| &k.values.0[..]);
    let mut out: [Vec<u8>; 3] = Default::default();
    let mut used = vec![false; c.len()];
    // Each point's one segment in and one segment out, when it has exactly one of each: a bead
    // runs through it, and its two segments meet there mitred.
    let (mut into, mut out_of) = (vec![None; c.len()], vec![None; c.len()]);
    for ends in cells.segments.0.as_chunks::<2>().0 {
        let [a, b] = ends.map(|i| i as usize);
        if let Some(slot) = out_of.get_mut(a) {
            *slot = if slot.is_none() { Some(Some(b)) } else { Some(None) };
        }
        if let Some(slot) = into.get_mut(b) {
            *slot = if slot.is_none() { Some(Some(a)) } else { Some(None) };
        }
    }
    let through = |links: &[Option<Option<usize>>], at: u32| links.get(at as usize).copied().flatten().flatten().and_then(|k| world.get(k).copied());
    for (kind, on, n) in [(1usize, On::Segment, 2usize), (2, On::Triangle, 3)] {
        let indices = if n == 2 { &cells.segments.0 } else { &cells.triangles.0 };
        let sources = names.map(|name| source(c, name, on));
        for (e, ids) in indices.chunks_exact(n).enumerate() {
            let at = |k: usize| ids.get(k.min(n - 1)).copied().unwrap_or(0);
            let corners = [at(0), at(1), at(2)];
            for i in corners {
                if let Some(u) = used.get_mut(i as usize) {
                    *u = true;
                }
            }
            let mut cell =
                Cell { corners: corners.map(|i| world.get(i as usize).copied().unwrap_or_default()), rgb: own(corners[0]), ..Default::default() };
            let values = sources.map(|s| {
                s.map(|s| match s {
                    Source::Coordinate(k) => cell.corners.map(|p| p[k]),
                    Source::Corners(v) => corners.map(|i| v.get(i as usize).copied().unwrap_or(0.0)),
                    Source::Element(v) => [v.get(e).copied().unwrap_or(0.0); 3],
                })
            });
            if on == On::Segment {
                let [a, b] = [corners[0], corners[1]];
                // The point before `a` only when `a` is passed through, not where beads branch.
                let joined = |at: u32| matches!((into.get(at as usize), out_of.get(at as usize)), (Some(Some(Some(_))), Some(Some(Some(_)))));
                let before = through(&into, a).filter(|_| joined(a));
                let after = through(&out_of, b).filter(|_| joined(b));
                cell.neighbours = [before.unwrap_or(cell.corners[0]), after.unwrap_or(cell.corners[1])];
                cell.size = [width.and_then(|w| w.get(e)).copied().unwrap_or(0.0), height.and_then(|h| h.get(e)).copied().unwrap_or(0.0)];
            }
            fill(&mut cell, values);
            cloud_cell(&mut out[kind], &cell);
        }
    }
    // The points drawn as dots: those no segment or triangle ends on.
    let sources = names.map(|name| source(c, name, On::Point));
    for (i, p) in world.iter().enumerate() {
        if used.get(i).copied().unwrap_or(false) {
            continue;
        }
        let mut cell = Cell { corners: [*p; 3], rgb: own(i as u32), ..Default::default() };
        let values = sources.map(|s| {
            s.map(|s| match s {
                Source::Coordinate(k) => [p[k]; 3],
                Source::Corners(v) | Source::Element(v) => [v.get(i).copied().unwrap_or(0.0); 3],
            })
        });
        fill(&mut cell, values);
        cloud_cell(&mut out[0], &cell);
    }
    out.map(Arc::new)
}

/// The coloured value and the two filtered values of a cell, where it has them.
fn fill(cell: &mut Cell, [value, f0, f1]: [Option<[f32; 3]>; 3]) {
    if let Some(v) = value {
        cell.value = v;
        cell.has |= 4;
    }
    for (k, f) in [f0, f1].into_iter().enumerate() {
        if let Some(f) = f {
            cell.filtered[k] = f;
            cell.has |= 1 << k;
        }
    }
}

/// How a cloud is coloured and which of its elements are shown, as the shaders read it.
fn style(c: &PointCloud) -> CloudStyle {
    let property = c.colored_property();
    let mode = match (c.color_by, property) {
        (ColorBy::File | ColorBy::Single, _) | (_, None) => ColorMode::Own,
        (_, Some(name)) if c.cells.integer(name) => ColorMode::Palette,
        _ => ColorMode::Ramp,
    };
    let range = match c.color_by {
        ColorBy::Height => c.height_range_now(),
        _ => c.height_range.or_else(|| property.and_then(|p| c.span(p))),
    };
    let mut filters = [NO_FILTER; 2];
    for (f, r) in c.filters.iter().zip(filters.iter_mut()) {
        *r = f.range.map(|v| v as f32);
    }
    CloudStyle { mode, range: range.unwrap_or([0.0, 1.0]).map(|v| v as f32), filters, point_size: c.point_size as f32 }
}

/// The shown point clouds as the GPU draws them.
pub fn gpu_clouds(app: &SolveApp) -> Vec<GpuCloud> {
    let doc = &app.session.doc;
    RECORDS.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|id, _| doc.point_clouds.iter().any(|c| c.id == *id));
        solidcraft_engine::view::shown_point_clouds(&app.session)
            .map(|c| {
                let key = Key::of(c);
                let (version, [dots, segments, triangles]) = match m.get(&c.id) {
                    Some((k, version, records)) if *k == key => (*version, records.clone()),
                    _ => {
                        let version = VERSION.with(|v| {
                            v.set(v.get() + 1);
                            v.get()
                        });
                        let records = records(c);
                        m.insert(c.id, (key, version, records.clone()));
                        (version, records)
                    }
                };
                GpuCloud { id: c.id, version, dots, segments, triangles, style: style(c) }
            })
            .collect()
    })
}

/// CPU viewport only: some of each shown cloud's points over the rendered model.
pub fn show(app: &SolveApp, painter: &Painter, proj: &Proj) {
    if app.viewport.gpu.is_some() {
        return;
    }
    let clouds: Vec<&PointCloud> = solidcraft_engine::view::shown_point_clouds(&app.session).collect();
    let total: usize = clouds.iter().map(|c| c.len()).sum();
    if total == 0 {
        return;
    }
    let stride = total.div_ceil(MAX_CPU_POINTS).max(1);
    let mut shapes = Vec::new();
    for c in clouds {
        let r = (c.point_size as f32 / painter.ctx().pixels_per_point() * 0.5).max(0.5);
        let shade = c.shade();
        for i in (0..c.len()).step_by(stride) {
            let Some(p) = c.world_point(i) else { continue };
            let Some(q) = proj.to_screen(p) else { continue };
            shapes.push(Shape::rect_filled(egui::Rect::from_center_size(q, egui::vec2(r * 2.0, r * 2.0)), 0.0, crate::theme::model_color(shade(i))));
        }
    }
    painter.extend(shapes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_made_again_only_when_the_cloud_changes_and_its_style_follows_it() {
        let mut h = crate::scenario::Harness::new();
        let dir = std::env::temp_dir().join(format!("solidcraft-ui-cloud-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("two.xyz");
        std::fs::write(&path, "0 0 0 255 0 0\n10 0 0 0 255 0\n").unwrap();
        let r = h.app.run("pointcloud.insert", serde_json::json!({"path": path.to_string_lossy(), "point_size": 3})).unwrap();
        let id = r["cloud"].as_u64().unwrap();
        h.app.run("browser.collapse_all", serde_json::json!({})).unwrap();
        assert!(!crate::browser::is_open(&h.app, "point_clouds", true));
        h.app.run("browser.expand_all", serde_json::json!({})).unwrap();
        assert!(crate::browser::is_open(&h.app, "point_clouds", true));
        let first = gpu_clouds(&h.app);
        assert_eq!(first.len(), 1);
        let size = solidcraft_gpu::view::CELL_SIZE;
        assert_eq!((first[0].dots.len(), first[0].segments.len()), (2 * size, 0));
        // The second point: at x = 10, green.
        assert_eq!(&first[0].dots[size..size + 4], &10f32.to_le_bytes());
        assert_eq!(&first[0].dots[2 * size - 4..2 * size - 1], &[0, 255, 0]);
        assert_eq!(first[0].style.point_size, 3.0);
        assert_eq!(gpu_clouds(&h.app)[0].version, first[0].version, "unchanged: the same records");
        h.app.run("pointcloud.edit", serde_json::json!({"cloud": id, "color": [0, 0, 1]})).unwrap();
        let blue = gpu_clouds(&h.app);
        assert_ne!(blue[0].version, first[0].version);
        assert_eq!(&blue[0].dots[size - 4..size - 1], &[0, 0, 255]);
        // By height: a ramp over the range, the range a style alone.
        h.app.run("pointcloud.edit", serde_json::json!({"cloud": id, "color_by": "height", "height_range": [0, 1]})).unwrap();
        let ramp = gpu_clouds(&h.app);
        assert_eq!((ramp[0].style.mode, ramp[0].style.range), (ColorMode::Ramp, [0.0, 1.0]));
        h.app.run("pointcloud.edit", serde_json::json!({"cloud": id, "height_range": [0, 2]})).unwrap();
        let wider = gpu_clouds(&h.app);
        assert_eq!((wider[0].version, wider[0].style.range), (ramp[0].version, [0.0, 2.0]), "a new range uploads nothing");
        // Hidden, alone or with its folder: nothing drawn; the view frames it while shown.
        assert!(!solidcraft_engine::view::bounds(&h.app.session).is_empty());
        let first_bounds = crate::viewport::model_bounds(&h.app);
        assert!(!first_bounds.is_empty());
        assert_eq!(crate::viewport::model_bounds(&h.app), first_bounds, "cached sketch bounds keep the point cloud extent");
        h.app.run("pointcloud.edit", serde_json::json!({"cloud": id, "visible": false})).unwrap();
        assert!(gpu_clouds(&h.app).is_empty());
        assert!(solidcraft_engine::view::bounds(&h.app.session).is_empty());
        assert!(crate::viewport::model_bounds(&h.app).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
