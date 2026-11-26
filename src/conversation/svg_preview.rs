use anyhow::Result;
use gpui::RenderImage;
use resvg::{
    tiny_skia::{Pixmap, Transform},
    usvg,
};
use std::{
    io::Cursor,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};

pub(super) fn rasterize(source: &str, max_edge: f32) -> Result<Arc<RenderImage>> {
    static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
        let mut fonts = usvg::fontdb::Database::new();
        fonts.load_system_fonts();
        Arc::new(fonts)
    });
    let remaining_pixels = AtomicU64::new(8 * 1024 * 1024);
    let options = usvg::Options {
        fontdb: FONTS.clone(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, bytes, _| {
                let reader = image::ImageReader::new(Cursor::new(bytes.as_slice()))
                    .with_guessed_format()
                    .ok()?;
                let kind = match reader.format()? {
                    image::ImageFormat::Png => usvg::ImageKind::PNG(bytes.clone()),
                    image::ImageFormat::Jpeg => usvg::ImageKind::JPEG(bytes.clone()),
                    image::ImageFormat::Gif => usvg::ImageKind::GIF(bytes.clone()),
                    image::ImageFormat::WebP => usvg::ImageKind::WEBP(bytes.clone()),
                    _ => return None,
                };
                let (width, height) = reader.into_dimensions().ok()?;
                let pixels = u64::from(width) * u64::from(height);
                remaining_pixels
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                        remaining.checked_sub(pixels)
                    })
                    .ok()?;
                Some(kind)
            }),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(source, &options)?;
    let size = tree.size();
    let scale = (max_edge / size.width().max(size.height())).min(2.);
    let width = (size.width() * scale).ceil().max(1.) as u32;
    let height = (size.height() * scale).ceil().max(1.) as u32;
    let mut pixmap =
        Pixmap::new(width, height).ok_or_else(|| anyhow::anyhow!("Invalid SVG size"))?;
    resvg::render(
        &tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let pixels = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.blue(), color.green(), color.red(), color.alpha()]
        })
        .collect();
    let buffer = image::RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| anyhow::anyhow!("Invalid SVG pixels"))?;
    Ok(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};

    #[test]
    fn renders_embedded_png() {
        let png = include_bytes!("../../tests/fixtures/attachments/diagram.png");
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><image width="100" height="100" href="data:image/png;base64,{}"/></svg>"#,
            STANDARD.encode(png)
        );
        let image = rasterize(&svg, 200.).unwrap();
        assert!(
            image
                .as_bytes(0)
                .unwrap()
                .chunks_exact(4)
                .any(|p| p[3] == 255)
        );
    }
}
