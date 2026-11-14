//! Cropping a picked image to a new avatar.
//!
//! [`AvatarCropView`] shows the picture in a square under a circular mask: drag
//! to move it, scroll, pinch or use the slider beside it to zoom. [`CropFrame`]
//! is the math behind that view, kept in the source picture's own pixels so it
//! doesn't depend on how big the view is drawn.
//!
//! Avatars are drawn as circles, so the upload is the circle's bounding square:
//! [`prepare_avatar_upload`] cuts it out of the original full-size picture,
//! scales it to at most [`AVATAR_MAX_SIDE`] pixels and encodes it.

use makepad_widgets::*;
use makepad_widgets::event::DigitId;
use makepad_widgets::image_cache::{ImageBuffer, decode_image_from_data};
use crate::shared::file_upload_modal::{FilePreview, FileUploadMetadata};

/// How far the view zooms in past its smallest zoom (at which the picture just covers the circle).
pub const MAX_ZOOM: f64 = 4.0;
/// The largest side of an uploaded avatar, in pixels. A smaller crop keeps its own size.
pub const AVATAR_MAX_SIDE: u32 = 512;
/// The JPEG quality of an avatar cropped from a JPEG.
const JPEG_QUALITY: u8 = 90;
/// The texture the view draws is never bigger than this: enough for a sharp
/// circle at the largest zoom, without a full-size photo's worth of texture.
const PREVIEW_MAX_SHORT_SIDE: usize = 1536;
const PREVIEW_MAX_LONG_SIDE: usize = 3072;
/// Scrolling this many points zooms in (or out) twice as far.
const SCROLL_POINTS_PER_DOUBLING: f64 = 200.0;
/// How far one press of a zoom button zooms: the cube root of 2, so three
/// presses double the zoom and six go from the smallest zoom to the largest.
pub const ZOOM_BUTTON_STEP: f64 = 1.259_921_049_894_873_2;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    set_type_default() do #(DrawAvatarCropMask::script_shader(vm)){
        ..mod.draw.DrawQuad
        radius: 0.0
        shade_color: uniform(#00000080)
        ring_color: uniform(#ffffffd9)
        // Half the ring's width, in points.
        ring_half_width: uniform(0.75)
        pixel: fn() {
            let size = self.rect_size
            let sdf = Sdf2d.viewport(self.pos * size)
            let c = size * 0.5
            // Past the quad's own edges, so its outermost pixels are darkened in full.
            sdf.rect(-1.0, -1.0, size.x + 2.0, size.y + 2.0)
            sdf.circle(c.x, c.y, self.radius)
            sdf.subtract()
            sdf.fill(self.shade_color)
            sdf.circle(c.x, c.y, self.radius)
            sdf.stroke(self.ring_color, self.ring_half_width)
            return sdf.result
        }
    }

    mod.widgets.AvatarCropView = set_type_default() do #(AvatarCropView::register_widget(vm)) {
        width: 256, height: 256
        circle_inset: 12.0
        // The ground shows where the picture doesn't reach, darkened like the rest outside the circle.
        draw_bg +: { color: #1c1c1c }
    }

    // The zoom slider: 0 is the smallest zoom, 1 the largest, in even steps of the zoom factor.
    mod.widgets.AvatarZoomSlider = mod.widgets.Slider {
        width: Fill, height: 36
        margin: 0, padding: 0
        min: 0.0, max: 1.0, step: 0.0, default: 0.0
        // The wheel over the slider zooms as fast as it does over the picture:
        // there, 400 points of scrolling go from the smallest zoom to the largest.
        scroll_step: 0.3
        // The drag follows the knob: its centre travels from inset + cap / 2 to the far end less the same.
        cap_size: 18.0
        track_inset: 3.0
        label_walk: Walk{width: 0, height: 0}
        text_input +: {width: 0, height: 0, is_read_only: true}
        draw_bg +: {
            label_size: 0
            track_color: uniform(mod.widgets.RINX_BORDER)
            fill_color: uniform(mod.widgets.RINX_ACCENT)
            knob_color: uniform(mod.widgets.RINX_FIELD)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let lo = self.inset_px + self.cap_px * 0.5
                let len = max(self.rect_size.x - lo * 2.0, 0.0)
                let x = lo + len * self.slide_pos
                let y = self.rect_size.y * 0.5
                sdf.box(lo, y - 2.0, len, 4.0, 2.0)
                sdf.fill(self.track_color)
                sdf.box(lo, y - 2.0, max(0.0, x - lo), 4.0, 2.0)
                sdf.fill(self.fill_color)
                sdf.circle(x, y, self.cap_px * 0.5 - 1.0)
                sdf.fill_keep(self.knob_color)
                sdf.stroke(self.fill_color, 1.0 + self.focus * 0.5)
                return sdf.result
            }
        }
    }

    // The zoom out and zoom in buttons on either side of the slider.
    mod.widgets.AvatarZoomButton = RobrixIconButton {
        width: Fit, height: Fit
        padding: 9
        spacing: 0
        align: Align{x: 0.5, y: 0.5}
        icon_walk: Walk{width: 18, height: 18, margin: 0}
        draw_icon.color: mod.widgets.RINX_MUTED
        draw_bg +: {
            border_size: 0
            color: #0000
            color_hover: #00000015
            color_down: #00000025
            // A click leaves the button with key focus; keep its face clear.
            color_focus: #0000
        }
    }
}

/// The square of source pixels kept for the avatar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvatarCropRect {
    pub x: u32,
    pub y: u32,
    pub side: u32,
    /// The size of the picture the square was chosen on.
    pub source_width: u32,
    pub source_height: u32,
}

/// Where the crop circle sits over a picture.
///
/// Everything is in the picture's own pixels: `zoom` is how far in from the zoom
/// at which the picture's short side spans the circle, and `center` is the
/// source pixel under the circle's centre. The circle always stays covered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropFrame {
    width: f64,
    height: f64,
    zoom: f64,
    center: DVec2,
}

impl CropFrame {
    /// The starting frame: centred, at the smallest zoom. `None` for an empty picture.
    pub fn new(width: usize, height: usize) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let (width, height) = (width as f64, height as f64);
        Some(Self { width, height, zoom: 1.0, center: dvec2(width / 2.0, height / 2.0) })
    }

    fn short_side(&self) -> f64 {
        self.width.min(self.height)
    }

    pub fn source_size(&self) -> DVec2 {
        dvec2(self.width, self.height)
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    /// The largest zoom: [`MAX_ZOOM`], or less for a picture so small that the
    /// circle would cover less than one of its pixels.
    pub fn max_zoom(&self) -> f64 {
        MAX_ZOOM.min(self.short_side()).max(1.0)
    }

    /// The source pixel under the circle's centre.
    pub fn center(&self) -> DVec2 {
        self.center
    }

    /// How many source pixels the circle spans.
    pub fn crop_side(&self) -> f64 {
        self.short_side() / self.zoom
    }

    /// Points per source pixel, for a circle `diameter` points across.
    pub fn scale(&self, diameter: f64) -> f64 {
        diameter / self.crop_side()
    }

    /// Keeps the zoom in range and the circle inside the picture.
    fn clamp(&mut self) {
        self.zoom = if self.zoom.is_finite() { self.zoom.clamp(1.0, self.max_zoom()) } else { 1.0 };
        let half = self.crop_side() / 2.0;
        self.center.x = self.center.x.clamp(half, self.width - half);
        self.center.y = self.center.y.clamp(half, self.height - half);
    }

    /// Zooms about the circle's centre.
    pub fn set_zoom(&mut self, zoom: f64) {
        if zoom.is_finite() {
            self.zoom = zoom;
            self.clamp();
        }
    }

    /// Zooms so that the source pixel under `anchor` stays under it, as far as the
    /// circle can stay covered. `anchor` is measured from the circle's centre, in points.
    pub fn zoom_about(&mut self, zoom: f64, anchor: DVec2, diameter: f64) {
        if !zoom.is_finite() || !(diameter > 0.0) {
            return;
        }
        let under_anchor = self.to_source(anchor, diameter);
        self.zoom = zoom.clamp(1.0, self.max_zoom());
        self.center = under_anchor - anchor / self.scale(diameter);
        self.clamp();
    }

    /// The source pixel under `point`, measured from the circle's centre in points.
    pub fn to_source(&self, point: DVec2, diameter: f64) -> DVec2 {
        self.center + point / self.scale(diameter)
    }

    /// Moves the picture with the pointer: `center_at_press` is where the drag
    /// started, `moved` how far the pointer has gone since, in points.
    pub fn drag(&mut self, center_at_press: DVec2, moved: DVec2, diameter: f64) {
        if diameter > 0.0 {
            self.center = center_at_press - moved / self.scale(diameter);
            self.clamp();
        }
    }

    /// The zoom slider's position: 0 at the smallest zoom, 1 at the largest,
    /// with even steps of the zoom factor between them.
    pub fn zoom_fraction(&self) -> f64 {
        let max = self.max_zoom();
        if max <= 1.0 { 0.0 } else { (self.zoom.ln() / max.ln()).clamp(0.0, 1.0) }
    }

    pub fn set_zoom_fraction(&mut self, fraction: f64) {
        if fraction.is_finite() {
            self.set_zoom(self.max_zoom().powf(fraction.clamp(0.0, 1.0)));
        }
    }

    /// The whole picture's rect, in points, for a circle `diameter` points across
    /// centred on `circle_center`.
    pub fn image_rect(&self, circle_center: DVec2, diameter: f64) -> Rect {
        let scale = self.scale(diameter);
        Rect { pos: circle_center - self.center * scale, size: self.source_size() * scale }
    }

    /// The square the circle covers, in whole source pixels inside the picture.
    pub fn crop_rect(&self) -> AvatarCropRect {
        let (width, height) = (self.width as u32, self.height as u32);
        let side = (self.crop_side().round() as u32).clamp(1, width.min(height));
        let start = |center: f64, len: u32| ((center - side as f64 / 2.0).round().max(0.0) as u32).min(len - side);
        AvatarCropRect {
            x: start(self.center.x, width),
            y: start(self.center.y, height),
            side,
            source_width: width,
            source_height: height,
        }
    }
}

/// The largest square centred in `rect`.
fn square_in(rect: Rect) -> Rect {
    let side = rect.size.x.min(rect.size.y).max(0.0);
    Rect { pos: rect.pos + (rect.size - dvec2(side, side)) * 0.5, size: dvec2(side, side) }
}

/// The circle's diameter in a square `side` points across, `inset` points in from its edges.
fn circle_diameter(side: f64, inset: f64) -> f64 {
    (side - 2.0 * inset.max(0.0)).max(1.0)
}

/// The part of the picture `image` that shows inside `square`, and the share of
/// the texture that part shows: its rect, then the texture scale and pan for it.
fn visible_part(image: Rect, square: Rect) -> Option<(Rect, DVec2, DVec2)> {
    let min = dvec2(image.pos.x.max(square.pos.x), image.pos.y.max(square.pos.y));
    let max = dvec2(
        (image.pos.x + image.size.x).min(square.pos.x + square.size.x),
        (image.pos.y + image.size.y).min(square.pos.y + square.size.y),
    );
    if !(max.x > min.x && max.y > min.y) {
        return None;
    }
    let visible = Rect { pos: min, size: max - min };
    let scale = visible.size / image.size;
    let pan = (visible.pos - image.pos) / image.size;
    Some((visible, scale, pan))
}

/// A picked picture made ready for the crop view, off the UI thread: its first
/// frame, shrunk to a size the view can draw, and the size of the picture it
/// came from.
#[derive(Clone, Debug)]
pub struct AvatarCropPreview {
    buffer: ImageBuffer,
    source_width: usize,
    source_height: usize,
}

impl AvatarCropPreview {
    /// `None` for an empty picture.
    pub fn new(decoded: ImageBuffer) -> Option<Self> {
        let frame = first_frame(decoded);
        let (source_width, source_height) = (frame.width, frame.height);
        if source_width == 0 || source_height == 0 || frame.data.len() < source_width * source_height {
            return None;
        }
        let (width, height) = preview_size(source_width, source_height);
        let buffer = if (width, height) == (source_width, source_height) {
            frame
        } else {
            frame.area_downscaled(width, height)
        };
        Some(Self { buffer, source_width, source_height })
    }

    /// The size of the picture the preview was made from, which crops are measured in.
    pub fn source_size(&self) -> (usize, usize) {
        (self.source_width, self.source_height)
    }

    /// The size of the texture the view draws.
    pub fn preview_size(&self) -> (usize, usize) {
        (self.buffer.width, self.buffer.height)
    }

    /// The texture for the view, with mipmaps so a zoomed-out picture stays smooth.
    pub fn into_texture(self, cx: &mut Cx) -> Texture {
        self.buffer.into_new_mip_texture(cx)
    }
}

/// The size of the crop view's texture for a picture of this size: never bigger than the picture.
fn preview_size(width: usize, height: usize) -> (usize, usize) {
    let short = width.min(height).max(1) as f64;
    let long = width.max(height).max(1) as f64;
    let scale = (PREVIEW_MAX_SHORT_SIDE as f64 / short).min(PREVIEW_MAX_LONG_SIDE as f64 / long).min(1.0);
    (
        ((width as f64 * scale).round() as usize).clamp(1, width.max(1)),
        ((height as f64 * scale).round() as usize).clamp(1, height.max(1)),
    )
}

/// The first frame of a decoded picture: an animation is decoded into a grid of
/// its frames, the first at the top left. A still picture is returned as it is.
fn first_frame(mut buffer: ImageBuffer) -> ImageBuffer {
    let Some(animation) = buffer.animation.take() else { return buffer };
    let stride = buffer.width;
    let width = animation.width.min(stride);
    let height = animation.height.min(buffer.height);
    let mut frame = ImageBuffer::default();
    if stride > 0 && buffer.data.len() >= stride * height {
        frame.data = buffer.data.chunks_exact(stride).take(height)
            .flat_map(|row| row[..width].iter().copied())
            .collect();
        frame.width = width;
        frame.height = height;
    }
    frame
}

/// What the person chose for their new avatar's framing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvatarCrop {
    /// The square chosen in the crop view.
    Chosen(AvatarCropRect),
    /// The crop view's starting square (centred, at the smallest zoom), for a
    /// picture confirmed before its preview was ready.
    Centered,
}

/// Why a picture couldn't be cropped.
#[derive(Debug, PartialEq, Eq)]
pub enum AvatarCropError {
    /// The picture couldn't be decoded.
    Decode(String),
    /// The picture isn't the size it was when its crop was chosen.
    Changed,
    /// The crop doesn't fit inside the picture.
    OutOfBounds,
    /// The cropped picture couldn't be encoded.
    Encode(String),
}

/// A cropped avatar, encoded and ready to upload.
#[derive(Debug)]
pub struct EncodedAvatar {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub extension: &'static str,
    /// The width and height of the encoded square, in pixels.
    pub side: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceFormat { Jpeg, Png, Other }

fn source_format(data: &[u8]) -> SourceFormat {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        SourceFormat::Jpeg
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        SourceFormat::Png
    } else {
        SourceFormat::Other
    }
}

/// Cuts the avatar's square out of the encoded picture `data` at its full size,
/// scales it down to at most [`AVATAR_MAX_SIDE`] pixels (Lanczos3), and encodes
/// it: a JPEG at quality 90 when the picture was a JPEG, otherwise a PNG.
///
/// The picture is decoded the way the crop view's preview was (upright, its first
/// frame), so the square is the one the person saw. Blocking: run it off the UI thread.
pub fn crop_and_encode_avatar(data: &[u8], crop: AvatarCrop) -> Result<EncodedAvatar, AvatarCropError> {
    let decoded = decode_image_from_data(data).map_err(|e| AvatarCropError::Decode(format!("{e:?}")))?;
    let frame = first_frame(decoded);
    let rect = match crop {
        AvatarCrop::Chosen(rect) => {
            if (rect.source_width as usize, rect.source_height as usize) != (frame.width, frame.height) {
                return Err(AvatarCropError::Changed);
            }
            rect
        }
        AvatarCrop::Centered => CropFrame::new(frame.width, frame.height)
            .ok_or_else(|| AvatarCropError::Decode("the picture is empty".into()))?
            .crop_rect(),
    };
    let fits = |start: u32, len: usize| start as usize + rect.side as usize <= len;
    if rect.side == 0 || !fits(rect.x, frame.width) || !fits(rect.y, frame.height)
        || frame.data.len() < frame.width * frame.height
    {
        return Err(AvatarCropError::OutOfBounds);
    }

    let mut square = cut_square(&frame, rect);
    drop(frame);
    let has_alpha = square.pixels().any(|pixel| pixel[3] < u8::MAX);
    let side = rect.side.min(AVATAR_MAX_SIDE);
    if side < rect.side {
        // The filters assume premultiplied alpha; without it the colour of
        // transparent pixels bleeds into the edges around them.
        if has_alpha { premultiply(&mut square); }
        // A big crop is first averaged down to twice the size, which Lanczos3
        // then takes the rest of the way at a fraction of the cost.
        if rect.side > 2 * side {
            square = ::image::imageops::thumbnail(&square, 2 * side, 2 * side);
        }
        square = ::image::imageops::resize(&square, side, side, ::image::imageops::FilterType::Lanczos3);
        if has_alpha { unpremultiply(&mut square); }
    }

    let format = source_format(data);
    let icc_profile = rgb_icc_profile(data, format);
    let encoded = match format {
        SourceFormat::Jpeg => encode_jpeg(square, icc_profile).map(|bytes| (bytes, "image/jpeg", "jpg")),
        _ => encode_png(square, has_alpha, icc_profile).map(|bytes| (bytes, "image/png", "png")),
    };
    let (bytes, mime_type, extension) = encoded.map_err(|e| AvatarCropError::Encode(e.to_string()))?;
    Ok(EncodedAvatar { bytes, mime_type, extension, side })
}

/// The square `rect` of `frame`, as RGBA pixels.
fn cut_square(frame: &ImageBuffer, rect: AvatarCropRect) -> ::image::RgbaImage {
    let (x, y, side) = (rect.x as usize, rect.y as usize, rect.side as usize);
    let mut pixels = Vec::with_capacity(side * side * 4);
    for row in frame.data.chunks_exact(frame.width).skip(y).take(side) {
        // Decoded pixels are 0xAARRGGBB.
        for &pixel in &row[x..x + side] {
            pixels.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8, (pixel >> 24) as u8]);
        }
    }
    ::image::RgbaImage::from_raw(rect.side, rect.side, pixels).expect("BUG: a square has side * side pixels")
}

fn premultiply(image: &mut ::image::RgbaImage) {
    for pixel in image.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

fn unpremultiply(image: &mut ::image::RgbaImage) {
    for pixel in image.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = match alpha {
                0 => 0,
                _ => ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8,
            };
        }
    }
}

/// The picture's colour profile, when it describes RGB colours. The decoded
/// pixels are still in the picture's own colours, so the cropped avatar needs the
/// same profile to look the same (e.g., a phone photo in Display P3). A profile
/// for other colours (CMYK, grey) wouldn't describe the RGB pixels written out.
fn rgb_icc_profile(data: &[u8], format: SourceFormat) -> Option<Vec<u8>> {
    use ::image::ImageDecoder;
    let input = std::io::Cursor::new(data);
    let profile = match format {
        SourceFormat::Jpeg => ::image::codecs::jpeg::JpegDecoder::new(input).ok()?.icc_profile().ok()??,
        SourceFormat::Png => ::image::codecs::png::PngDecoder::new(input).ok()?.icc_profile().ok()??,
        SourceFormat::Other => return None,
    };
    // An ICC profile's header names its colour space at bytes 16..20.
    (profile.get(16..20) == Some(b"RGB ".as_slice())).then_some(profile)
}

fn encode_jpeg(square: ::image::RgbaImage, icc_profile: Option<Vec<u8>>) -> ::image::ImageResult<Vec<u8>> {
    use ::image::ImageEncoder;
    let rgb = ::image::DynamicImage::ImageRgba8(square).into_rgb8();
    let mut bytes = Vec::new();
    let mut encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY);
    if let Some(profile) = icc_profile {
        let _ = encoder.set_icc_profile(profile);
    }
    encoder.write_image(rgb.as_raw(), rgb.width(), rgb.height(), ::image::ExtendedColorType::Rgb8)?;
    Ok(bytes)
}

fn encode_png(square: ::image::RgbaImage, keep_alpha: bool, icc_profile: Option<Vec<u8>>) -> ::image::ImageResult<Vec<u8>> {
    use ::image::ImageEncoder;
    let mut bytes = Vec::new();
    let mut encoder = ::image::codecs::png::PngEncoder::new(&mut bytes);
    if let Some(profile) = icc_profile {
        let _ = encoder.set_icc_profile(profile);
    }
    if keep_alpha {
        encoder.write_image(square.as_raw(), square.width(), square.height(), ::image::ExtendedColorType::Rgba8)?;
    } else {
        let rgb = ::image::DynamicImage::ImageRgba8(square).into_rgb8();
        encoder.write_image(rgb.as_raw(), rgb.width(), rgb.height(), ::image::ExtendedColorType::Rgb8)?;
    }
    Ok(bytes)
}

/// Turns the picked file into the file to upload as the avatar: the chosen square,
/// scaled and encoded by [`crop_and_encode_avatar`], in a temporary file of its own
/// that's deleted once the upload is done with it.
///
/// A picture confirmed before its preview was ready that can't be decoded here
/// either (one the crop view could never have shown) is uploaded as it was picked.
///
/// Blocking: reads, decodes and encodes a full-size picture. Run it off the UI thread.
pub fn prepare_avatar_upload(file_data: FileUploadMetadata, crop: AvatarCrop) -> Result<FileUploadMetadata, String> {
    let name = file_data.file_name();
    let data = std::fs::read(file_data.path()).map_err(|e| crate::i18n::format(
        "Couldn't read \"{name}\": {error}",
        &[("name", name.clone()), ("error", e.to_string())],
    ))?;
    let encoded = match crop_and_encode_avatar(&data, crop) {
        Ok(encoded) => encoded,
        Err(AvatarCropError::Decode(_)) if crop == AvatarCrop::Centered => return Ok(file_data),
        Err(AvatarCropError::Changed) => return Err(crate::i18n::format(
            "\"{name}\" changed after it was opened. Please choose it again.",
            &[("name", name)],
        )),
        Err(e) => {
            warning!("Failed to crop the new avatar image {name}: {e:?}");
            return Err(crate::i18n::format(
                "Couldn't crop \"{name}\". Please choose a different image.",
                &[("name", name)],
            ));
        }
    };
    let stem = std::path::Path::new(&name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("avatar");
    let source = robius_file_picker::LocalFile::from_bytes(
        &format!("{stem}.{}", encoded.extension),
        Some(encoded.mime_type),
        &encoded.bytes,
    ).map_err(|e| crate::i18n::format(
        "Couldn't save the cropped avatar: {error}",
        &[("error", e.to_string())],
    ))?;
    Ok(FileUploadMetadata {
        source,
        caption: None,
        mime_type: encoded.mime_type.to_string(),
        preview: FilePreview::None,
        size: encoded.bytes.len() as u64,
    })
}

/// The ring and the darkened corners drawn over the picture.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawAvatarCropMask {
    #[deref] draw_super: DrawQuad,
    /// The circle's radius in points. Its centre is the quad's.
    #[live] radius: f32,
}

/// Actions emitted by an [`AvatarCropView`].
#[derive(Clone, Debug, Default)]
pub enum AvatarCropViewAction {
    /// The person zoomed with the wheel, a pinch or a double click:
    /// the zoom's new slider position (see [`CropFrame::zoom_fraction`]).
    Zoomed(f64),
    #[default]
    None,
}

#[derive(Clone, Copy, Debug)]
struct CropDrag {
    digit: DigitId,
    center_at_press: DVec2,
}

/// A square showing a picture under a circular mask, which the person drags
/// and zooms to choose the part kept for their avatar.
#[derive(Script, ScriptHook, Widget)]
pub struct AvatarCropView {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    /// The ground under the picture, where the picture doesn't reach.
    #[redraw] #[live] draw_bg: DrawColor,
    #[live] draw_image: DrawImage,
    #[live] draw_mask: DrawAvatarCropMask,
    /// Points between the square's edges and the circle.
    #[live(12.0)] circle_inset: f64,

    #[rust] texture: Option<Texture>,
    #[rust] frame: Option<CropFrame>,
    #[rust] drag: Option<CropDrag>,
}

impl Widget for AvatarCropView {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let Some(mut frame) = self.frame else { return };
        let area = self.draw_bg.area();
        // Where the square is on screen: the layout may have moved it after it was drawn, to align it.
        let square = area.rect(cx);
        let diameter = circle_diameter(square.size.x, self.circle_inset);
        match event.hits(cx, area) {
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                cx.set_cursor(if self.drag.is_some() { MouseCursor::Grabbing } else { MouseCursor::Grab });
            }
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                if fe.tap_count == 2 {
                    // A double click starts over: centred, at the smallest zoom.
                    if let Some(start) = CropFrame::new(frame.width as usize, frame.height as usize) {
                        self.set_frame(cx, start, true);
                    }
                }
                cx.claim_finger_drag(fe.digit_id);
                self.drag = Some(CropDrag { digit: fe.digit_id, center_at_press: self.frame.unwrap_or(frame).center() });
                cx.set_cursor(MouseCursor::Grabbing);
            }
            Hit::FingerMove(fe) => {
                if let Some(drag) = self.drag.filter(|drag| drag.digit == fe.digit_id) {
                    frame.drag(drag.center_at_press, fe.abs - fe.abs_start, diameter);
                    self.set_frame(cx, frame, false);
                }
            }
            Hit::FingerUp(fe) => {
                if self.drag.is_some_and(|drag| drag.digit == fe.digit_id) {
                    self.drag = None;
                    if fe.is_over && fe.device.has_hovers() {
                        cx.set_cursor(MouseCursor::Grab);
                    }
                }
            }
            Hit::FingerScroll(fs) => {
                // Zooming takes the whole gesture, so the dialog under the picture stays put.
                let is_owner = match event {
                    Event::Scroll(se) => {
                        let is_owner_x = cx.is_scroll_owner(se, area, Vec2Index::X, true);
                        cx.is_scroll_owner(se, area, Vec2Index::Y, true) || is_owner_x
                    }
                    _ => false,
                };
                if is_owner {
                    let zoom = frame.zoom() * 2f64.powf(-fs.scroll.y / SCROLL_POINTS_PER_DOUBLING);
                    frame.zoom_about(zoom, fs.abs - square.center(), diameter);
                    self.set_frame(cx, frame, true);
                    event.set_scroll_handled(Vec2Index::X);
                    event.set_scroll_handled(Vec2Index::Y);
                }
            }
            Hit::FingerPinch(pe) => {
                frame.zoom_about(frame.zoom() * pe.scale, pe.abs - square.center(), diameter);
                self.set_frame(cx, frame, true);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let square = square_in(cx.walk_turtle(walk));
        self.draw_bg.draw_abs(cx, square);
        if let (Some(texture), Some(frame)) = (&self.texture, &self.frame) {
            let diameter = circle_diameter(square.size.x, self.circle_inset);
            let image = frame.image_rect(square.center(), diameter);
            // Only the part of the picture inside the square is drawn.
            if let Some((visible, scale, pan)) = visible_part(image, square) {
                self.draw_image.draw_vars.set_texture(0, texture);
                self.draw_image.image_scale = vec2(scale.x as f32, scale.y as f32);
                self.draw_image.image_pan = vec2(pan.x as f32, pan.y as f32);
                self.draw_image.draw_abs(cx, visible);
            }
            self.draw_mask.radius = (diameter * 0.5) as f32;
            self.draw_mask.draw_abs(cx, square);
        }
        DrawStep::done()
    }
}

impl AvatarCropView {
    /// Shows `frame`, and tells the dialog the zoom it's at when `report_zoom`
    /// (a zoom the person made here, rather than one the dialog asked for).
    fn set_frame(&mut self, cx: &mut Cx, frame: CropFrame, report_zoom: bool) {
        let old = self.frame.replace(frame);
        if old == Some(frame) {
            return;
        }
        if report_zoom && old.is_none_or(|old| old.zoom() != frame.zoom()) {
            cx.widget_action(self.uid, AvatarCropViewAction::Zoomed(frame.zoom_fraction()));
        }
        self.redraw(cx);
    }

    /// Shows a new picture, centred at the smallest zoom: `width` by `height`
    /// is the size of the source picture, whatever the texture's own size.
    pub fn set_image(&mut self, cx: &mut Cx, texture: Texture, width: usize, height: usize) {
        self.clear(cx);
        self.frame = CropFrame::new(width, height);
        if self.frame.is_some() {
            self.texture = Some(texture);
        } else {
            texture.release(cx);
        }
        self.redraw(cx);
    }

    /// Drops the picture and frees its texture.
    pub fn clear(&mut self, cx: &mut Cx) {
        if let Some(texture) = self.texture.take() {
            texture.release(cx);
        }
        self.frame = None;
        self.drag = None;
        self.redraw(cx);
    }

    pub fn frame(&self) -> Option<CropFrame> {
        self.frame
    }

    pub fn crop_rect(&self) -> Option<AvatarCropRect> {
        self.frame.map(|frame| frame.crop_rect())
    }
}

impl AvatarCropViewRef {
    /// See [`AvatarCropView::set_image`].
    pub fn set_image(&self, cx: &mut Cx, texture: Texture, width: usize, height: usize) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_image(cx, texture, width, height);
        }
    }

    /// See [`AvatarCropView::clear`].
    pub fn clear(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.clear(cx);
        }
    }

    pub fn frame(&self) -> Option<CropFrame> {
        self.borrow().and_then(|inner| inner.frame())
    }

    /// The square chosen so far, or `None` if no picture is shown.
    pub fn crop_rect(&self) -> Option<AvatarCropRect> {
        self.borrow().and_then(|inner| inner.crop_rect())
    }

    /// Zooms about the circle's centre to the slider position `fraction`.
    pub fn set_zoom_fraction(&self, cx: &mut Cx, fraction: f64) {
        let Some(mut inner) = self.borrow_mut() else { return };
        let Some(mut frame) = inner.frame else { return };
        frame.set_zoom_fraction(fraction);
        inner.set_frame(cx, frame, false);
    }

    /// Zooms about the circle's centre by `factor`, and returns the zoom's new slider position.
    pub fn zoom_by(&self, cx: &mut Cx, factor: f64) -> Option<f64> {
        let mut inner = self.borrow_mut()?;
        let mut frame = inner.frame?;
        frame.set_zoom(frame.zoom() * factor);
        inner.set_frame(cx, frame, false);
        Some(frame.zoom_fraction())
    }

    /// The zoom's new slider position, if the person zoomed in the view.
    pub fn zoomed(&self, actions: &Actions) -> Option<f64> {
        match actions.find_widget_action(self.widget_uid())?.cast::<AvatarCropViewAction>() {
            AvatarCropViewAction::Zoomed(fraction) => Some(fraction),
            AvatarCropViewAction::None => None,
        }
    }
}

#[cfg(test)]
mod tests;
