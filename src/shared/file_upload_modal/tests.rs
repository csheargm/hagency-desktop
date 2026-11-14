use super::*;
use crate::shared::avatar_crop::AvatarCropViewWidgetRefExt;
use makepad_widgets::event::ScrollEvent;
use makepad_widgets::makepad_draw::cx_draw::CxDraw;
use std::cell::Cell;

fn fixture() -> (Cx, WidgetRef) {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let modal = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, { mod.widgets.FileUploadModal {} });
        let modal = WidgetRef::script_from_value(vm, value);
        modal.register_as_ui_root(vm);
        assert!(vm.take_errors().is_empty());
        modal
    });
    (cx, modal)
}

fn draw(cx: &mut Cx, modal: &WidgetRef, size: Vec2d) {
    cx.redraw_all();
    let pass = DrawPass::new(cx);
    pass.set_size(cx, size);
    let mut list = DrawList2d::new(cx);
    let event = DrawEvent::default();
    let mut draw = CxDraw::new(cx, &event);
    let mut cx = Cx2d::new(&mut draw);
    cx.begin_pass(&pass, None);
    list.begin_always(&mut cx);
    cx.begin_root_turtle(size, Layout::flow_overlay());
    modal.draw_all(&mut cx, &mut Scope::empty());
    cx.end_pass_sized_turtle();
    list.end(&mut cx);
    cx.end_pass(&pass);
}

#[test]
fn upload_preview_fits_content_and_keeps_actions_inside_the_window() {
    let (mut cx, modal) = fixture();
    for size in [dvec2(1440.0, 900.0), dvec2(800.0, 600.0), dvec2(390.0, 640.0)] {
        for text in [false, true] {
            let preview = if text {
                FilePreview::Text(TextPreview { content: "Preview line\n".repeat(80), truncated: false, is_code: false })
            } else {
                FilePreview::None
            };
            modal.borrow_mut::<FileUploadModal>().unwrap().show_preview(&mut cx, preview);
            draw(&mut cx, &modal, size);
            let card = modal.area().rect(&cx);
            let upload = modal.button(&mut cx, ids!(upload_button)).area().rect(&cx);
            assert!(card.size.x > 250.0 && card.size.x <= 560.0, "{size:?}: {card:?}");
            assert!(card.size.y < 580.0 && card.size.y < size.y, "{size:?}: {card:?}");
            assert!(upload.size.x > 0.0 && upload.size.y > 0.0);
            assert!(card.contains(upload.pos) && card.contains(upload.pos + upload.size), "Upload button outside card: {card:?} {upload:?}");
            assert!(upload.pos.y + upload.size.y <= size.y, "Upload button outside window: {upload:?}");
            if !text { assert!(card.size.y < 430.0, "Document without preview should stay compact: {card:?}"); }
        }
    }
}


#[test]
fn a_pasted_image_becomes_a_temporary_png_that_is_removed_when_dropped() {
    use makepad_widgets::makepad_platform::clipboard_read::ClipboardImage;

    // PNG bytes from the clipboard are written as they are.
    let mut png = std::io::Cursor::new(Vec::new());
    ::image::RgbaImage::from_pixel(3, 2, ::image::Rgba([10, 20, 30, 255]))
        .write_to(&mut png, ::image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    let file = pasted_image_file("pasted-image-test-a.png", &ClipboardImage::Png(Arc::new(png.clone()))).unwrap();
    let path = file.path().to_owned();
    assert_eq!(path.file_name().unwrap(), "pasted-image-test-a.png");
    assert_eq!(std::fs::read(&path).unwrap(), png);
    assert_eq!(file.mime_type(), Some("image/png"));
    assert!(file.is_temporary());

    // The metadata the upload modal shows: a PNG of that size, named after the file.
    let (metadata, _preview) = load_file_metadata(file.clone()).unwrap();
    assert_eq!(metadata.mime_type, "image/png");
    assert_eq!(metadata.size, png.len() as u64);
    assert_eq!(metadata.file_name(), "pasted-image-test-a.png");

    // The file lives as long as something refers to it, and no longer.
    drop(file);
    assert!(path.exists());
    drop(metadata);
    drop(_preview);
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());

    // Raw pixels (the Windows clipboard form) are encoded to a PNG of the same size.
    let rgba = ClipboardImage::Rgba { width: 4, height: 3, rgba: Arc::new(vec![200; 4 * 3 * 4]) };
    let file = pasted_image_file("pasted-image-test-b.png", &rgba).unwrap();
    let decoded = ::image::load_from_memory(&std::fs::read(file.path()).unwrap()).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (4, 3));

    // Pixel data that does not match its stated size, or no data at all, is refused.
    let short = ClipboardImage::Rgba { width: 4, height: 3, rgba: Arc::new(vec![0; 5]) };
    assert_eq!(pasted_image_file("x.png", &short).unwrap_err(), "The pasted image has an unexpected size.");
    let empty = ClipboardImage::Png(Arc::new(Vec::new()));
    assert_eq!(pasted_image_file("x.png", &empty).unwrap_err(), "The pasted image is empty.");
}

/// A picked picture to set as the avatar. `size` is what the dialog is told it weighs.
fn avatar_upload(size: u64) -> PendingUpload {
    let source = robius_file_picker::LocalFile::from_bytes("me.png", Some("image/png"), b"not read here").unwrap();
    PendingUpload::Avatar(FileUploadMetadata {
        source,
        caption: None,
        mime_type: "image/png".into(),
        preview: FilePreview::Loading,
        size,
    })
}

/// An 800 x 600 picture, as the picker's thread hands it over for a new avatar.
fn avatar_picture() -> FilePreview {
    let mut buffer = ImageBuffer::default();
    buffer.width = 800;
    buffer.height = 600;
    buffer.data = vec![0xff3366cc; 800 * 600];
    FilePreview::Image(buffer).for_avatar()
}

/// Hands `event` to the modal, then the actions it caused, as the app's event loop does.
fn deliver(cx: &mut Cx, modal: &WidgetRef, event: &Event) {
    let actions = cx.capture_actions(|cx| modal.handle_event(cx, event, &mut Scope::empty()));
    if !actions.is_empty() {
        modal.handle_event(cx, &Event::Actions(actions), &mut Scope::empty());
    }
}

fn zoom(cx: &Cx, modal: &WidgetRef) -> f64 {
    modal.avatar_crop_view(cx, ids!(avatar_crop_view)).frame().unwrap().zoom()
}

fn slider_value(cx: &Cx, modal: &WidgetRef) -> f64 {
    modal.slider(cx, ids!(avatar_zoom_slider)).value().unwrap()
}

#[test]
fn only_an_avatar_picture_goes_to_the_crop_tool() {
    assert!(matches!(avatar_picture(), FilePreview::AvatarCrop(_)));
    assert!(matches!(FilePreview::Image(ImageBuffer::default()).for_avatar(), FilePreview::None));
    assert!(matches!(FilePreview::None.for_avatar(), FilePreview::None));
    let text = FilePreview::Text(TextPreview { content: "<svg/>".into(), truncated: false, is_code: true });
    assert!(matches!(text.for_avatar(), FilePreview::Text(_)));
}

#[test]
fn an_avatar_picture_opens_in_the_crop_tool_that_fits_the_window() {
    let (mut cx, modal) = fixture();
    // Over 10 MB, which a cropped avatar never is.
    modal.borrow_mut::<FileUploadModal>().unwrap().set_upload(&mut cx, avatar_upload(12_000_000), 7);
    assert!(!modal.widget(&cx, ids!(large_attachment_warning_label)).visible());
    assert_eq!(modal.borrow::<FileUploadModal>().unwrap().avatar_crop(&mut cx), Some(AvatarCrop::Centered));
    modal.borrow_mut::<FileUploadModal>().unwrap().set_preview(&mut cx, 7, avatar_picture());
    assert_eq!(modal.borrow::<FileUploadModal>().unwrap().preview_page, id!(avatar_crop_page));
    assert!(!modal.widget(&cx, ids!(large_attachment_warning_label)).visible());
    assert!(!modal.widget(&cx, ids!(caption_view)).visible());
    assert_eq!(modal.button(&cx, ids!(upload_button)).text(), crate::i18n::tr("Set Avatar"));

    for size in [dvec2(1440.0, 900.0), dvec2(800.0, 600.0), dvec2(390.0, 640.0)] {
        draw(&mut cx, &modal, size);
        let card = modal.area().rect(&cx);
        let inside = |rect: Rect| card.contains(rect.pos) && card.contains(rect.pos + rect.size);
        let crop = modal.widget(&cx, ids!(avatar_crop_view)).area().rect(&cx);
        assert_eq!(crop.size, dvec2(256.0, 256.0), "{size:?}");
        assert!(inside(crop), "{size:?}: crop view {crop:?} outside {card:?}");
        // A widget whose shader didn't compile draws nothing, and has no size here.
        for path in [ids!(avatar_zoom_out_button), ids!(avatar_zoom_slider), ids!(avatar_zoom_in_button), ids!(avatar_crop_hint)] {
            let rect = modal.widget(&cx, path).area().rect(&cx);
            assert!(rect.size.x > 0.0 && rect.size.y > 0.0, "{size:?}: {path:?} {rect:?}");
            assert!(inside(rect), "{size:?}: {path:?} {rect:?} outside {card:?}");
            assert!(rect.pos.y >= crop.pos.y + crop.size.y, "{size:?}: {path:?} overlaps the crop view");
        }
        let preview = modal.widget(&cx, ids!(preview_block)).area().rect(&cx);
        let hint = modal.widget(&cx, ids!(avatar_crop_hint)).area().rect(&cx);
        assert!(hint.pos.y + hint.size.y <= preview.pos.y + preview.size.y, "{size:?}: hint {hint:?} below {preview:?}");
        let upload = modal.button(&cx, ids!(upload_button)).area().rect(&cx);
        assert!(card.size.y < size.y, "{size:?}: {card:?}");
        assert!(inside(upload), "{size:?}: upload button {upload:?} outside {card:?}");
        assert!(upload.pos.y + upload.size.y <= size.y, "{size:?}: {upload:?}");
    }

    // The square to upload is the one shown, until the dialog is reset.
    let crop = modal.avatar_crop_view(&cx, ids!(avatar_crop_view)).crop_rect().unwrap();
    assert_eq!((crop.x, crop.y, crop.side), (100, 0, 600));
    assert_eq!(modal.borrow::<FileUploadModal>().unwrap().avatar_crop(&mut cx), Some(AvatarCrop::Chosen(crop)));
    modal.borrow_mut::<FileUploadModal>().unwrap().reset(&mut cx);
    assert!(modal.avatar_crop_view(&cx, ids!(avatar_crop_view)).frame().is_none());
}

#[test]
fn an_avatar_picture_without_a_preview_is_uploaded_as_picked() {
    let (mut cx, modal) = fixture();
    modal.borrow_mut::<FileUploadModal>().unwrap().set_upload(&mut cx, avatar_upload(12_000_000), 3);
    modal.borrow_mut::<FileUploadModal>().unwrap().set_preview(&mut cx, 3, FilePreview::None.for_avatar());
    assert_eq!(modal.borrow::<FileUploadModal>().unwrap().avatar_crop(&mut cx), None);
    // Then the file's own size is what's uploaded, so it's warned about.
    assert!(modal.widget(&cx, ids!(large_attachment_warning_label)).visible());
}

#[test]
fn the_crop_tool_follows_the_pointer_and_keeps_its_slider_in_step() {
    let (mut cx, modal) = fixture();
    modal.borrow_mut::<FileUploadModal>().unwrap().set_upload(&mut cx, avatar_upload(5000), 1);
    modal.borrow_mut::<FileUploadModal>().unwrap().set_preview(&mut cx, 1, avatar_picture());
    draw(&mut cx, &modal, dvec2(1440.0, 900.0));
    let crop = modal.widget(&cx, ids!(avatar_crop_view)).area().clipped_rect(&cx);
    let center = crop.center();
    assert_eq!((zoom(&cx, &modal), slider_value(&cx, &modal)), (1.0, 0.0));

    // Scrolling up over the picture zooms in, about the pointer, and moves the slider.
    let scroll = |abs: DVec2, y: f64| Event::Scroll(ScrollEvent {
        window_id: WindowId(0, 0),
        scroll: dvec2(0.0, y),
        abs,
        modifiers: KeyModifiers::default(),
        handled_x: Cell::new(false),
        handled_y: Cell::new(false),
        is_mouse: true,
        time: 1.0,
        phase: Default::default(),
    });
    let event = scroll(center, -200.0);
    deliver(&mut cx, &modal, &event);
    assert!((zoom(&cx, &modal) - 2.0).abs() < 1e-9, "{}", zoom(&cx, &modal));
    assert!((slider_value(&cx, &modal) - 0.5).abs() < 1e-6, "{}", slider_value(&cx, &modal));
    let crop_rect = modal.avatar_crop_view(&cx, ids!(avatar_crop_view)).crop_rect().unwrap();
    assert_eq!((crop_rect.x, crop_rect.y, crop_rect.side), (250, 150, 300));
    // The dialog under the picture doesn't scroll with it.
    let Event::Scroll(scrolled) = &event else { unreachable!() };
    assert!(scrolled.handled_y.get());
    // Scrolling down zooms back out about the pointer, here the circle's top left,
    // and in again about the same point returns to the same square.
    let corner = crop.pos + dvec2(12.0, 12.0);
    deliver(&mut cx, &modal, &scroll(corner, 100.0));
    assert!((zoom(&cx, &modal) - 2f64.sqrt()).abs() < 1e-9, "{}", zoom(&cx, &modal));
    assert!((slider_value(&cx, &modal) - 0.25).abs() < 1e-6, "{}", slider_value(&cx, &modal));
    deliver(&mut cx, &modal, &scroll(corner, -100.0));
    assert_eq!(modal.avatar_crop_view(&cx, ids!(avatar_crop_view)).crop_rect(), Some(crop_rect));

    // Dragging the picture left shows more of its right side.
    let at = |abs: DVec2| (abs, KeyModifiers::default(), WindowId(0, 0));
    let (start, modifiers, window_id) = at(center);
    let end = start - dvec2(58.0, 0.0);
    // The platform marks the button held before it hands the press on, and lets go after the release.
    cx.fingers.first_mouse_button = Some((MouseButton::PRIMARY, window_id));
    for event in [
        Event::MouseDown(MouseDownEvent { abs: start, button: MouseButton::PRIMARY, window_id, modifiers, handled: Cell::new(Area::Empty), time: 2.0 }),
        Event::MouseMove(MouseMoveEvent { abs: start - dvec2(20.0, 0.0), lock_delta: DVec2::default(), window_id, modifiers, handled: Cell::new(Area::Empty), time: 2.1 }),
        Event::MouseMove(MouseMoveEvent { abs: end, lock_delta: DVec2::default(), window_id, modifiers, handled: Cell::new(Area::Empty), time: 2.2 }),
        Event::MouseUp(MouseUpEvent { abs: end, button: MouseButton::PRIMARY, window_id, modifiers, time: 2.3 }),
    ] {
        deliver(&mut cx, &modal, &event);
    }
    cx.fingers.first_mouse_button = None;
    // 232 points across 300 pixels: 58 points are 75 pixels.
    let dragged = modal.avatar_crop_view(&cx, ids!(avatar_crop_view)).crop_rect().unwrap();
    assert_eq!((dragged.x, dragged.y, dragged.side), (325, 150, 300), "{dragged:?}");

    // The slider zooms the picture about the circle's centre.
    let slider = modal.slider(&cx, ids!(avatar_zoom_slider)).widget_uid();
    let actions = cx.capture_actions(|cx| cx.widget_action(slider, SliderAction::Slide(1.0)));
    modal.handle_event(&mut cx, &Event::Actions(actions), &mut Scope::empty());
    assert!((zoom(&cx, &modal) - 4.0).abs() < 1e-9);
    let zoomed = modal.avatar_crop_view(&cx, ids!(avatar_crop_view)).crop_rect().unwrap();
    assert_eq!((zoomed.x, zoomed.y, zoomed.side), (400, 225, 150), "{zoomed:?}");

    // And so do the buttons, in sixths of the way.
    for (path, expected) in [(ids!(avatar_zoom_out_button), 5.0 / 6.0), (ids!(avatar_zoom_out_button), 4.0 / 6.0), (ids!(avatar_zoom_in_button), 5.0 / 6.0)] {
        let button = modal.button(&cx, path).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(button, ButtonAction::Clicked(KeyModifiers::default())));
        modal.handle_event(&mut cx, &Event::Actions(actions), &mut Scope::empty());
        assert!((slider_value(&cx, &modal) - expected).abs() < 1e-6, "{path:?}: {}", slider_value(&cx, &modal));
    }
}
