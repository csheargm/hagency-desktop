use super::*;
use makepad_widgets::makepad_draw::cx_draw::CxDraw;

fn fixture() -> (Cx, WidgetRef) {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let root = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {use mod.prelude.widgets.*
        mod.widgets.View {
            width: Fill height: Fit flow: Down
            text := mod.widgets.HtmlOrPlaintext {}
            attachments := mod.widgets.MessageDownloadSection {
                visible: true
                open_markdown_button.visible: true
            }
        }});
        let root = WidgetRef::script_from_value(vm, value);
        root.register_as_ui_root(vm);
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "Attachment/text fixture: {errors:?}");
        root
    });
    (cx, root)
}

#[test]
fn plaintext_room_mentions_escape_markup_and_keep_line_breaks() {
    let (mut cx, root) = fixture();
    let text = root.html_or_plaintext(&cx, ids!(text));
    let room = ruma::room_id!("!room:test").to_owned();
    for body in [
        "<b>literal</b> & @room\nnext\n\nlast",
        "<b>literal</b> & @room\nhttps://example.org/?a=1&b=2\nlast",
    ] {
        populate_text_message_content(&mut cx, &text, body, None, Some(&room), None, None, None);
        let html = text.html(&cx, ids!(html_view.html)).text();
        assert!(html.contains("&lt;b&gt;literal&lt;/b&gt; &amp; "), "{html}");
        assert!(html.contains("https://matrix.to/#/!room:test"), "{html}");
        assert!(html.contains("</a><br>"), "{html}");
        assert!(!html.contains("&amp;amp;"), "{html}");
        if body.contains("next") { assert!(html.contains("next<br><br>last"), "{html}"); }
    }
    let formatted = FormattedBody {format: MessageFormat::Html, body: "<b>rich</b><br>kept".into()};
    populate_text_message_content(&mut cx, &text, "rich\nkept", Some(&formatted), None, None, None, None);
    assert_eq!(text.html(&cx, ids!(html_view.html)).text(), formatted.body);
    populate_text_message_content(&mut cx, &text, "plain\ntext", None, None, None, None, None);
    assert!(text.view(&cx, ids!(plaintext_view)).visible());
}

#[test]
fn attachment_actions_wrap_inside_narrow_panes_at_larger_text_sizes() {
    let (mut cx, root) = fixture();
    for font_size in [12.0, 22.0] {
        for path in [ids!(attachments.open_markdown_button), ids!(attachments.download_button), ids!(attachments.share_button)] {
            let mut button = root.button(&cx, path);
            script_apply_eval!(cx, button, {draw_text.text_style.font_size: #(font_size)});
        }
        for width in [240.0, 320.0, 720.0] {
            cx.redraw_all();
            let pass = DrawPass::new(&mut cx);
            pass.set_size(&mut cx, dvec2(width, 600.0));
            let mut list = DrawList2d::new(&mut cx);
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(&mut cx, &event);
            let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None);
            list.begin_always(&mut cx2d);
            cx2d.begin_root_turtle(dvec2(width, 600.0), Layout::flow_overlay());
            root.draw_all(&mut cx2d, &mut Scope::empty());
            cx2d.end_pass_sized_turtle();
            list.end(&mut cx2d);
            cx2d.end_pass(&pass);
            drop(cx2d); drop(draw);
            let container = root.view(&cx, ids!(attachments)).area().rect(&cx);
            for path in [ids!(attachments.open_markdown_button), ids!(attachments.download_button), ids!(attachments.share_button)] {
                let rect = root.button(&cx, path).area().rect(&cx);
                assert!(rect.size.x > 0.0 && rect.size.y > 0.0);
                assert!(rect.pos.x >= container.pos.x && rect.pos.x + rect.size.x <= width + 0.5,
                    "font {font_size}, width {width}: {rect:?}");
                assert!(rect.pos.y + rect.size.y <= container.pos.y + container.size.y + 0.5);
            }
        }
    }
}
