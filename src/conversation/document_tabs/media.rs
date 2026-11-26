use super::{Preview, attachments};
use crate::conversation::svg_preview;
use gpui::{Image, ImageFormat, RenderImage};
#[cfg(target_os = "macos")]
use image::Frame;
use std::{
    io::{self, Read},
    sync::Arc,
};

const IMAGE_LIMIT: usize = 8 * 1024 * 1024;
const PDF_LIMIT: usize = 32 * 1024 * 1024;
const MAX_PREVIEW_EDGE: f32 = 1200.;

pub(super) enum Media {
    Raster(ImageFormat),
    Svg,
    Pdf,
}

impl Media {
    pub(super) fn detect(bytes: &[u8], extension: &str) -> Option<Self> {
        if bytes.starts_with(b"%PDF-") || extension.eq_ignore_ascii_case("pdf") {
            return Some(Self::Pdf);
        }
        match image::guess_format(bytes) {
            Ok(image::ImageFormat::Png) => Some(Self::Raster(ImageFormat::Png)),
            Ok(image::ImageFormat::Jpeg) => Some(Self::Raster(ImageFormat::Jpeg)),
            Ok(image::ImageFormat::WebP) => Some(Self::Raster(ImageFormat::Webp)),
            Ok(image::ImageFormat::Gif) => Some(Self::Raster(ImageFormat::Gif)),
            Ok(image::ImageFormat::Bmp) => Some(Self::Raster(ImageFormat::Bmp)),
            Ok(image::ImageFormat::Tiff) => Some(Self::Raster(ImageFormat::Tiff)),
            _ if extension.eq_ignore_ascii_case("svg") => Some(Self::Svg),
            _ => None,
        }
    }

    pub(super) fn read(self, file: impl Read, mut bytes: Vec<u8>) -> io::Result<Preview> {
        let limit = match self {
            Self::Pdf => PDF_LIMIT,
            Self::Raster(_) | Self::Svg => IMAGE_LIMIT,
        };
        file.take((limit + 1 - bytes.len()) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Ok(Preview::Notice(match self {
                Self::Pdf => "This PDF exceeds the 32 MiB preview limit.",
                Self::Raster(_) | Self::Svg => "This image is too large to preview.",
            }));
        }
        let (format, thumbnail) = match self {
            Self::Raster(format) => (format, attachments::image_preview(&bytes, true)),
            Self::Svg => (
                ImageFormat::Svg,
                std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|svg| svg_preview::rasterize(svg, MAX_PREVIEW_EDGE).ok()),
            ),
            Self::Pdf => return Ok(pdf_preview(&bytes)),
        };
        Ok(thumbnail.map_or(
            Preview::Notice("Could not preview this image."),
            |thumbnail| Preview::Image {
                thumbnail,
                source: Arc::new(Image::from_bytes(format, bytes)),
            },
        ))
    }
}

#[cfg(target_os = "macos")]
fn pdf_preview(bytes: &[u8]) -> Preview {
    use objc2_core_foundation::CFData;
    use objc2_core_graphics::{CGDataProvider, CGPDFDocument};

    const PAGE_LIMIT: usize = 5;
    let data = CFData::from_bytes(bytes);
    let document = CGDataProvider::with_cf_data(Some(&data))
        .and_then(|provider| CGPDFDocument::with_provider(Some(&provider)));
    let Some(document) = document else {
        return Preview::Notice("Could not open this PDF.");
    };
    if !CGPDFDocument::is_unlocked(Some(&document)) {
        return Preview::Notice("This PDF is password protected.");
    }
    let total = CGPDFDocument::number_of_pages(Some(&document));
    if total == 0 {
        return Preview::Notice("This PDF has no pages to preview.");
    }
    let pages = (1..=total.min(PAGE_LIMIT))
        .map(|number| {
            CGPDFDocument::page(Some(&document), number).and_then(|page| render_pdf_page(&page))
        })
        .collect();
    Preview::Pdf { pages, total }
}

#[cfg(not(target_os = "macos"))]
fn pdf_preview(_: &[u8]) -> Preview {
    Preview::Notice("PDF preview is available on macOS.")
}

#[cfg(target_os = "macos")]
fn render_pdf_page(page: &objc2_core_graphics::CGPDFPage) -> Option<Arc<RenderImage>> {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGBitmapContextGetData, CGColorSpace, CGContext, CGImageAlphaInfo,
        CGImageByteOrderInfo, CGPDFBox, CGPDFPage,
    };

    let page_box = CGPDFBox::CropBox;
    let size = CGPDFPage::box_rect(Some(page), page_box).size;
    let (width, height) = if CGPDFPage::rotation_angle(Some(page)).rem_euclid(180) == 90 {
        (size.height, size.width)
    } else {
        (size.width, size.height)
    };
    if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
        return None;
    }
    let scale = (f64::from(MAX_PREVIEW_EDGE) / width.max(height)).min(2.);
    let width = (width * scale).ceil().max(1.) as u32;
    let height = (height * scale).ceil().max(1.) as u32;
    let stride = width as usize * 4;
    let colors = CGColorSpace::new_device_rgb()?;
    // A null data pointer lets Core Graphics allocate and own the bitmap.
    let context = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            width as usize,
            height as usize,
            8,
            stride,
            Some(&colors),
            CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
        )
    }?;
    let context = Some(&*context);
    let bounds = CGRect::new(
        CGPoint::ZERO,
        CGSize::new(f64::from(width), f64::from(height)),
    );
    CGContext::set_rgb_fill_color(context, 1., 1., 1., 1.);
    CGContext::fill_rect(context, bounds);
    let transform = CGPDFPage::drawing_transform(Some(page), page_box, bounds, 0, true);
    CGContext::concat_ctm(context, transform);
    CGContext::draw_pdf_page(context, Some(page));
    let data = CGBitmapContextGetData(context).cast::<u8>();
    if data.is_null() {
        return None;
    }
    // The context owns stride * height bytes and remains alive during the copy.
    let pixels = unsafe { std::slice::from_raw_parts(data, stride * height as usize) }.to_vec();
    let frame = Frame::new(image::RgbaImage::from_raw(width, height, pixels)?);
    Some(Arc::new(RenderImage::new([frame])))
}

#[cfg(test)]
mod tests {
    use super::{super::read_file, *};
    use std::{fs, path::Path};

    #[test]
    fn previews_bmp_and_tiff() {
        let root = tempfile::tempdir().unwrap();
        let bitmap = image::RgbaImage::from_pixel(4, 2, image::Rgba([192, 64, 32, 255]));
        for (name, format) in [
            ("picture.bmp", ImageFormat::Bmp),
            ("picture.tiff", ImageFormat::Tiff),
        ] {
            let path = Path::new(name);
            bitmap.save(root.path().join(path)).unwrap();
            let Preview::Image { thumbnail, source } = read_file(root.path(), path).unwrap() else {
                panic!("expected an image preview for {name}");
            };
            assert_eq!(source.format, format);
            assert_eq!(&thumbnail.as_bytes(0).unwrap()[..4], &[32, 64, 192, 255]);
        }
    }

    #[test]
    fn renders_svg_instead_of_showing_its_source() {
        let root = tempfile::tempdir().unwrap();
        let path = Path::new("picture.SVG");
        fs::write(
            root.path().join(path),
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="2"><rect width="4" height="2" fill="red"/></svg>"#,
        )
        .unwrap();
        let Preview::Image { thumbnail, source } = read_file(root.path(), path).unwrap() else {
            panic!("expected an SVG preview");
        };
        assert_eq!(source.format, ImageFormat::Svg);
        assert_eq!(&thumbnail.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn renders_pdf_page_content_upright() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/attachments");
        let Preview::Pdf { pages, total } = read_file(&root, Path::new("report.pdf")).unwrap()
        else {
            panic!("expected a PDF preview");
        };
        assert_eq!(total, 1);
        let page = pages[0].as_ref().unwrap();
        let pixels = page.as_bytes(0).unwrap();
        let (top, bottom) = pixels.split_at(pixels.len() / 2);
        assert!(top.chunks_exact(4).any(|pixel| pixel[0] < 100));
        assert!(bottom.iter().all(|channel| *channel == 255));
    }
}
