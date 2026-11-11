use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.BouncingDots = #(BouncingDots::register_widget(vm)) {
        width: 24,
        height: 12,
        flow: Down,
        // Keep this shader in its own draw call so its animated uniform cannot
        // change another instance of the indicator.
        new_batch: true,

        show_bg: true,
        draw_bg +: {
            color: uniform(mod.widgets.RINX_INK),
            anim_time: uniform(0.0),
            freq: uniform(0.9),  // Animation frequency
            phase_offset: uniform(5.0), // Phase difference
            dot_radius: uniform(1.5), // Dot radius
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size);
                let amplitude = self.rect_size.y * 0.21;
                let center_y = self.rect_size.y * 0.5;
                // Create three circle SDFs
                sdf.circle(
                    self.rect_size.x * 0.25, 
                    amplitude * sin(self.anim_time * 2.0 * PI * self.freq) + center_y, 
                    self.dot_radius
                );
                sdf.fill(self.color);
                sdf.circle(
                    self.rect_size.x * 0.5, 
                    amplitude * sin(self.anim_time * 2.0 * PI * self.freq + self.phase_offset) + center_y, 
                    self.dot_radius
                );
                sdf.fill(self.color);
                sdf.circle(
                    self.rect_size.x * 0.75, 
                    amplitude * sin(self.anim_time * 2.0 * PI * self.freq + self.phase_offset * 2) + center_y, 
                    self.dot_radius
                );
                sdf.fill(self.color);
                return sdf.result;
            }
        }

    }
}

/// Thirty updates per second are sufficient for these small typing dots.
const DOTS_UPDATE_SECONDS: f64 = 1.0 / 30.0;
const DOTS_VISIBILITY_CHECK_SECONDS: f64 = 0.25;

#[derive(Script, ScriptHook, Widget)]
pub struct BouncingDots {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] timer: Timer,
    #[rust] started_at: Option<f64>,
    #[rust] timer_due_at: f64,
    #[rust] next_visibility_check: f64,
}

impl BouncingDots {
    fn schedule(&mut self, cx: &mut Cx, now: f64) {
        self.timer = cx.start_timeout(DOTS_UPDATE_SECONDS);
        self.timer_due_at = now + DOTS_UPDATE_SECONDS;
    }

    fn is_attached(&self, cx: &Cx) -> bool {
        let Some(area) = self.view.draw_bg.draw_vars.area.valid_instance(cx) else { return false };
        let Some(pass_id) = cx.draw_lists[area.draw_list_id].draw_pass_id else { return false };
        cx.attached_draw_lists(pass_id).contains(&area.draw_list_id)
    }
}

impl Widget for BouncingDots {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.timer.is_empty() && self.timer.is_event(event).is_some() {
            self.timer = Timer::empty();
            if let Some(started_at) = self.started_at {
                let now = Cx::monotonic_now();
                // Retained draw calls can outlive a hidden tab. Stop ticking if
                // this draw list is no longer attached.
                let attached = if now >= self.next_visibility_check {
                    self.next_visibility_check = now + DOTS_VISIBILITY_CHECK_SECONDS;
                    self.is_attached(cx)
                } else {
                    true
                };
                if attached {
                    // Update the retained draw call without laying out the chat.
                    // Elapsed time keeps delayed timers from slowing the dots.
                    let time = ((now - started_at).max(0.0) % 1.0) as f32;
                    self.view.draw_bg.draw_vars.set_uniform_on_area(cx, id!(anim_time), &[time]);
                    // A one-shot timer also stops naturally if the widget is removed.
                    self.schedule(cx, now);
                }
            }
        }
        // A Dock can reattach its cached draw list without drawing the child
        // again. Probe once on navigation; hidden tabs do not keep a timer.
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
        let now = Cx::monotonic_now();
        if self.started_at.is_some()
            && (self.timer.is_empty() || now > self.timer_due_at + 0.1)
        {
            // A covered panel may not have forwarded the timeout at all.
            cx.stop_timer(self.timer);
            self.next_visibility_check = 0.0;
            self.schedule(cx, now);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}


impl BouncingDotsRef {
    /// Starts animation of the bouncing dots.
    pub fn start_animation(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            if inner.started_at.is_none() {
                let now = Cx::monotonic_now();
                inner.started_at = Some(now);
                inner.next_visibility_check = 0.0;
                inner.schedule(cx, now);
            }
        }
    }
    /// Stops animation of the bouncing dots.
    pub fn stop_animation(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            if inner.started_at.take().is_some() {
                cx.stop_timer(inner.timer);
                inner.timer = Timer::empty();
                inner.view.draw_bg.draw_vars.set_uniform_on_area(cx, id!(anim_time), &[0.0]);
            }
        }
    }
}
