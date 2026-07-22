use ab_glyph::{Font, FontRef, ScaleFont};
use anyhow::{Context, Result};
use egui::{Color32, Pos2, Rect};
use tiny_skia::{FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::overlay::Annotation;

const FONT_BYTES: &[u8] = include_bytes!("../assets/Inter-Medium.ttf");
const STROKE_W: f32 = 3.0;
const TEXT_PX: f32 = 18.0;

/// Crop the selection from the monitor image and rasterize annotations into it.
/// Everything is already in physical px relative to the monitor image.
pub fn render(
    shot: &image::RgbaImage,
    sel: Rect,
    annotations: &[Annotation],
) -> Result<image::RgbaImage> {
    let x = (sel.min.x.round() as i64).clamp(0, shot.width() as i64 - 1) as u32;
    let y = (sel.min.y.round() as i64).clamp(0, shot.height() as i64 - 1) as u32;
    let w = (sel.width().round() as i64).clamp(1, (shot.width() - x) as i64) as u32;
    let h = (sel.height().round() as i64).clamp(1, (shot.height() - y) as i64) as u32;
    let crop = image::imageops::crop_imm(shot, x, y, w, h).to_image();

    // The source is fully opaque, so premultiplied == straight and Pixmap round-trips losslessly.
    let mut pixmap = Pixmap::from_vec(
        crop.into_raw(),
        tiny_skia::IntSize::from_wh(w, h).context("bad crop size")?,
    )
    .context("pixmap from crop")?;

    let off = egui::vec2(x as f32, y as f32);
    for a in annotations {
        match a {
            Annotation::Pen { points, color } => {
                if points.len() < 2 {
                    continue;
                }
                let mut pb = PathBuilder::new();
                pb.move_to(points[0].x - off.x, points[0].y - off.y);
                for p in &points[1..] {
                    pb.line_to(p.x - off.x, p.y - off.y);
                }
                stroke(&mut pixmap, pb, *color, STROKE_W);
            }
            Annotation::Line { from, to, color } => {
                let mut pb = PathBuilder::new();
                pb.move_to(from.x - off.x, from.y - off.y);
                pb.line_to(to.x - off.x, to.y - off.y);
                stroke(&mut pixmap, pb, *color, STROKE_W);
            }
            Annotation::Arrow { from, to, color } => {
                let f = *from - off;
                let t = *to - off;
                let dir = (t - f).normalized();
                if dir.is_finite() {
                    let head = STROKE_W * 4.0;
                    let base = t - dir * head;
                    // Stop the shaft at the head base so it doesn't poke past the tip.
                    let mut pb = PathBuilder::new();
                    pb.move_to(f.x, f.y);
                    pb.line_to(base.x, base.y);
                    stroke(&mut pixmap, pb, *color, STROKE_W);

                    let perp = egui::vec2(-dir.y, dir.x) * (head * 0.5);
                    let mut pb = PathBuilder::new();
                    pb.move_to(t.x, t.y);
                    pb.line_to(base.x + perp.x, base.y + perp.y);
                    pb.line_to(base.x - perp.x, base.y - perp.y);
                    pb.close();
                    fill(&mut pixmap, pb, *color);
                }
            }
            Annotation::Rect { rect, color } => {
                if let Some(r) = tiny_skia::Rect::from_ltrb(
                    rect.min.x - off.x,
                    rect.min.y - off.y,
                    rect.max.x - off.x,
                    rect.max.y - off.y,
                ) {
                    let mut pb = PathBuilder::new();
                    pb.push_rect(r);
                    stroke(&mut pixmap, pb, *color, STROKE_W);
                }
            }
            Annotation::Text { pos, text, color } => {
                draw_text(&mut pixmap, *pos - off, text, *color)?;
            }
        }
    }

    image::RgbaImage::from_raw(w, h, pixmap.take()).context("pixmap size mismatch")
}

fn paint(color: Color32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(color.r(), color.g(), color.b(), color.a());
    p.anti_alias = true;
    p
}

fn stroke(pixmap: &mut Pixmap, pb: PathBuilder, color: Color32, width: f32) {
    if let Some(path) = pb.finish() {
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
    }
}

fn fill(pixmap: &mut Pixmap, pb: PathBuilder, color: Color32) {
    if let Some(path) = pb.finish() {
        pixmap.fill_path(
            &path,
            &paint(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn draw_text(pixmap: &mut Pixmap, pos: Pos2, text: &str, color: Color32) -> Result<()> {
    let font = FontRef::try_from_slice(FONT_BYTES).context("embedded font")?;
    let scaled = font.as_scaled(ab_glyph::PxScale::from(TEXT_PX));
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let data = pixmap.data_mut();

    // pos is the text's top-left (matches the overlay's Align2::LEFT_TOP).
    let baseline = pos.y + scaled.ascent();
    let mut pen_x = pos.x;
    let mut prev: Option<ab_glyph::GlyphId> = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(p) = prev {
            pen_x += scaled.kern(p, id);
        }
        let glyph = id.with_scale_and_position(TEXT_PX, ab_glyph::point(pen_x, baseline));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, cov| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || px >= pw || py >= ph {
                    return;
                }
                let idx = ((py * pw + px) * 4) as usize;
                let a = cov.clamp(0.0, 1.0);
                for (c, src) in [color.r(), color.g(), color.b()].into_iter().enumerate() {
                    let dst = data[idx + c] as f32;
                    data[idx + c] = (src as f32 * a + dst * (1.0 - a)) as u8;
                }
            });
        }
        pen_x += scaled.h_advance(id);
        prev = Some(id);
    }
    Ok(())
}

pub fn to_clipboard(img: &image::RgbaImage) -> Result<()> {
    let (w, h) = img.dimensions();
    arboard::Clipboard::new()?
        .set_image(arboard::ImageData {
            width: w as usize,
            height: h as usize,
            bytes: img.as_raw().as_slice().into(),
        })
        .context("clipboard set_image")
}

pub fn to_file(img: &image::RgbaImage) -> Result<std::path::PathBuf> {
    let path = crate::config::save_dir()?.join(crate::config::filename_now());
    img.save(&path).context("saving png")?;
    Ok(path)
}
