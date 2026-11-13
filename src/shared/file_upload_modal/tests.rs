use super::*;
use makepad_widgets::makepad_draw::cx_draw::CxDraw;

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
