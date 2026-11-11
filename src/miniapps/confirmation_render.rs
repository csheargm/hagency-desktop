//! Native consent phases replace layout, including previously hidden widgets.
//! Invalidate the owning pass (the separate mini-app window on desktop), not
//! only the old child areas. Defer once so an asynchronous completion observed
//! during a render cannot lose its invalidation at the current draw boundary.
use super::*;

impl MiniAppsPanel {
    pub(in crate::octoscript_apps::ui) fn request_confirmation_redraw(&mut self, cx: &mut Cx) {
        self.redraw_confirmation_pass(cx);
        self.confirmation_redraw = cx.new_next_frame();
    }

    pub(in crate::octoscript_apps::ui) fn redraw_confirmation_pass(&self, cx: &mut Cx) {
        let pass = self
            .view
            .area()
            .draw_list_id()
            .and_then(|id| cx.draw_lists.checked_index(id))
            .and_then(|list| list.draw_pass_id);
        if let Some(pass) = pass {
            crate::jit_compat::redraw_pass_and_parent_passes(cx, pass);
        } else {
            // Before the first native layout there is no owning area yet.
            cx.redraw_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;

    fn frame(
        cx: &mut Cx,
        panel: &mut MiniAppsPanel,
        pass: &DrawPass,
        list: &mut DrawList2d,
        width: f64,
        height: f64,
    ) {
        // Consume the actual queued invalidation; no forced redraw is inserted.
        let event = std::mem::take(&mut cx.new_draw_event);
        assert!(event.will_redraw(), "phase did not request a frame");
        let mut draw = CxDraw::new(cx, &event);
        let mut cx2d = Cx2d::new(&mut draw);
        cx2d.begin_pass(pass, None);
        if list.begin(&mut cx2d, Walk::default()).is_redrawing() {
            cx2d.begin_root_turtle(dvec2(width, height), Layout::flow_overlay());
            panel.draw_all(&mut cx2d, &mut Scope::empty());
            cx2d.end_pass_sized_turtle();
            list.end(&mut cx2d);
        }
        cx2d.end_pass(pass);
    }

    #[test]
    fn native_confirmation_transitions_redraw_details_and_footer_in_all_panels() {
        for (width, height, cn) in [
            (360.0, 640.0, false),
            (360.0, 640.0, true),
            (840.0, 700.0, false),
            (840.0, 700.0, true),
        ] {
            let mut cx = Cx::new(Box::new(|_, _| {}));
            let mut panel = cx.with_vm(|vm| {
                makepad_widgets::script_mod(vm);
                crate::i18n::install(vm);
                makepad_code_editor::script_mod(vm);
                crate::shared::script_mod(vm);
                vm.bx.captured_errors = Some(Vec::new());
                crate::miniapps::script_mod(vm);
                let value =
                    vm.eval(script! {use mod.prelude.widgets.* use mod.widgets.* MiniAppsPanel{}});
                let panel = MiniAppsPanel::script_from_value(vm, value);
                assert!(vm.take_errors().is_empty());
                panel
            });
            for id in [
                ids!(import_form),
                ids!(app_content),
                ids!(notice),
                ids!(approval),
                ids!(grant_confirmation),
                ids!(ceiling_confirmation),
                ids!(adoption_confirmation),
                ids!(execution_confirmation),
            ] {
                panel.view.widget(&cx, id).set_visible(&mut cx, false);
            }
            let pass = DrawPass::new(&mut cx);
            pass.set_size(&mut cx, dvec2(width, height));
            let mut list = DrawList2d::new(&mut cx);
            for root in [
                id!(adoption_confirmation),
                id!(grant_confirmation),
                id!(ceiling_confirmation),
                id!(execution_confirmation),
            ] {
                // The native sheets keep details behind their one disclosure
                // and a recovery step in a grouped row above it.
                let sheet = root == id!(grant_confirmation) || root == id!(ceiling_confirmation);
                let recover: Vec<LiveId> = if sheet {
                    vec![root, id!(sheet_scroll), id!(actions), id!(recover)]
                } else {
                    vec![root, id!(buttons), id!(recover)]
                };
                panel.view.widget(&cx, &[root]).set_visible(&mut cx, true);
                // Initial Choose, then the async Loading state removes the
                // previously visible details and primary control.
                for phase in [
                    "choose", "loading", "review", "loading", "progress", "failed", "loading",
                    "review",
                ] {
                    let details = if phase == "loading" || phase == "failed" {
                        "".to_owned()
                    } else if cn {
                        "资源标识\n原始资源\n共享月度上限\n400000\n".repeat(40)
                    } else {
                        "Resource ID\noriginal-resource\nShared monthly limit\n400000\n".repeat(40)
                    };
                    let primary = phase == "choose" || phase == "review";
                    panel
                        .view
                        .view(&cx, &[root, id!(sheet_scroll), id!(choices)])
                        .set_visible(&mut cx, phase == "choose");
                    panel
                        .view
                        .label(&cx, &[root, id!(sheet_scroll), id!(status)])
                        .set_text(&mut cx, phase);
                    panel
                        .view
                        .label(&cx, &[root, id!(sheet_scroll), id!(details), id!(copy)])
                        .set_text(&mut cx, &details);
                    panel
                        .view
                        .view(&cx, &[root, id!(sheet_scroll), id!(details)])
                        .set_visible(&mut cx, !details.is_empty());
                    panel
                        .view
                        .widget(&cx, &[root, id!(buttons), id!(primary)])
                        .set_visible(&mut cx, primary);
                    if sheet {
                        panel
                            .view
                            .widget(&cx, &[root, id!(sheet_scroll), id!(more)])
                            .set_visible(&mut cx, !details.is_empty());
                        panel
                            .view
                            .widget(&cx, &[root, id!(sheet_scroll), id!(actions)])
                            .set_visible(&mut cx, phase == "failed" || phase == "progress");
                    }
                    panel
                        .view
                        .widget(&cx, &recover)
                        .set_visible(&mut cx, phase == "failed" || phase == "progress");
                    panel.request_confirmation_redraw(&mut cx);
                    frame(&mut cx, &mut panel, &pass, &mut list, width, height);
                    assert_eq!(
                        panel
                            .view
                            .label(&cx, &[root, id!(sheet_scroll), id!(status)])
                            .text(),
                        phase
                    );
                    let details_widget = panel.view.widget(&cx, &[root, id!(sheet_scroll), id!(details)]);
                    assert_eq!(details_widget.visible(), !details.is_empty());
                    if !details.is_empty() {
                        assert!(details_widget.area().is_valid(&cx));
                        assert!(details_widget.area().rect(&cx).size.y > 0.0);
                    }
                    for control in [
                        vec![root, id!(buttons), id!(cancel)],
                        if primary {
                            vec![root, id!(buttons), id!(primary)]
                        } else if phase == "failed" || phase == "progress" {
                            recover.clone()
                        } else {
                            vec![root, id!(buttons), id!(cancel)]
                        },
                    ] {
                        let widget = panel.view.widget(&cx, &control);
                        let rect = widget.area().rect(&cx);
                        assert!(widget.visible() && widget.area().is_valid(&cx));
                        assert!(
                            rect.size.x > 0.0
                                && rect.size.y >= 40.0
                                && rect.pos.y >= 0.0
                                && rect.pos.y + rect.size.y <= height + 0.5,
                            "{phase} {root:?} {rect:?}"
                        );
                    }
                    assert_ne!(panel.confirmation_redraw, NextFrame::default());
                }
                panel.view.widget(&cx, &[root]).set_visible(&mut cx, false);
            }
        }
    }
}
