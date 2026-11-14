use super::*;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn close_v(a: DVec2, b: DVec2) -> bool {
    close(a.x, b.x) && close(a.y, b.y)
}

fn rect(x: u32, y: u32, side: u32, source_width: u32, source_height: u32) -> AvatarCropRect {
    AvatarCropRect { x, y, side, source_width, source_height }
}

/// A PNG of `width` x `height` with a different colour in each quarter:
/// red top left, green top right, blue bottom left, white bottom right.
fn quarters_png(width: u32, height: u32) -> Vec<u8> {
    let image = ::image::RgbImage::from_fn(width, height, |x, y| {
        match (x < width / 2, y < height / 2) {
            (true, true) => ::image::Rgb([255, 0, 0]),
            (false, true) => ::image::Rgb([0, 255, 0]),
            (true, false) => ::image::Rgb([0, 0, 255]),
            (false, false) => ::image::Rgb([255, 255, 255]),
        }
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, ::image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn encode_jpeg_with(image: &::image::RgbImage, icc_profile: Option<Vec<u8>>) -> Vec<u8> {
    use ::image::ImageEncoder;
    let mut bytes = Vec::new();
    let mut encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95);
    if let Some(profile) = icc_profile {
        encoder.set_icc_profile(profile).unwrap();
    }
    encoder.write_image(image.as_raw(), image.width(), image.height(), ::image::ExtendedColorType::Rgb8).unwrap();
    bytes
}

fn encode_png_with(image: &::image::RgbImage, icc_profile: Option<Vec<u8>>) -> Vec<u8> {
    use ::image::ImageEncoder;
    let mut bytes = Vec::new();
    let mut encoder = ::image::codecs::png::PngEncoder::new(&mut bytes);
    if let Some(profile) = icc_profile {
        encoder.set_icc_profile(profile).unwrap();
    }
    encoder.write_image(image.as_raw(), image.width(), image.height(), ::image::ExtendedColorType::Rgb8).unwrap();
    bytes
}

/// A made-up ICC profile: only its header's colour space field (bytes 16..20) matters here.
fn icc_profile(color_space: &[u8; 4]) -> Vec<u8> {
    let mut profile: Vec<u8> = (0..200u32).map(|i| (i * 7 % 251) as u8).collect();
    profile[16..20].copy_from_slice(color_space);
    profile
}

fn jpeg_icc_profile(data: &[u8]) -> Option<Vec<u8>> {
    use ::image::ImageDecoder;
    ::image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(data)).unwrap().icc_profile().unwrap()
}

fn png_icc_profile(data: &[u8]) -> Option<Vec<u8>> {
    use ::image::ImageDecoder;
    ::image::codecs::png::PngDecoder::new(std::io::Cursor::new(data)).unwrap().icc_profile().unwrap()
}

fn assert_color(image: &::image::RgbaImage, x: u32, y: u32, expected: [u8; 4], tolerance: u8) {
    let actual = image.get_pixel(x, y).0;
    for (a, e) in actual.iter().zip(expected) {
        assert!(a.abs_diff(e) <= tolerance, "pixel ({x}, {y}) is {actual:?}, expected {expected:?}");
    }
}

#[test]
fn the_crop_view_s_shaders_compile() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, { mod.widgets.AvatarCropView {} });
        let view = WidgetRef::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty());
        let view = view.borrow::<AvatarCropView>().unwrap();
        assert!(view.draw_bg.draw_vars.can_instance());
        assert!(view.draw_image.draw_vars.can_instance());
        assert!(view.draw_mask.draw_vars.can_instance());
        assert_eq!(view.circle_inset, 12.0);
    });
}

#[test]
fn a_new_frame_is_centred_and_the_short_side_spans_the_circle() {
    let frame = CropFrame::new(800, 600).unwrap();
    assert_eq!(frame.zoom(), 1.0);
    assert!(close_v(frame.center(), dvec2(400.0, 300.0)));
    assert_eq!(frame.crop_side(), 600.0);
    assert_eq!(frame.crop_rect(), rect(100, 0, 600, 800, 600));

    let portrait = CropFrame::new(600, 800).unwrap();
    assert_eq!(portrait.crop_rect(), rect(0, 100, 600, 600, 800));

    let square = CropFrame::new(512, 512).unwrap();
    assert_eq!(square.crop_rect(), rect(0, 0, 512, 512, 512));

    assert!(CropFrame::new(0, 10).is_none());
    assert!(CropFrame::new(10, 0).is_none());
}

#[test]
fn the_zoom_stays_between_covering_the_circle_and_the_largest_zoom() {
    let mut frame = CropFrame::new(800, 600).unwrap();
    frame.set_zoom(0.5);
    assert_eq!(frame.zoom(), 1.0);
    frame.set_zoom(10.0);
    assert_eq!(frame.zoom(), MAX_ZOOM);
    assert_eq!(frame.crop_rect(), rect(325, 225, 150, 800, 600));
    frame.set_zoom(f64::NAN);
    assert_eq!(frame.zoom(), MAX_ZOOM);
    frame.set_zoom(f64::INFINITY);
    assert_eq!(frame.zoom(), MAX_ZOOM);

    // A picture too small to zoom as far stops where the circle spans one pixel.
    let mut tiny = CropFrame::new(3, 2).unwrap();
    assert_eq!(tiny.max_zoom(), 2.0);
    tiny.set_zoom(4.0);
    assert_eq!(tiny.zoom(), 2.0);
    assert_eq!(tiny.crop_rect().side, 1);
    let mut one = CropFrame::new(1, 1).unwrap();
    one.set_zoom(4.0);
    assert_eq!(one.zoom(), 1.0);
    assert_eq!(one.crop_rect(), rect(0, 0, 1, 1, 1));
    assert_eq!(one.zoom_fraction(), 0.0);
}

#[test]
fn a_drag_moves_the_picture_with_the_pointer_but_never_off_the_circle() {
    // A 300-point circle over 600 pixels: half a point a pixel.
    let mut frame = CropFrame::new(800, 600).unwrap();
    let start = frame.center();
    frame.drag(start, dvec2(10.0, 0.0), 300.0);
    // Moving the picture right shows more of its left side.
    assert!(close_v(frame.center(), dvec2(380.0, 300.0)));
    frame.drag(start, dvec2(-10.0, 25.0), 300.0);
    // At the smallest zoom the short side already fills the circle, so it can't move that way.
    assert!(close_v(frame.center(), dvec2(420.0, 300.0)));
    frame.drag(start, dvec2(500.0, 0.0), 300.0);
    assert_eq!(frame.crop_rect(), rect(0, 0, 600, 800, 600));
    frame.drag(start, dvec2(-500.0, 0.0), 300.0);
    assert_eq!(frame.crop_rect(), rect(200, 0, 600, 800, 600));

    // Zoomed in, it moves both ways, as far as the picture's edges.
    frame.set_zoom(2.0);
    let start = frame.center();
    frame.drag(start, dvec2(0.0, -30.0), 300.0);
    assert!(close(frame.center().y, 330.0));
    frame.drag(start, dvec2(1000.0, 1000.0), 300.0);
    assert_eq!(frame.crop_rect(), rect(0, 0, 300, 800, 600));

    // A view that hasn't been drawn yet has no size to drag by.
    let before = frame;
    frame.drag(start, dvec2(10.0, 10.0), 0.0);
    assert_eq!(frame, before);
}

#[test]
fn zooming_about_a_point_keeps_the_pixel_under_it() {
    let mut frame = CropFrame::new(800, 600).unwrap();
    let anchor = dvec2(50.0, -30.0);
    let under_anchor = frame.to_source(anchor, 300.0);
    assert!(close_v(under_anchor, dvec2(500.0, 240.0)));
    frame.zoom_about(2.0, anchor, 300.0);
    assert_eq!(frame.zoom(), 2.0);
    assert!(close_v(frame.to_source(anchor, 300.0), under_anchor));
    assert!(close_v(frame.center(), dvec2(450.0, 270.0)));

    // Near the picture's edge the circle stays covered instead.
    let mut frame = CropFrame::new(800, 600).unwrap();
    frame.zoom_about(4.0, dvec2(-150.0, -150.0), 300.0);
    assert_eq!(frame.zoom(), 4.0);
    assert_eq!(frame.crop_rect(), rect(100, 0, 150, 800, 600));

    // Zooming out past the smallest zoom stops there, centred again on the short side.
    frame.zoom_about(0.25, dvec2(100.0, 100.0), 300.0);
    assert_eq!(frame.zoom(), 1.0);
    assert!(close(frame.center().y, 300.0));
}

#[test]
fn the_slider_position_moves_in_even_steps_of_the_zoom_factor() {
    let mut frame = CropFrame::new(800, 600).unwrap();
    assert_eq!(frame.zoom_fraction(), 0.0);
    frame.set_zoom_fraction(0.5);
    assert!(close(frame.zoom(), 2.0));
    assert!(close(frame.zoom_fraction(), 0.5));
    frame.set_zoom_fraction(1.0);
    assert!(close(frame.zoom(), MAX_ZOOM));
    frame.set_zoom_fraction(2.0);
    assert!(close(frame.zoom(), MAX_ZOOM));
    frame.set_zoom_fraction(-1.0);
    assert_eq!(frame.zoom(), 1.0);
    // Six presses of a zoom button go all the way.
    for _ in 0..6 {
        frame.set_zoom(frame.zoom() * ZOOM_BUTTON_STEP);
    }
    assert!(close(frame.zoom(), MAX_ZOOM));
}

#[test]
fn the_picture_is_drawn_around_the_circle_and_only_inside_the_square() {
    let frame = CropFrame::new(800, 600).unwrap();
    // 0.5 points a pixel, around a circle centred at (200, 200).
    let image = frame.image_rect(dvec2(200.0, 200.0), 300.0);
    assert_eq!(image, Rect { pos: dvec2(0.0, 50.0), size: dvec2(400.0, 300.0) });

    let square = Rect { pos: dvec2(50.0, 50.0), size: dvec2(300.0, 300.0) };
    let (visible, scale, pan) = visible_part(image, square).unwrap();
    assert_eq!(visible, square);
    assert!(close_v(scale, dvec2(0.75, 1.0)));
    assert!(close_v(pan, dvec2(0.125, 0.0)));

    // A picture entirely outside the square draws nothing.
    let away = Rect { pos: dvec2(400.0, 0.0), size: dvec2(10.0, 10.0) };
    assert!(visible_part(away, square).is_none());

    assert_eq!(
        square_in(Rect { pos: dvec2(0.0, 0.0), size: dvec2(300.0, 200.0) }),
        Rect { pos: dvec2(50.0, 0.0), size: dvec2(200.0, 200.0) },
    );
    assert_eq!(circle_diameter(256.0, 12.0), 232.0);
    assert_eq!(circle_diameter(10.0, 12.0), 1.0);
}

#[test]
fn the_crop_square_is_whole_pixels_inside_the_picture() {
    let mut frame = CropFrame::new(1001, 1000).unwrap();
    frame.set_zoom(3.0);
    let crop = frame.crop_rect();
    assert_eq!(crop.side, 333);
    assert!(crop.x + crop.side <= 1001 && crop.y + crop.side <= 1000);
    frame.drag(frame.center(), dvec2(-10_000.0, -10_000.0), 232.0);
    assert_eq!(frame.crop_rect(), rect(668, 667, 333, 1001, 1000));
}

#[test]
fn the_preview_is_shrunk_to_what_the_view_can_show() {
    assert_eq!(preview_size(4000, 3000), (2048, 1536));
    assert_eq!(preview_size(3000, 4000), (1536, 2048));
    assert_eq!(preview_size(10_000, 1000), (3072, 307));
    assert_eq!(preview_size(100, 50), (100, 50));
    assert_eq!(preview_size(1, 1), (1, 1));

    let mut buffer = ImageBuffer::default();
    buffer.width = 4000;
    buffer.height = 100;
    buffer.data = vec![0xff336699; 4000 * 100];
    let preview = AvatarCropPreview::new(buffer).unwrap();
    assert_eq!(preview.source_size(), (4000, 100));
    assert_eq!(preview.preview_size(), (3072, 77));

    assert!(AvatarCropPreview::new(ImageBuffer::default()).is_none());
}

#[test]
fn an_animation_is_cropped_from_its_first_frame() {
    // Two 2x2 frames side by side, as an animation is decoded.
    let mut buffer = ImageBuffer::default();
    buffer.width = 4;
    buffer.height = 2;
    buffer.data = vec![
        1, 2, 10, 20,
        3, 4, 30, 40,
    ];
    buffer.animation = Some(TextureAnimation {
        width: 2,
        height: 2,
        num_frames: 2,
        frame_delays: vec![0.1, 0.1],
    });
    let frame = first_frame(buffer);
    assert_eq!((frame.width, frame.height), (2, 2));
    assert_eq!(frame.data, vec![1, 2, 3, 4]);
    assert!(frame.animation.is_none());

    let preview = {
        let mut buffer = ImageBuffer::default();
        buffer.width = 6;
        buffer.height = 3;
        buffer.data = vec![0; 18];
        buffer.animation = Some(TextureAnimation {
            width: 3,
            height: 3,
            num_frames: 2,
            frame_delays: vec![0.1, 0.1],
        });
        AvatarCropPreview::new(buffer).unwrap()
    };
    assert_eq!(preview.source_size(), (3, 3));
}

#[test]
fn a_big_crop_is_scaled_down_to_the_avatar_size() {
    let png = quarters_png(800, 600);
    let avatar = crop_and_encode_avatar(&png, AvatarCrop::Chosen(rect(100, 0, 600, 800, 600))).unwrap();
    assert_eq!((avatar.mime_type, avatar.extension, avatar.side), ("image/png", "png", AVATAR_MAX_SIDE));
    let decoded = ::image::load_from_memory(&avatar.bytes).unwrap();
    // An opaque picture is saved without an alpha channel.
    assert_eq!(decoded.color(), ::image::ColorType::Rgb8);
    let decoded = decoded.to_rgba8();
    assert_eq!(decoded.dimensions(), (512, 512));
    assert_color(&decoded, 40, 40, [255, 0, 0, 255], 1);
    assert_color(&decoded, 470, 40, [0, 255, 0, 255], 1);
    assert_color(&decoded, 40, 470, [0, 0, 255, 255], 1);
    assert_color(&decoded, 470, 470, [255, 255, 255, 255], 1);

    // The square is the one chosen: the right part of the picture.
    let right = crop_and_encode_avatar(&png, AvatarCrop::Chosen(rect(500, 0, 300, 800, 600))).unwrap();
    let right = ::image::load_from_memory(&right.bytes).unwrap().to_rgba8();
    assert_eq!(right.dimensions(), (300, 300));
    assert_color(&right, 150, 150, [0, 255, 0, 255], 0);
}

#[test]
fn a_small_crop_keeps_its_size_and_a_centred_crop_is_the_starting_square() {
    let png = quarters_png(100, 80);
    let avatar = crop_and_encode_avatar(&png, AvatarCrop::Centered).unwrap();
    assert_eq!(avatar.side, 80);
    let decoded = ::image::load_from_memory(&avatar.bytes).unwrap().to_rgba8();
    assert_eq!(decoded.dimensions(), (80, 80));
    // Cut from x = 10, so the quarters meet at x = 40, y = 40.
    assert_color(&decoded, 39, 39, [255, 0, 0, 255], 0);
    assert_color(&decoded, 40, 39, [0, 255, 0, 255], 0);
    assert_color(&decoded, 39, 40, [0, 0, 255, 255], 0);
    assert_color(&decoded, 40, 40, [255, 255, 255, 255], 0);
}

#[test]
fn a_photo_stays_a_jpeg_with_its_colour_profile() {
    let photo = ::image::RgbImage::from_fn(900, 600, |x, _| {
        if x < 450 { ::image::Rgb([200, 40, 40]) } else { ::image::Rgb([40, 40, 200]) }
    });
    let profile = icc_profile(b"RGB ");
    let jpeg = encode_jpeg_with(&photo, Some(profile.clone()));
    let avatar = crop_and_encode_avatar(&jpeg, AvatarCrop::Chosen(rect(150, 0, 600, 900, 600))).unwrap();
    assert_eq!((avatar.mime_type, avatar.extension, avatar.side), ("image/jpeg", "jpg", 512));
    assert!(avatar.bytes.starts_with(&[0xFF, 0xD8, 0xFF]));
    assert_eq!(jpeg_icc_profile(&avatar.bytes), Some(profile));
    let decoded = ::image::load_from_memory(&avatar.bytes).unwrap().to_rgba8();
    assert_eq!(decoded.dimensions(), (512, 512));
    assert_color(&decoded, 100, 256, [200, 40, 40, 255], 12);
    assert_color(&decoded, 410, 256, [40, 40, 200, 255], 12);

    // A profile for other colours than RGB wouldn't describe the pixels written out.
    let grey = encode_jpeg_with(&photo, Some(icc_profile(b"GRAY")));
    let avatar = crop_and_encode_avatar(&grey, AvatarCrop::Centered).unwrap();
    assert_eq!(jpeg_icc_profile(&avatar.bytes), None);
    let plain = encode_jpeg_with(&photo, None);
    let avatar = crop_and_encode_avatar(&plain, AvatarCrop::Centered).unwrap();
    assert_eq!(jpeg_icc_profile(&avatar.bytes), None);

    // A PNG keeps its profile too.
    let png = encode_png_with(&photo, Some(icc_profile(b"RGB ")));
    let avatar = crop_and_encode_avatar(&png, AvatarCrop::Centered).unwrap();
    assert_eq!(avatar.mime_type, "image/png");
    assert_eq!(png_icc_profile(&avatar.bytes), Some(icc_profile(b"RGB ")));
}

#[test]
fn transparent_edges_keep_their_colour_when_scaled_down() {
    // Fully transparent black on the left, opaque white on the right: averaging
    // without the alpha in mind would leave a grey seam between them.
    let picture = ::image::RgbaImage::from_fn(1100, 1100, |x, _| {
        if x < 550 { ::image::Rgba([0, 0, 0, 0]) } else { ::image::Rgba([255, 255, 255, 255]) }
    });
    let mut png = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut png, ::image::ImageFormat::Png).unwrap();
    let avatar = crop_and_encode_avatar(png.get_ref(), AvatarCrop::Centered).unwrap();
    assert_eq!((avatar.mime_type, avatar.side), ("image/png", 512));
    let decoded = ::image::load_from_memory(&avatar.bytes).unwrap();
    assert_eq!(decoded.color(), ::image::ColorType::Rgba8);
    let decoded = decoded.to_rgba8();
    assert_eq!(decoded.get_pixel(10, 10).0[3], 0);
    assert_color(&decoded, 500, 10, [255, 255, 255, 255], 1);
    let mut seen_partial = false;
    for (_, _, pixel) in decoded.enumerate_pixels() {
        let [r, g, b, a] = pixel.0;
        if a > 0 {
            assert!(r >= 254 && g >= 254 && b >= 254, "dark seam: {:?}", pixel.0);
            seen_partial |= a < 255;
        }
    }
    assert!(seen_partial, "the seam should be soft");
}

#[test]
fn a_crop_that_no_longer_fits_the_picture_is_refused() {
    let png = quarters_png(80, 60);
    assert_eq!(
        crop_and_encode_avatar(&png, AvatarCrop::Chosen(rect(0, 0, 60, 81, 60))).unwrap_err(),
        AvatarCropError::Changed,
    );
    assert_eq!(
        crop_and_encode_avatar(&png, AvatarCrop::Chosen(rect(30, 0, 60, 80, 60))).unwrap_err(),
        AvatarCropError::OutOfBounds,
    );
    assert_eq!(
        crop_and_encode_avatar(&png, AvatarCrop::Chosen(rect(0, 0, 0, 80, 60))).unwrap_err(),
        AvatarCropError::OutOfBounds,
    );
    assert!(matches!(
        crop_and_encode_avatar(b"not a picture", AvatarCrop::Centered),
        Err(AvatarCropError::Decode(_)),
    ));
}

fn picked_file(name: &str, mime_type: &str, bytes: &[u8]) -> FileUploadMetadata {
    FileUploadMetadata {
        source: robius_file_picker::LocalFile::from_bytes(name, Some(mime_type), bytes).unwrap(),
        caption: None,
        mime_type: mime_type.to_string(),
        preview: FilePreview::Loading,
        size: bytes.len() as u64,
    }
}

#[test]
fn the_cropped_avatar_is_a_temporary_file_of_its_own() {
    let photo = ::image::RgbImage::from_pixel(700, 900, ::image::Rgb([10, 120, 220]));
    let picked = picked_file("holiday.jpeg", "image/jpeg", &encode_jpeg_with(&photo, None));
    let picked_path = picked.path().to_owned();
    let upload = prepare_avatar_upload(picked.clone(), AvatarCrop::Chosen(rect(0, 100, 700, 700, 900))).unwrap();
    assert_eq!(upload.file_name(), "holiday.jpg");
    assert_eq!(upload.mime_type, "image/jpeg");
    assert!(upload.caption.is_none());
    let bytes = std::fs::read(upload.path()).unwrap();
    assert_eq!(upload.size, bytes.len() as u64);
    assert_eq!(::image::load_from_memory(&bytes).unwrap().width(), 512);
    assert!(upload.source.is_temporary());

    // Each file goes once nothing refers to it anymore.
    let path = upload.path().to_owned();
    drop(upload);
    assert!(!path.exists());
    assert!(picked_path.exists());
    drop(picked);
    assert!(!picked_path.exists());

    // A picture other than a JPEG becomes a PNG.
    let picked = picked_file("Screen Shot.png", "image/png", &quarters_png(64, 64));
    let upload = prepare_avatar_upload(picked, AvatarCrop::Centered).unwrap();
    assert_eq!((upload.file_name().as_str(), upload.mime_type.as_str()), ("Screen Shot.png", "image/png"));
}

#[test]
fn a_picture_that_cannot_be_cropped_is_uploaded_as_picked_only_if_its_crop_was_never_seen() {
    // Confirmed before its preview was ready, and not a picture this app can decode:
    // the server may still take it, as it did before there was a crop tool.
    let picked = picked_file("avatar.avif", "image/avif", b"\0\0\0\x1cftypavif");
    let upload = prepare_avatar_upload(picked.clone(), AvatarCrop::Centered).unwrap();
    assert_eq!(upload.path(), picked.path());
    assert_eq!(upload.mime_type, "image/avif");

    // A crop chosen on a picture that can't be read any more is an error.
    let error = prepare_avatar_upload(picked, AvatarCrop::Chosen(rect(0, 0, 1, 1, 1))).unwrap_err();
    assert!(error.contains("avatar.avif"), "{error}");

    let picked = picked_file("small.png", "image/png", &quarters_png(40, 30));
    let error = prepare_avatar_upload(picked, AvatarCrop::Chosen(rect(0, 0, 30, 41, 30))).unwrap_err();
    assert!(error.contains("small.png"), "{error}");
}
