//! Browser contacts use the existing FrameXML mouse dispatcher only when they begin over UI.
//! World contacts are left alone; one captured contact owns the Lua pointer until it ends.

use benilla_ui::script::UiScript;
use bevy::input::touch::{TouchInput, TouchPhase};
use bevy::prelude::*;

#[cfg(target_arch = "wasm32")]
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct TouchFeed<'w, 's> {
    events: MessageReader<'w, 's, TouchInput>,
    memo: Local<'s, super::VmMemo<TouchState>>,
    worlds: Option<Res<'w, super::SweptWorlds>>,
    primary: Query<'w, 's, Entity, With<bevy::window::PrimaryWindow>>,
    egui: Res<'w, super::EguiPointerOver>,
    buttons: Res<'w, ButtonInput<MouseButton>>,
}

#[cfg(target_arch = "wasm32")]
impl TouchFeed<'_, '_> {
    pub(super) fn clear(&mut self, script: Option<&mut UiScript>) {
        if let Some(script) = script {
            self.memo.get(script).cancel(script);
        }
        self.memo.get_for(None);
        self.events.clear();
    }

    pub(super) fn feed(
        &mut self,
        script: &mut UiScript,
        window: &Window,
        screen: super::UiScreen,
        synthetic: bool,
    ) -> TouchOutcome {
        let state = self
            .memo
            .get_in_world(script, self.worlds.as_deref().copied().unwrap_or_default());
        if state.contact.is_none() && self.events.is_empty() {
            return TouchOutcome::default();
        }
        if synthetic {
            // A probe owns the VM pointer; do not cancel or replace its gesture.
            state.contact = None;
            self.events.clear();
            return TouchOutcome::default();
        }
        let context = TouchContext {
            window: self.primary.single().ok(),
            screen,
            available: window.focused && !self.egui.0,
            mouse_busy: self.buttons.get_pressed().next().is_some()
                || self.buttons.get_just_pressed().next().is_some()
                || self.buttons.get_just_released().next().is_some(),
        };
        state.feed(script, context, self.events.read())
    }
}

#[derive(Default)]
pub(super) struct TouchOutcome {
    /// Also suppress the ordinary mouse feed on a release/cancel frame.
    pub(super) handled: bool,
    /// Keep pointer arbitration on the UI while its contact is captured, even outside its bounds.
    pub(super) owner: Option<u32>,
}

#[derive(Clone, Copy)]
struct TouchContext {
    window: Option<Entity>,
    /// The mouse feed's screen, including any 4:3 stretch across the logical window.
    screen: super::UiScreen,
    available: bool,
    mouse_busy: bool,
}

#[derive(Clone, Copy)]
struct Contact {
    id: u64,
    window: Entity,
    position: Vec2,
    owner: u32,
}

#[derive(Default)]
struct TouchState {
    contact: Option<Contact>,
}

impl TouchState {
    fn cancel(&mut self, script: &mut UiScript) -> TouchOutcome {
        let Some(contact) = self.contact.take() else {
            return TouchOutcome::default();
        };
        // The existing lost-pointer path stops drags and unpresses buttons without a click.
        script.pointer_left_hover();
        script.pointer_left_window();
        TouchOutcome {
            handled: true,
            owner: Some(contact.owner),
        }
    }

    fn feed<'a>(
        &mut self,
        script: &mut UiScript,
        context: TouchContext,
        events: impl Iterator<Item = &'a TouchInput>,
    ) -> TouchOutcome {
        if !context.available
            || context.window.is_none()
            || !context.screen.window.is_finite()
            || context.screen.window.min_element() <= 0.0
        {
            events.for_each(|_| {});
            return self.cancel(script);
        }
        let mut outcome = TouchOutcome::default();
        if self
            .contact
            .is_some_and(|contact| Some(contact.window) != context.window)
        {
            outcome = self.cancel(script);
        }
        let mut moved = false;
        for event in events {
            if Some(event.window) != context.window {
                continue;
            }
            if !event.position.is_finite() {
                if self.contact.is_some_and(|contact| contact.id == event.id) {
                    outcome = self.cancel(script);
                }
                continue;
            }
            let at = super::window_to_ui(context.screen, event.position);
            match event.phase {
                TouchPhase::Started if self.contact.is_none() && !context.mouse_busy => {
                    let inside = event.position.cmpge(Vec2::ZERO).all()
                        && event.position.cmple(context.screen.window).all();
                    if !inside {
                        continue;
                    }
                    // World contacts must not change Lua hover or run pointer handlers.
                    let Some(owner) = script
                        .hit_test(at.x, at.y)
                        .filter(|id| !script.is_world_frame(*id))
                    else {
                        continue;
                    };
                    self.contact = Some(Contact {
                        id: event.id,
                        window: event.window,
                        position: event.position,
                        owner,
                    });
                    script.mouse_move(at.x, at.y);
                    script.mouse_button(at.x, at.y, "LeftButton", true);
                    moved = true;
                    outcome = TouchOutcome {
                        handled: true,
                        owner: Some(owner),
                    };
                }
                TouchPhase::Moved | TouchPhase::Ended | TouchPhase::Canceled => {
                    let Some(contact) = self.contact.as_mut().filter(|c| c.id == event.id) else {
                        continue;
                    };
                    outcome = TouchOutcome {
                        handled: true,
                        owner: Some(contact.owner),
                    };
                    if event.phase == TouchPhase::Canceled {
                        self.cancel(script);
                        continue;
                    }
                    contact.position = event.position;
                    script.mouse_move(at.x, at.y);
                    moved = true;
                    if event.phase == TouchPhase::Ended {
                        script.mouse_button(at.x, at.y, "LeftButton", false);
                        self.cancel(script);
                    }
                }
                _ => {}
            }
        }
        if let Some(contact) = self.contact {
            if !moved {
                let at = super::window_to_ui(context.screen, contact.position);
                script.mouse_move(at.x, at.y);
            }
            outcome = TouchOutcome {
                handled: true,
                owner: Some(contact.owner),
            };
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> TouchContext {
        TouchContext {
            window: Some(Entity::PLACEHOLDER),
            screen: super::super::UiScreen::of(Vec2::new(1024.0, 768.0), true),
            available: true,
            mouse_busy: false,
        }
    }

    fn event(id: u64, phase: TouchPhase, x: f32, y: f32) -> TouchInput {
        TouchInput {
            id,
            phase,
            position: Vec2::new(x, y),
            window: Entity::PLACEHOLDER,
            force: None,
        }
    }

    fn ui() -> UiScript {
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        script
            .run(
                r#"
                clicks, downs, ups, stops, starts, worldClicks, leaves = 0, 0, 0, 0, 0, 0, 0
                WorldFrame = CreateFrame("WorldFrame", "WorldFrame")
                WorldFrame:SetAllPoints()
                WorldFrame:SetFrameStrata("BACKGROUND")
                WorldFrame:SetScript("OnClick", function() worldClicks = worldClicks + 1 end)
                TestButton = CreateFrame("Button", "TestButton")
                TestButton:SetPoint("BOTTOMLEFT", 100, 200)
                TestButton:SetWidth(100) TestButton:SetHeight(60)
                TestButton:SetFrameStrata("HIGH") TestButton:EnableMouse(true)
                TestButton:RegisterForClicks("LeftButtonUp")
                TestButton:SetScript("OnMouseDown", function()
                    downs = downs + 1
                    assert(arg1 == "LeftButton")
                end)
                TestButton:SetScript("OnMouseUp", function() ups = ups + 1 end)
                TestButton:SetScript("OnClick", function() clicks = clicks + 1 end)
                TestButton:SetScript("OnLeave", function() leaves = leaves + 1 end)
                TestButton:SetScript("OnDragStart", function() starts = starts + 1 end)
                TestButton:SetScript("OnDragStop", function() stops = stops + 1 end)
                "#,
            )
            .unwrap();
        script.resolve();
        script
    }

    fn count(script: &UiScript, name: &str) -> i64 {
        script.eval(&format!("return {name}")).unwrap()
    }

    #[test]
    fn a_fast_tap_keeps_both_edges_and_clicks_once_without_a_cursor() {
        let mut script = ui();
        let mut touch = TouchState::default();
        let events = [
            event(1, TouchPhase::Started, 150.0, 538.0),
            event(1, TouchPhase::Ended, 150.0, 538.0),
        ];
        let result = touch.feed(&mut script, context(), events.iter());
        assert!(result.handled && result.owner.is_some());
        assert_eq!(count(&script, "downs"), 1);
        assert_eq!(count(&script, "ups"), 1);
        assert_eq!(count(&script, "clicks"), 1);
        assert_eq!(count(&script, "leaves"), 1);
        assert_eq!(count(&script, "worldClicks"), 0);
        assert!(touch.contact.is_none());
        assert!(script.errors().is_empty(), "{:?}", script.errors());
    }

    #[test]
    fn logical_positions_use_the_existing_screen_unit_conversion() {
        let mut script = ui();
        let mut touch = TouchState::default();
        let mut context = context();
        context.screen = super::super::UiScreen::of(Vec2::new(2048.0, 1536.0), true);
        let events = [
            event(1, TouchPhase::Started, 300.0, 1076.0),
            event(1, TouchPhase::Ended, 300.0, 1076.0),
        ];
        touch.feed(&mut script, context, events.iter());
        assert_eq!(count(&script, "clicks"), 1);
    }

    #[test]
    fn a_stretched_four_three_ui_uses_its_screen_for_touch_hit_testing() {
        for widescreen in [false, true] {
            let mut script = ui();
            let mut touch = TouchState::default();
            let mut context = context();
            context.screen = super::super::UiScreen::of(Vec2::new(2048.0, 768.0), widescreen);
            // The 4:3 screen stretches the button's x = 150 across to logical x = 300.
            // With widescreen on, the same logical point misses the button.
            let events = [
                event(1, TouchPhase::Started, 300.0, 538.0),
                event(1, TouchPhase::Ended, 300.0, 538.0),
            ];
            let result = touch.feed(&mut script, context, events.iter());
            assert_eq!(result.handled, !widescreen);
            assert_eq!(count(&script, "clicks"), i64::from(!widescreen));
            assert_eq!(count(&script, "worldClicks"), 0);
        }
    }

    #[test]
    fn a_world_contact_does_not_hover_click_or_acquire_ui_when_it_moves_over_it() {
        let mut script = ui();
        let mut touch = TouchState::default();
        let events = [
            event(1, TouchPhase::Started, 500.0, 500.0),
            event(1, TouchPhase::Moved, 150.0, 538.0),
            event(1, TouchPhase::Ended, 150.0, 538.0),
        ];
        let result = touch.feed(&mut script, context(), events.iter());
        assert!(!result.handled && result.owner.is_none());
        assert_eq!(count(&script, "downs"), 0);
        assert_eq!(count(&script, "worldClicks"), 0);
        assert_eq!(count(&script, "leaves"), 0);
    }

    #[test]
    fn another_finger_cannot_move_or_release_the_captured_pointer() {
        let mut script = ui();
        let mut touch = TouchState::default();
        touch.feed(
            &mut script,
            context(),
            [event(1, TouchPhase::Started, 150.0, 538.0)].iter(),
        );
        let events = [
            event(2, TouchPhase::Started, 500.0, 500.0),
            event(2, TouchPhase::Moved, 500.0, 500.0),
            event(2, TouchPhase::Ended, 500.0, 500.0),
        ];
        let result = touch.feed(&mut script, context(), events.iter());
        assert!(result.handled && result.owner.is_some());
        assert_eq!(count(&script, "downs"), 1);
        assert_eq!(count(&script, "ups"), 0);
        touch.feed(
            &mut script,
            context(),
            [event(1, TouchPhase::Ended, 150.0, 538.0)].iter(),
        );
        assert_eq!(count(&script, "clicks"), 1);
    }

    #[test]
    fn a_drag_keeps_capture_outside_ui_and_cancel_stops_it_without_clicking() {
        let mut script = ui();
        script
            .run("TestButton:RegisterForDrag('LeftButton')")
            .unwrap();
        let mut touch = TouchState::default();
        let events = [
            event(1, TouchPhase::Started, 150.0, 538.0),
            event(1, TouchPhase::Moved, 500.0, 500.0),
        ];
        let result = touch.feed(&mut script, context(), events.iter());
        assert!(result.handled && result.owner.is_some());
        assert_eq!(count(&script, "starts"), 1);
        let canceled = [event(1, TouchPhase::Canceled, 500.0, 500.0)];
        assert!(touch.feed(&mut script, context(), canceled.iter()).handled);
        assert_eq!(count(&script, "stops"), 1);
        assert_eq!(count(&script, "clicks"), 0);
        assert!(touch.contact.is_none());
    }

    #[test]
    fn cancel_focus_loss_and_invalid_positions_never_complete_a_click() {
        for mode in 0..3 {
            let mut script = ui();
            let mut touch = TouchState::default();
            touch.feed(
                &mut script,
                context(),
                [event(1, TouchPhase::Started, 150.0, 538.0)].iter(),
            );
            let mut context = context();
            let events = match mode {
                0 => vec![event(1, TouchPhase::Canceled, 150.0, 538.0)],
                1 => {
                    context.available = false;
                    Vec::new()
                }
                _ => vec![event(1, TouchPhase::Ended, f32::NAN, 538.0)],
            };
            assert!(touch.feed(&mut script, context, events.iter()).handled);
            assert!(touch.contact.is_none());
            assert_eq!(count(&script, "clicks"), 0);
            // An ordinary mouse click still works after the contact was canceled.
            script.mouse_move(150.0, 230.0);
            script.mouse_button(150.0, 230.0, "LeftButton", true);
            script.mouse_button(150.0, 230.0, "LeftButton", false);
            assert_eq!(count(&script, "clicks"), 1);
        }
    }

    #[test]
    fn hiding_the_pressed_button_does_not_retarget_its_release() {
        let mut script = ui();
        let mut touch = TouchState::default();
        touch.feed(
            &mut script,
            context(),
            [event(1, TouchPhase::Started, 150.0, 538.0)].iter(),
        );
        script.run("TestButton:Hide()").unwrap();
        script.resolve();
        touch.feed(
            &mut script,
            context(),
            [event(1, TouchPhase::Ended, 150.0, 538.0)].iter(),
        );
        assert_eq!(count(&script, "clicks"), 0);
        assert_eq!(count(&script, "worldClicks"), 0);
        assert!(touch.contact.is_none());
    }

    #[test]
    fn idle_blocked_mouse_and_foreign_window_contacts_do_not_take_the_pointer() {
        let mut script = ui();
        let mut touch = TouchState::default();
        let result = touch.feed(&mut script, context(), [].iter());
        assert!(!result.handled && result.owner.is_none());
        let mut context = context();
        context.mouse_busy = true;
        let events = [event(1, TouchPhase::Started, 150.0, 538.0)];
        assert!(!touch.feed(&mut script, context, events.iter()).handled);
        context.mouse_busy = false;
        let mut foreign = events;
        foreign[0].window = World::new().spawn_empty().id();
        assert!(!touch.feed(&mut script, context, foreign.iter()).handled);
        assert_eq!(count(&script, "downs"), 0);
    }

    #[test]
    fn session_and_world_memos_do_not_carry_a_contact_into_a_new_ui() {
        let mut script = ui();
        let mut memo = super::super::VmMemo::<TouchState>::default();
        memo.get(&script).feed(
            &mut script,
            context(),
            [event(1, TouchPhase::Started, 150.0, 538.0)].iter(),
        );
        assert!(memo.get(&script).contact.is_some());
        assert!(memo
            .get_in_world(&script, super::super::SweptWorlds(1))
            .contact
            .is_none());
        let second = ui();
        assert!(memo.get(&second).contact.is_none());
    }
}
