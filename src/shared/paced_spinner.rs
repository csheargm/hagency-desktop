//! A loading spinner that updates a retained draw call at a bounded rate.
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.PacedLoadingSpinner = #(PacedLoadingSpinner::register_widget(vm)) {
        ..mod.widgets.LoadingSpinner
        // Each spinner must have its own draw call for its clock uniform.
        new_batch: true
        draw_bg +: {
            clock: uniform(0.0)
            // Match Makepad's LoadingSpinner arc while driving it with a paced clock.
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let radius = min(self.rect_size.x * 0.5, self.rect_size.y * 0.5) - self.stroke_width * 0.5
                let center = self.rect_size * 0.5
                let rotation = self.clock * self.rotation_speed * 2.0 * PI
                let rotation_cycles = rotation / (2.0 * PI)
                let arc_phase = modf(rotation_cycles * 0.5, 1.0)
                let expand_phase = clamp(arc_phase / 0.55, 0.0, 1.0)
                let contract_phase = clamp((arc_phase - 0.55) / 0.45, 0.0, 1.0)
                let cycle = expand_phase * (1.0 - contract_phase)
                let gap_ratio = mix(self.min_gap_ratio, self.max_gap_ratio, cycle)
                let gap_radians = gap_ratio * 2.0 * PI
                sdf.arc_round_caps(
                    center.x, center.y, radius, rotation,
                    rotation + 2.0 * PI - gap_radians, self.stroke_width
                )
                return sdf.fill(self.color)
            }
        }
    }
}

const UPDATE_SECONDS: f64 = 1.0 / 30.0;
const VISIBILITY_CHECK_SECONDS: f64 = 0.25;

#[derive(Script, ScriptHook, Widget)]
pub struct PacedLoadingSpinner {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] timer: Timer,
    #[rust] started_at: Option<f64>,
    #[rust] timer_due_at: f64,
    #[rust] next_visibility_check: f64,
}

impl PacedLoadingSpinner {
    fn schedule(&mut self, cx: &mut Cx, now: f64) {
        self.timer = cx.start_timeout(UPDATE_SECONDS);
        self.timer_due_at = now + UPDATE_SECONDS;
    }

    fn is_attached(&self, cx: &Cx) -> bool {
        let Some(area) = self.view.draw_bg.draw_vars.area.valid_instance(cx) else { return false };
        let Some(pass_id) = cx.draw_lists[area.draw_list_id].draw_pass_id else { return false };
        cx.attached_draw_lists(pass_id).contains(&area.draw_list_id)
    }
}

impl Widget for PacedLoadingSpinner {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.timer.is_empty() && self.timer.is_event(event).is_some() {
            self.timer = Timer::empty();
            if !self.view.visible {
                self.started_at = None;
            } else if let Some(started_at) = self.started_at {
                let now = Cx::monotonic_now();
                // Hidden dock pages can retain valid draw calls that are no
                // longer attached to the window's draw tree.
                let attached = if now >= self.next_visibility_check {
                    self.next_visibility_check = now + VISIBILITY_CHECK_SECONDS;
                    self.is_attached(cx)
                } else {
                    true
                };
                if attached {
                    let elapsed = (now - started_at).max(0.0) as f32;
                    self.view.draw_bg.draw_vars.set_uniform_on_area(cx, id!(clock), &[elapsed]);
                    // A one-shot timer expires naturally if this widget is removed.
                    self.schedule(cx, now);
                }
            }
        }
        // A Dock can reattach a cached draw list without calling draw_walk on
        // its children. A navigation action gets one delayed visibility check;
        // it resumes a reattached spinner and expires for a still-hidden one.
        if self.started_at.is_some() && self.view.visible
            && matches!(event, Event::Actions(_) | Event::Custom(_))
        {
            let now = Cx::monotonic_now();
            if self.timer.is_empty() || now > self.timer_due_at + 0.1 {
                // A covered parent may not have forwarded the last timeout.
                cx.stop_timer(self.timer);
                self.next_visibility_check = 0.0;
                self.schedule(cx, now);
            }
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.view.visible {
            let now = Cx::monotonic_now();
            if self.started_at.is_none() {
                self.started_at = Some(now);
            }
            if self.timer.is_empty() || now > self.timer_due_at + 0.1 {
                // A covered panel may not have forwarded the timeout.
                cx.stop_timer(self.timer);
                self.next_visibility_check = 0.0;
                self.schedule(cx, now);
            }
        } else {
            cx.stop_timer(self.timer);
            self.timer = Timer::empty();
            self.started_at = None;
        }
        self.view.draw_walk(cx, scope, walk)
    }
}
