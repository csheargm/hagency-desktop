//! Shared Apple font setup; Rinx selects it before registering widgets.
pub(crate) use article_makepad::apple_fonts::install;

/// Lays PingFang over the base themes now and over every later build of them.
/// JIT PORT: upstream builds the base themes again inside `widgets_mod` when a
/// style sheet is worn, which drops faces set on them before it; the fork built
/// them once per module run, so one `install` between `theme_mod` and
/// `widgets_mod` was enough.
pub(crate) fn install_over_theme_builds(vm: &mut makepad_widgets::ScriptVm) {
    makepad_widgets::set_app_theme_fonts(vm, Some(install));
    install(vm);
}

#[cfg(test)]
mod tests {
    use super::install_over_theme_builds;
    use makepad_widgets::*;
    use makepad_widgets::makepad_draw::{
        cx_draw::CxDraw,
        text::{fonts::Fonts, font_face::FontFace},
    };
    use std::{cell::RefCell, rc::Rc};

    fn register(vm: &mut ScriptVm) {
        makepad_widgets::theme_mod(vm);
        script_eval!(vm, { mod.theme = mod.themes.light });
        install_over_theme_builds(vm);
        makepad_widgets::widgets_mod(vm);
        desktop_style::apply_widgets(vm);
        crate::app::register_widgets(vm);
    }

    fn selected_font_names(cx: &mut Cx, style: &TextStyle, text: &str) -> Vec<String> {
        style.ensure_fonts_loaded(cx);
        let fonts = cx.get_global::<Rc<RefCell<Fonts>>>().clone();
        let family = fonts
            .borrow_mut()
            .get_or_load_font_family(style.font_family_id());
        family
            .get_or_shape(text.into())
            .glyphs
            .iter()
            .map(|glyph| {
                assert_ne!(glyph.id, 0);
                let face = FontFace::from_data_and_index(glyph.font.data().clone(), 0).unwrap();
                face.with_ttf_parser_face(|face| {
                    face.names()
                        .into_iter()
                        .find_map(|name| (name.name_id == 6).then(|| name.to_string()).flatten())
                        .unwrap()
                })
            })
            .collect()
    }

    fn draw_chat(cx: &mut Cx, html: &mut Html) {
        cx.redraw_all();
        let pass = DrawPass::new(cx);
        pass.set_size(cx, dvec2(800., 500.));
        let mut list = DrawList2d::new(cx);
        let event = DrawEvent {
            redraw_all: true,
            ..DrawEvent::default()
        };
        let mut draw = CxDraw::new(cx, &event);
        let mut cx2d = Cx2d::new(&mut draw);
        cx2d.begin_pass(&pass, None);
        list.begin_always(&mut cx2d);
        cx2d.begin_root_turtle(dvec2(800., 500.), Layout::flow_down());
        html.draw_walk_all(
            &mut cx2d,
            &mut Scope::empty(),
            Walk {
                width: Size::Fixed(760.),
                height: Size::Fit {
                    min: None,
                    max: None,
                },
                ..Walk::default()
            },
        );
        cx2d.end_pass_sized_turtle();
        list.end(&mut cx2d);
        cx2d.end_pass(&pass);
    }

    #[test]
    fn apple_fonts_actual_chat_glyphs_survive_draw_theme_reload_and_fresh_start() {
        if !article_makepad::apple_fonts::installed() {
            eprintln!("skipping: PingFang SC is not installed on this machine");
            return;
        }
        // Repeat with a fresh context, then with retained chat widgets across
        // reloads: cold-start success alone must not hide this regression.
        for _ in 0..2 {
            let mut cx = Cx::new(Box::new(|_, _| {}));
            let mut chat: Option<Html> = None;
            for dark in [false, true, false] {
                let mut html = cx.with_vm(|vm| {
                    let selection = crate::theme::Selection {
                        appearance: if dark {
                            crate::theme::Appearance::Dark
                        } else {
                            crate::theme::Appearance::Light
                        },
                        ..Default::default()
                    };
                    desktop_style::install(
                        vm,
                        selection.stylesheet(desktop_style::DesktopStyle::Macos),
                    );
                    if chat.is_some() {
                        vm.with_reload(register);
                    } else {
                        register(vm);
                    }
                    let value = script_eval!(vm, {mod.widgets.MessageHtml {}});
                    if let Some(mut retained) = chat.take() {
                        retained.script_apply(
                            vm,
                            &Apply::ScriptReapply,
                            &mut Scope::empty(),
                            value,
                        );
                        retained
                    } else {
                        Html::script_from_value(vm, value)
                    }
                });
                html.set_text(&mut cx, "<p>English 中文繁體 0123</p><p><b>English 中文繁體 0123</b></p><code>Hello 123</code>");
                for _ in 0..2 {
                    cx.script_data.resources.bump_generation();
                    draw_chat(&mut cx, &mut html);
                    for (style, expected) in [
                        (&html.text_style_normal, "PingFangSC-Regular"),
                        (&html.text_style_bold, "PingFangSC-Semibold"),
                    ] {
                        let fonts = selected_font_names(&mut cx, style, "English 中文繁體 0123");
                        assert!(!fonts.is_empty());
                        assert!(fonts.iter().all(|font| font == expected), "{fonts:?}");
                    }
                    let fonts = selected_font_names(&mut cx, &html.text_style_fixed, "Hello 123");
                    assert!(
                        fonts.iter().all(
                            |font| font == "LiberationMono" || font == "LiberationMono-Regular"
                        ),
                        "{fonts:?}"
                    );
                }
                chat = Some(html);
            }
        }
    }

    #[test]
    fn apple_fonts_respect_explicit_monospaced_theme() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let style = cx.with_vm(|vm| {
            let mut sheet = crate::theme::Selection::default().stylesheet(desktop_style::DesktopStyle::Macos);
            sheet.theme.push_str("\nmod.theme.font_regular = mod.theme.font_code {}\nmod.theme.font_bold = mod.theme.font_code {}\n");
            desktop_style::install(vm, sheet);
            register(vm);
            let value = script_eval!(vm, {mod.widgets.MessageHtml.text_style_normal});
            TextStyle::script_from_value(vm, value)
        });
        let fonts = selected_font_names(&mut cx, &style, "Hello 123");
        assert!(
            fonts
                .iter()
                .all(|font| font == "LiberationMono" || font == "LiberationMono-Regular"),
            "{fonts:?}"
        );
    }
}
