use egui::{
    Align2, Area, Color32, ColorImage, Context, CornerRadius, CursorIcon, FontId, Id, Key, Pos2,
    Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2, ViewportBuilder,
    ViewportCommand, ViewportId, pos2, vec2,
};

use crate::capture::MonitorShot;

pub const COLORS: [Color32; 5] = [
    Color32::from_rgb(229, 72, 77),  // red
    Color32::from_rgb(255, 197, 61), // yellow
    Color32::from_rgb(70, 180, 90),  // green
    Color32::from_rgb(62, 133, 240), // blue
    Color32::BLACK,
];

#[derive(Clone, Copy, PartialEq)]
pub enum Tool {
    Select,
    Pen,
    Line,
    Arrow,
    Rect,
    Text,
}

/// All geometry in physical px relative to the owning monitor's image.
#[derive(Clone)]
pub enum Annotation {
    Pen {
        points: Vec<Pos2>,
        color: Color32,
    },
    Line {
        from: Pos2,
        to: Pos2,
        color: Color32,
    },
    Arrow {
        from: Pos2,
        to: Pos2,
        color: Color32,
    },
    Rect {
        rect: Rect,
        color: Color32,
    },
    Text {
        pos: Pos2,
        text: String,
        color: Color32,
    },
}

enum Selection {
    None,
    Dragging { start: Pos2, cur: Pos2 },
    Placed { rect: Rect },
}

enum DragOp {
    MoveSel { last: Pos2 },
    Resize { handle: usize },
    Pen { points: Vec<Pos2> },
    Shape { start: Pos2, cur: Pos2 },
}

struct TextDraft {
    pos: Pos2, // phys
    buffer: String,
}

pub enum Outcome {
    Cancel,
    Copy,
    Save,
}

fn overlay_id(i: usize) -> ViewportId {
    ViewportId::from_hash_of(("glimt-overlay", i))
}

/// Persistent overlay: one fullscreen viewport per monitor, created once at startup
/// and kept alive (hidden) between captures. Window + swapchain creation is by far
/// the slowest part of showing the overlay, so it must not sit on the PrtSc path.
pub struct Overlay {
    monitors: Vec<(i32, i32, i32, i32)>,
    scales: Vec<Option<f32>>,
    animations_disabled: bool,
    cap: Option<Capture>,
}

/// Everything belonging to one capture session; dropped when the overlay closes.
struct Capture {
    shots: Vec<MonitorShot>,
    textures: Vec<TextureHandle>,
    sel: Selection,
    active_monitor: Option<usize>,
    annotations: Vec<Annotation>,
    tool: Tool,
    color: Color32,
    drag: Option<DragOp>,
    text_draft: Option<TextDraft>,
    frames: u32,
}

impl Overlay {
    pub fn new() -> Self {
        let monitors = crate::capture::monitor_rects();
        let n = monitors.len();
        Overlay {
            monitors,
            scales: vec![None; n],
            animations_disabled: false,
            cap: None,
        }
    }

    pub fn active(&self) -> bool {
        self.cap.is_some()
    }

    pub fn start(&mut self, ctx: &Context, shots: Vec<MonitorShot>) {
        let rects: Vec<_> = shots.iter().map(|s| s.rect).collect();
        if rects != self.monitors {
            // Monitor layout changed since the windows were created; they get
            // recreated/moved by the next frame's builders.
            self.monitors = rects;
            self.scales = vec![None; self.monitors.len()];
            self.animations_disabled = false;
        }
        // The RGBA -> Color32 conversion is the biggest CPU cost after capture
        // itself; convert all monitors in parallel.
        let images: Vec<ColorImage> = std::thread::scope(|s| {
            let handles: Vec<_> = shots
                .iter()
                .map(|shot| {
                    s.spawn(move || {
                        ColorImage::from_rgba_unmultiplied(
                            [shot.image.width() as usize, shot.image.height() as usize],
                            shot.image.as_raw(),
                        )
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("image conversion thread panicked"))
                .collect()
        });
        let textures = images
            .into_iter()
            .enumerate()
            .map(|(i, img)| ctx.load_texture(format!("shot{i}"), img, TextureOptions::LINEAR))
            .collect();
        self.cap = Some(Capture {
            shots,
            textures,
            sel: Selection::None,
            active_monitor: None,
            annotations: Vec::new(),
            tool: Tool::Select,
            color: COLORS[0],
            drag: None,
            text_draft: None,
            frames: 0,
        });
        ctx.request_repaint();
    }

    /// Hide the overlay windows (kept alive for the next capture) and drop the session.
    pub fn close(&mut self, ctx: &Context) {
        for i in 0..self.monitors.len() {
            ctx.send_viewport_cmd_to(overlay_id(i), ViewportCommand::Visible(false));
        }
        self.cap = None;
        ctx.request_repaint();
    }

    pub fn export_data(&self) -> Option<(&image::RgbaImage, Rect, &[Annotation])> {
        let cap = self.cap.as_ref()?;
        let mon = cap.active_monitor?;
        let Selection::Placed { rect } = cap.sel else {
            return None;
        };
        Some((&cap.shots[mon].image, rect, &cap.annotations))
    }

    pub fn scale_of(&self, monitor: usize) -> f32 {
        self.scales.get(monitor).copied().flatten().unwrap_or(1.0)
    }

    /// x, y, w, h in physical virtual-screen px.
    pub fn monitor_rect(&self, monitor: usize) -> (i32, i32, i32, i32) {
        self.monitors[monitor]
    }

    /// Declare one fullscreen viewport per monitor; returns Some when the overlay
    /// should close.
    ///
    /// A fresh capture paints one frame while the windows are still hidden and is
    /// revealed on the next. Showing before painting would composite stale or
    /// uninitialized content, and DWM's window-open fade (disabled below) would
    /// animate it — both read as a flash instead of an instant freeze.
    pub fn show_all(&mut self, ctx: &Context) -> Option<Outcome> {
        let mut outcome = None;
        let fallback_scale = ctx.pixels_per_point();
        let visible = self.cap.as_ref().is_some_and(|c| c.frames >= 1);
        let n = self.monitors.len();
        for i in 0..n {
            let scale = self.scales[i].unwrap_or(fallback_scale);
            let (x, y, w, h) = self.monitors[i];
            let builder = ViewportBuilder::default()
                .with_title("Glimt")
                .with_position(pos2(x as f32 / scale, y as f32 / scale))
                .with_inner_size(vec2(w as f32 / scale, h as f32 / scale))
                .with_decorations(false)
                .with_resizable(false)
                .with_always_on_top()
                .with_taskbar(false)
                .with_visible(visible);
            ctx.show_viewport_immediate(overlay_id(i), builder, |ui, _| {
                if let Some(o) = self.monitor_ui(ui, i) {
                    outcome = Some(o);
                }
            });
        }
        if !self.animations_disabled {
            // The windows exist (hidden) once declared; the transition-disable lands
            // before their first composition, killing the DWM open fade.
            disable_open_animations();
            self.animations_disabled = true;
        }
        if let Some(cap) = &mut self.cap {
            cap.frames += 1;
            if cap.frames == 2 {
                // First visible frame: grab keyboard focus so Esc works before any click.
                ctx.send_viewport_cmd_to(overlay_id(0), ViewportCommand::Focus);
            }
            if cap.frames <= 2 {
                ctx.request_repaint();
            }
        }
        outcome
    }

    fn monitor_ui(&mut self, ui: &mut egui::Ui, i: usize) -> Option<Outcome> {
        let ctx = ui.ctx().clone();
        // Measure the real per-monitor scale so next frame's builder positions correctly.
        self.scales[i] = Some(ctx.pixels_per_point());
        let ppp = ctx.pixels_per_point();
        let cap = self.cap.as_mut()?;

        let mut outcome = None;
        {
            let ctx = &ctx;
            let screen = ui.max_rect();
            let painter = ui.painter();

            // Frozen screen content.
            painter.image(
                cap.textures[i].id(),
                screen,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );

            let sel_pts = cap.current_sel_phys().and_then(|(mon, r)| {
                (mon == i).then(|| Rect::from_min_max(r.min / ppp, r.max / ppp))
            });

            // Dim everything but the selection (4 rects punch out the hole).
            // Very light (~15%): just enough to signal "capture active".
            let dim = Color32::from_black_alpha(38);
            match sel_pts {
                Some(s) => {
                    let s = s.intersect(screen);
                    painter.rect_filled(
                        Rect::from_min_max(screen.min, pos2(screen.max.x, s.min.y)),
                        0.0,
                        dim,
                    );
                    painter.rect_filled(
                        Rect::from_min_max(pos2(screen.min.x, s.max.y), screen.max),
                        0.0,
                        dim,
                    );
                    painter.rect_filled(
                        Rect::from_min_max(pos2(screen.min.x, s.min.y), pos2(s.min.x, s.max.y)),
                        0.0,
                        dim,
                    );
                    painter.rect_filled(
                        Rect::from_min_max(pos2(s.max.x, s.min.y), pos2(screen.max.x, s.max.y)),
                        0.0,
                        dim,
                    );
                    painter.rect_stroke(
                        s,
                        0.0,
                        Stroke::new(1.0, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
                None => {
                    painter.rect_filled(screen, 0.0, dim);
                }
            }

            // Committed + in-progress annotations (only on the owning monitor).
            if cap.active_monitor == Some(i) {
                cap.paint_annotations(ui.painter(), ppp);
            }

            let locked_elsewhere = cap.active_monitor.is_some() && cap.active_monitor != Some(i);
            if !locked_elsewhere {
                outcome = cap.interact(ui, ctx, i, ppp);
            }

            // Chrome drawn on top of everything.
            if cap.active_monitor == Some(i) {
                if let Some(s) = sel_pts {
                    cap.draw_badge(ui.painter(), s, screen, ppp);
                    if matches!(cap.sel, Selection::Placed { .. }) {
                        cap.draw_handles(ui.painter(), s);
                    }
                }
                if let Selection::Dragging { cur, .. } = cap.sel {
                    cap.draw_loupe(ui, i, cur, ppp);
                }
            }
            if cap.active_monitor == Some(i)
                && matches!(cap.sel, Selection::Placed { .. })
                && let Some(s) = sel_pts
            {
                cap.toolbar(ctx, s, ui.max_rect());
                cap.text_editor(ctx, ppp);
            }
        }

        if let Some(o) = cap.handle_keys(&ctx, i) {
            outcome = Some(o);
        }
        outcome
    }
}

impl Capture {
    fn current_sel_phys(&self) -> Option<(usize, Rect)> {
        let mon = self.active_monitor?;
        match &self.sel {
            Selection::None => None,
            Selection::Dragging { start, cur } => Some((mon, Rect::from_two_pos(*start, *cur))),
            Selection::Placed { rect } => Some((mon, *rect)),
        }
    }

    fn interact(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &Context,
        i: usize,
        ppp: f32,
    ) -> Option<Outcome> {
        let screen = ui.max_rect();
        let resp = ui.interact(screen, Id::new(("bg", i)), Sense::click_and_drag());
        let pointer_pts = resp.hover_pos().or(resp.interact_pointer_pos());
        let pointer_phys = pointer_pts.map(|p| pos2(p.x * ppp, p.y * ppp));

        if matches!(self.sel, Selection::None | Selection::Dragging { .. }) {
            ctx.set_cursor_icon(CursorIcon::Crosshair);
        }

        if resp.drag_started() {
            // Anchor at the press position: by the time egui's drag threshold trips,
            // the pointer has already moved a few px past where the user pressed.
            let origin_phys = ctx
                .input(|inp| inp.pointer.press_origin())
                .map(|o| pos2(o.x * ppp, o.y * ppp));
            let p = origin_phys.or(pointer_phys)?;
            match (&self.sel, self.active_monitor) {
                (Selection::None, _) | (_, None) => {
                    self.active_monitor = Some(i);
                    self.sel = Selection::Dragging { start: p, cur: p };
                }
                (Selection::Placed { rect }, Some(mon)) if mon == i => {
                    let rect = *rect;
                    let rect_pts = Rect::from_min_max(rect.min / ppp, rect.max / ppp);
                    if self.tool == Tool::Select {
                        if let Some(h) = hit_handle(rect_pts, pointer_pts?) {
                            self.drag = Some(DragOp::Resize { handle: h });
                        } else if rect.contains(p) {
                            self.drag = Some(DragOp::MoveSel { last: p });
                        } else {
                            self.sel = Selection::Dragging { start: p, cur: p };
                            self.drag = None;
                        }
                    } else if rect.contains(p) {
                        self.commit_text_draft();
                        self.drag = Some(match self.tool {
                            Tool::Pen => DragOp::Pen { points: vec![p] },
                            Tool::Text => return None, // text is click-driven, below
                            _ => DragOp::Shape { start: p, cur: p },
                        });
                    } else {
                        self.sel = Selection::Dragging { start: p, cur: p };
                        self.drag = None;
                    }
                }
                _ => {}
            }
        }

        if resp.dragged()
            && let Some(p) = pointer_phys
        {
            {
                let p = self.clamp_to_monitor(i, p);
                let bounds = self.monitor_size(i);
                match (&mut self.sel, &mut self.drag) {
                    (Selection::Dragging { cur, .. }, _) => *cur = p,
                    (Selection::Placed { rect }, Some(DragOp::MoveSel { last })) => {
                        let delta = p - *last;
                        *rect = clamp_rect(rect.translate(delta), bounds);
                        *last = p;
                    }
                    (Selection::Placed { rect }, Some(DragOp::Resize { handle })) => {
                        *rect = resize_rect(*rect, *handle, p);
                    }
                    (_, Some(DragOp::Pen { points })) => {
                        if points.last().is_none_or(|l| (*l - p).length() > 1.5) {
                            points.push(p);
                        }
                    }
                    (_, Some(DragOp::Shape { cur, .. })) => *cur = p,
                    _ => {}
                }
            }
        }

        if resp.drag_stopped() {
            match self.drag.take() {
                Some(DragOp::Pen { points }) => {
                    if points.len() > 1 {
                        self.annotations.push(Annotation::Pen {
                            points,
                            color: self.color,
                        });
                    }
                }
                Some(DragOp::Shape { start, cur }) if (cur - start).length() > 2.0 => {
                    self.annotations.push(match self.tool {
                        Tool::Line => Annotation::Line {
                            from: start,
                            to: cur,
                            color: self.color,
                        },
                        Tool::Arrow => Annotation::Arrow {
                            from: start,
                            to: cur,
                            color: self.color,
                        },
                        _ => Annotation::Rect {
                            rect: Rect::from_two_pos(start, cur),
                            color: self.color,
                        },
                    });
                }
                _ => {}
            }
            if let Selection::Dragging { start, cur } = self.sel {
                self.sel = Selection::Placed {
                    rect: Rect::from_two_pos(start, cur),
                };
            }
        }

        // Text tool: click inside the selection opens a floating editor.
        if resp.clicked() && self.tool == Tool::Text {
            if let (Some(p), Selection::Placed { rect }) = (pointer_phys, &self.sel) {
                if rect.contains(p) {
                    self.commit_text_draft();
                    self.text_draft = Some(TextDraft {
                        pos: p,
                        buffer: String::new(),
                    });
                } else {
                    self.commit_text_draft();
                }
            }
        } else if resp.clicked() {
            self.commit_text_draft();
        }

        None
    }

    fn handle_keys(&mut self, ctx: &Context, i: usize) -> Option<Outcome> {
        // Only the focused viewport actually receives key events; checking everywhere is
        // harmless. Use per-event modifiers, not frame-level ones: a fast Ctrl+key can
        // arrive and release entirely within one frame.
        let (mut esc, mut enter, mut copy, mut save, mut undo) =
            (false, false, false, false, false);
        ctx.input(|inp| {
            for e in &inp.events {
                match e {
                    // egui_winit turns Ctrl+C into Event::Copy; no Key event arrives.
                    egui::Event::Copy => copy = true,
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => match key {
                        Key::Escape => esc = true,
                        Key::Enter => enter = true,
                        Key::C if modifiers.ctrl => copy = true,
                        Key::S if modifiers.ctrl => save = true,
                        Key::Z if modifiers.ctrl => undo = true,
                        _ => {}
                    },
                    _ => {}
                }
            }
        });

        if self.text_draft.is_some() {
            if esc {
                self.text_draft = None;
            } else if enter {
                self.commit_text_draft();
            }
            return None;
        }

        if esc {
            return Some(Outcome::Cancel);
        }
        if matches!(self.sel, Selection::Placed { .. }) {
            if copy {
                return Some(Outcome::Copy);
            }
            if save {
                return Some(Outcome::Save);
            }
        }
        if undo {
            self.annotations.pop();
        }

        // Arrow-key nudge, disabled mid-drag.
        if self.drag.is_none() && self.active_monitor == Some(i) {
            let size = self.monitor_size(i);
            if let Selection::Placed { rect } = &mut self.sel {
                // Per-event modifiers again: Shift state at frame level can already be
                // stale when the events coalesced into one frame.
                let delta = ctx.input(|inp| {
                    let mut d = vec2(0.0, 0.0);
                    for e in &inp.events {
                        if let egui::Event::Key {
                            key,
                            pressed: true,
                            modifiers,
                            ..
                        } = e
                        {
                            let step = if modifiers.shift { 10.0 } else { 1.0 };
                            match key {
                                Key::ArrowLeft => d.x -= step,
                                Key::ArrowRight => d.x += step,
                                Key::ArrowUp => d.y -= step,
                                Key::ArrowDown => d.y += step,
                                _ => {}
                            }
                        }
                    }
                    d
                });
                if delta != vec2(0.0, 0.0) {
                    *rect = clamp_rect(rect.translate(delta), size);
                }
            }
        }
        None
    }

    fn commit_text_draft(&mut self) {
        if let Some(draft) = self.text_draft.take()
            && !draft.buffer.trim().is_empty()
        {
            self.annotations.push(Annotation::Text {
                pos: draft.pos,
                text: draft.buffer,
                color: self.color,
            });
        }
    }

    fn monitor_size(&self, i: usize) -> Vec2 {
        let (_, _, w, h) = self.shots[i].rect;
        vec2(w as f32, h as f32)
    }

    fn clamp_to_monitor(&self, i: usize, p: Pos2) -> Pos2 {
        let s = self.monitor_size(i);
        pos2(p.x.clamp(0.0, s.x), p.y.clamp(0.0, s.y))
    }

    fn paint_annotations(&self, painter: &egui::Painter, ppp: f32) {
        let to_pts = |p: Pos2| p / ppp;
        let mut all: Vec<Annotation> = self.annotations.clone();
        match &self.drag {
            Some(DragOp::Pen { points }) => all.push(Annotation::Pen {
                points: points.clone(),
                color: self.color,
            }),
            Some(DragOp::Shape { start, cur }) => all.push(match self.tool {
                Tool::Line => Annotation::Line {
                    from: *start,
                    to: *cur,
                    color: self.color,
                },
                Tool::Arrow => Annotation::Arrow {
                    from: *start,
                    to: *cur,
                    color: self.color,
                },
                _ => Annotation::Rect {
                    rect: Rect::from_two_pos(*start, *cur),
                    color: self.color,
                },
            }),
            _ => {}
        }

        for a in &all {
            match a {
                Annotation::Pen { points, color } => {
                    painter.add(egui::Shape::line(
                        points.iter().map(|p| to_pts(*p)).collect(),
                        Stroke::new(3.0 / ppp, *color),
                    ));
                }
                Annotation::Line { from, to, color } => {
                    painter
                        .line_segment([to_pts(*from), to_pts(*to)], Stroke::new(3.0 / ppp, *color));
                }
                Annotation::Arrow { from, to, color } => {
                    let (f, t) = (to_pts(*from), to_pts(*to));
                    painter.line_segment([f, t], Stroke::new(3.0 / ppp, *color));
                    for p in arrow_head(f, t, 12.0 / ppp) {
                        painter.add(egui::Shape::convex_polygon(p, *color, Stroke::NONE));
                    }
                }
                Annotation::Rect { rect, color } => {
                    painter.rect_stroke(
                        Rect::from_min_max(to_pts(rect.min), to_pts(rect.max)),
                        0.0,
                        Stroke::new(3.0 / ppp, *color),
                        StrokeKind::Middle,
                    );
                }
                Annotation::Text { pos, text, color } => {
                    painter.text(
                        to_pts(*pos),
                        Align2::LEFT_TOP,
                        text,
                        FontId::proportional(18.0 / ppp),
                        *color,
                    );
                }
            }
        }
    }

    fn draw_badge(&self, painter: &egui::Painter, sel_pts: Rect, screen: Rect, ppp: f32) {
        let (w, h) = (
            (sel_pts.width() * ppp).round(),
            (sel_pts.height() * ppp).round(),
        );
        let text = format!("{w}\u{00D7}{h}", w = w as i64, h = h as i64);
        let font = FontId::proportional(12.0);
        let galley = painter.layout_no_wrap(text, font.clone(), Color32::WHITE);
        let pad = vec2(6.0, 3.0);
        let mut pos = sel_pts.max + vec2(4.0, 4.0);
        if pos.x + galley.size().x + pad.x * 2.0 > screen.max.x
            || pos.y + galley.size().y + pad.y * 2.0 > screen.max.y
        {
            pos = sel_pts.max - galley.size() - pad * 2.0 - vec2(4.0, 4.0);
        }
        let rect = Rect::from_min_size(pos, galley.size() + pad * 2.0);
        painter.rect_filled(rect, 4.0, Color32::from_black_alpha(200));
        painter.galley(rect.min + pad, galley, Color32::WHITE);
    }

    fn draw_loupe(&self, ui: &egui::Ui, i: usize, cur_phys: Pos2, ppp: f32) {
        const SIZE: f32 = 120.0;
        const ZOOM: f32 = 8.0;
        let painter = ui.painter();
        let screen = ui.max_rect();
        let cur_pts = cur_phys / ppp;
        let mut origin = cur_pts + vec2(24.0, 24.0);
        if origin.x + SIZE > screen.max.x {
            origin.x = cur_pts.x - 24.0 - SIZE;
        }
        if origin.y + SIZE > screen.max.y {
            origin.y = cur_pts.y - 24.0 - SIZE;
        }
        let rect = Rect::from_min_size(origin, vec2(SIZE, SIZE));

        let tex = &self.textures[i];
        let (tw, th) = (
            self.shots[i].image.width() as f32,
            self.shots[i].image.height() as f32,
        );
        // The loupe shows SIZE*ppp/ZOOM physical px around the cursor at ~8x.
        let half = vec2(SIZE * ppp / ZOOM / 2.0 / tw, SIZE * ppp / ZOOM / 2.0 / th);
        let center = pos2(cur_phys.x / tw, cur_phys.y / th);
        let uv = Rect::from_min_max(center - half, center + half);

        let radius = CornerRadius::same((SIZE / 2.0) as u8);
        let mut shape = egui::epaint::RectShape::filled(rect, radius, Color32::WHITE);
        shape.brush = Some(std::sync::Arc::new(egui::epaint::Brush {
            fill_texture_id: tex.id(),
            uv,
        }));
        painter.add(shape);
        painter.circle_stroke(rect.center(), SIZE / 2.0, Stroke::new(2.0, Color32::WHITE));
        // Crosshair.
        let c = rect.center();
        let ch = Stroke::new(1.0, Color32::from_white_alpha(180));
        painter.line_segment(
            [pos2(rect.min.x + 4.0, c.y), pos2(rect.max.x - 4.0, c.y)],
            ch,
        );
        painter.line_segment(
            [pos2(c.x, rect.min.y + 4.0), pos2(c.x, rect.max.y - 4.0)],
            ch,
        );
    }

    fn draw_handles(&self, painter: &egui::Painter, sel_pts: Rect) {
        for p in handle_positions(sel_pts) {
            let r = Rect::from_center_size(p, vec2(6.0, 6.0));
            painter.rect_filled(r, 1.0, Color32::WHITE);
            painter.rect_stroke(
                r,
                1.0,
                Stroke::new(1.0, Color32::BLACK),
                StrokeKind::Outside,
            );
        }
    }

    fn toolbar(&mut self, ctx: &Context, sel_pts: Rect, screen: Rect) {
        const BAR_H: f32 = 36.0;
        let mut pos = pos2(sel_pts.min.x, sel_pts.max.y + 10.0);
        if pos.y + BAR_H > screen.max.y {
            pos.y = sel_pts.min.y - 10.0 - BAR_H;
        }
        Area::new(Id::new("glimt-toolbar"))
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(Color32::from_rgb(27, 30, 40))
                    .corner_radius(6.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| self.photo_tools(ui));
                    });
            });
    }

    fn photo_tools(&mut self, ui: &mut egui::Ui) {
        // Glyphs restricted to what egui's built-in fonts cover.
        let tools = [
            (Tool::Select, "\u{2196}"),
            (Tool::Pen, "\u{270F}"),
            (Tool::Line, "/"),
            (Tool::Arrow, "\u{27A1}"),
            (Tool::Rect, "\u{25FB}"),
            (Tool::Text, "T"),
        ];
        for (tool, label) in tools {
            if ui.selectable_label(self.tool == tool, label).clicked() {
                self.commit_text_draft();
                self.tool = tool;
            }
        }
        ui.separator();
        for color in COLORS {
            let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
            let center = rect.center();
            ui.painter().circle_filled(center, 7.0, color);
            if self.color == color {
                ui.painter()
                    .circle_stroke(center, 8.5, Stroke::new(1.5, Color32::WHITE));
            }
            if resp.clicked() {
                self.color = color;
            }
        }
        ui.separator();
        if ui
            .button("\u{21B6}")
            .on_hover_text("Undo (Ctrl+Z)")
            .clicked()
        {
            self.annotations.pop();
        }
    }

    fn text_editor(&mut self, ctx: &Context, ppp: f32) {
        let Some(draft) = &mut self.text_draft else {
            return;
        };
        let pos = draft.pos / ppp;
        Area::new(Id::new("glimt-textedit"))
            .fixed_pos(pos)
            .show(ctx, |ui| {
                let edit = egui::TextEdit::singleline(&mut draft.buffer)
                    .font(FontId::proportional(18.0 / ppp))
                    .text_color(self.color)
                    .desired_width(220.0)
                    .frame(egui::Frame::NONE);
                let resp = ui.add(edit);
                resp.request_focus();
            });
    }
}

/// Corner + edge-midpoint handles; index order matters for `resize_rect`.
fn handle_positions(r: Rect) -> [Pos2; 8] {
    [
        r.min,
        pos2(r.center().x, r.min.y),
        pos2(r.max.x, r.min.y),
        pos2(r.max.x, r.center().y),
        r.max,
        pos2(r.center().x, r.max.y),
        pos2(r.min.x, r.max.y),
        pos2(r.min.x, r.center().y),
    ]
}

fn hit_handle(sel_pts: Rect, pointer: Pos2) -> Option<usize> {
    handle_positions(sel_pts)
        .iter()
        .enumerate()
        .filter(|(_, p)| (**p - pointer).length() <= 12.0)
        .min_by(|a, b| {
            (*a.1 - pointer)
                .length()
                .total_cmp(&(*b.1 - pointer).length())
        })
        .map(|(i, _)| i)
}

fn resize_rect(r: Rect, handle: usize, p: Pos2) -> Rect {
    let mut min = r.min;
    let mut max = r.max;
    match handle {
        0 => (min.x, min.y) = (p.x, p.y),
        1 => min.y = p.y,
        2 => (max.x, min.y) = (p.x, p.y),
        3 => max.x = p.x,
        4 => (max.x, max.y) = (p.x, p.y),
        5 => max.y = p.y,
        6 => (min.x, max.y) = (p.x, p.y),
        _ => min.x = p.x,
    }
    Rect::from_two_pos(min, max)
}

fn clamp_rect(r: Rect, bounds: Vec2) -> Rect {
    let mut r = r;
    if r.min.x < 0.0 {
        r = r.translate(vec2(-r.min.x, 0.0));
    }
    if r.min.y < 0.0 {
        r = r.translate(vec2(0.0, -r.min.y));
    }
    if r.max.x > bounds.x {
        r = r.translate(vec2(bounds.x - r.max.x, 0.0));
    }
    if r.max.y > bounds.y {
        r = r.translate(vec2(0.0, bounds.y - r.max.y));
    }
    r
}

/// Disable DWM window transitions on every top-level window of this process so the
/// overlay viewports appear without the system's window-open fade.
fn disable_open_animations() {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId};

    unsafe extern "system" fn cb(hwnd: HWND, _: LPARAM) -> windows::core::BOOL {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == GetCurrentProcessId() {
                let disable: i32 = 1;
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_TRANSITIONS_FORCEDISABLED,
                    &disable as *const i32 as *const _,
                    std::mem::size_of::<i32>() as u32,
                );
            }
        }
        true.into()
    }
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(0));
    }
}

/// Filled triangle head sized ~4x stroke width, split into the polygons egui needs.
fn arrow_head(from: Pos2, to: Pos2, size: f32) -> Vec<Vec<Pos2>> {
    let dir = (to - from).normalized();
    if !dir.is_finite() {
        return vec![];
    }
    let perp = vec2(-dir.y, dir.x);
    let base = to - dir * size;
    vec![vec![
        to,
        base + perp * (size * 0.5),
        base - perp * (size * 0.5),
    ]]
}
