use crate::{
    scene::{OrbitCamera, Planet, SpatialLayout, WindowPanel},
    surfaces::surface_position,
    webviews::{ShellInteraction, WebAction, WebCommands, WebMouseKind},
    workspace::{OrbitWorkspace, WorkspaceAction, WorkspaceActions},
};
use bevy::{
    input::{
        ButtonState,
        mouse::{AccumulatedMouseMotion, MouseWheel},
    },
    prelude::*,
    window::{PrimaryWindow, WindowEvent},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    f32::consts::{PI, TAU},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct OrbitPose {
    pub target: [f32; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look_target: Option<[f32; 3]>,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
}

impl Default for OrbitPose {
    fn default() -> Self {
        Self {
            target: [0.0; 3],
            look_target: None,
            yaw: 0.25,
            pitch: 0.32,
            distance: 21.0,
        }
    }
}

impl OrbitPose {
    pub fn transform(&self) -> Transform {
        let target = Vec3::from_array(self.target);
        let direction = Vec3::new(
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
            self.yaw.cos() * self.pitch.cos(),
        );
        Transform::from_translation(target + direction * self.distance).looking_at(
            Vec3::from_array(self.look_target.unwrap_or(self.target)),
            Vec3::Y,
        )
    }
}

pub struct CameraFlight {
    pub id: String,
    pub project_id: String,
    pub elapsed: f32,
    pub duration: f32,
    start: OrbitPose,
    destination: OrbitPose,
}

impl CameraFlight {
    pub fn new(
        id: String,
        project_id: String,
        start: OrbitPose,
        mut destination: OrbitPose,
    ) -> Self {
        destination.yaw = start.yaw + shortest_angle(destination.yaw - start.yaw);
        let duration = 0.58 + (destination.yaw - start.yaw).abs() * 0.27;
        Self {
            id,
            project_id,
            elapsed: 0.0,
            duration,
            start,
            destination,
        }
    }

    fn pose(&self, fraction: f32) -> OrbitPose {
        let interpolate = |first: [f32; 3], second: [f32; 3]| {
            Vec3::from_array(first)
                .lerp(Vec3::from_array(second), fraction)
                .to_array()
        };
        OrbitPose {
            target: interpolate(self.start.target, self.destination.target),
            look_target: Some(interpolate(
                self.start.look_target.unwrap_or(self.start.target),
                self.destination
                    .look_target
                    .unwrap_or(self.destination.target),
            )),
            yaw: self.start.yaw + (self.destination.yaw - self.start.yaw) * fraction,
            pitch: self.start.pitch + (self.destination.pitch - self.start.pitch) * fraction,
            distance: self.start.distance
                + (self.destination.distance - self.start.distance) * fraction,
        }
    }
}

pub fn shortest_angle(angle: f32) -> f32 {
    (angle + PI).rem_euclid(TAU) - PI
}

pub fn camera_pose(transform: &Transform, center: Vec3) -> OrbitPose {
    let offset = transform.translation - center;
    let distance = offset.length().max(5.0);
    OrbitPose {
        target: center.to_array(),
        look_target: Some((transform.translation + transform.forward() * distance).to_array()),
        yaw: offset.x.atan2(offset.z),
        pitch: offset.y.atan2(offset.x.hypot(offset.z)),
        distance,
    }
}

#[derive(Resource, Default, Serialize, Deserialize)]
pub struct SpatialState {
    pub camera: OrbitPose,
    pub anchors: HashMap<String, [f32; 3]>,
    pub current_project: Option<String>,
    #[serde(skip)]
    pub dirty: bool,
}

#[derive(Resource, Default)]
pub struct NavigationState {
    pub flight: Option<CameraFlight>,
    pub expanded_amount: f32,
    pub surface_corners: Option<[[f32; 2]; 4]>,
    pub surface_visible: bool,
    pub displayed: Option<String>,
    pub pointer_inside: bool,
    pub pointer_capture: bool,
    pub capture_buttons: u32,
    pub pointer_position: Option<[i32; 2]>,
    pub last_click: Option<(f32, Vec2)>,
    pub drag_distance: f32,
    pub moved_window: Option<String>,
}

pub fn open_launcher(
    mut events: MessageReader<WindowEvent>,
    mut pressed_keys: Local<ButtonInput<KeyCode>>,
    mut web: ResMut<WebCommands>,
    window: Single<Entity, With<PrimaryWindow>>,
) {
    for event in events.read() {
        match event {
            WindowEvent::KeyboardInput(input) if input.window == *window => {
                match input.state {
                    ButtonState::Pressed => pressed_keys.press(input.key_code),
                    ButtonState::Released => pressed_keys.release(input.key_code),
                }
                if input.state.is_pressed()
                    && input.key_code == KeyCode::KeyT
                    && !input.repeat
                    && pressed_keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])
                    && !pressed_keys.any_pressed([
                        KeyCode::AltLeft,
                        KeyCode::AltRight,
                        KeyCode::ShiftLeft,
                        KeyCode::ShiftRight,
                        KeyCode::SuperLeft,
                        KeyCode::SuperRight,
                    ])
                {
                    web.actions.push(WebAction::Launcher);
                }
            }
            WindowEvent::WindowFocused(focus) if focus.window == *window && !focus.focused => {
                pressed_keys.reset_all();
            }
            WindowEvent::KeyboardFocusLost(_) => pressed_keys.reset_all(),
            _ => {}
        }
    }
}

pub fn navigate(
    mut wheels: MessageReader<MouseWheel>,
    motion: Res<AccumulatedMouseMotion>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut workspace: ResMut<OrbitWorkspace>,
    mut spatial: ResMut<SpatialState>,
    mut navigation: ResMut<NavigationState>,
    mut actions: ResMut<WorkspaceActions>,
    mut web: ResMut<WebCommands>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    panels: Query<(&WindowPanel, &GlobalTransform)>,
    planets: Query<(&Planet, &GlobalTransform)>,
    mut layout: ResMut<SpatialLayout>,
    time: Res<Time>,
    shell_interaction: Res<ShellInteraction>,
) {
    let mut scroll = Vec2::ZERO;
    for wheel in wheels.read() {
        let scale = if wheel.unit == bevy::input::mouse::MouseScrollUnit::Line {
            1.0
        } else {
            0.02
        };
        scroll += Vec2::new(wheel.x, wheel.y) * scale;
    }
    if shell_interaction.overlay {
        if keys.just_pressed(KeyCode::Escape)
            || (buttons.just_released(MouseButton::Left)
                && window.cursor_position().is_some_and(|cursor| {
                    cursor.y >= 64.0
                        && !shell_interaction
                            .palette
                            .is_some_and(|palette| palette.contains(cursor))
                }))
        {
            web.actions.push(WebAction::ClosePanel);
        }
        if let Some(id) = &workspace.selected {
            let position = navigation.pointer_position.unwrap_or([0, 0]);
            for (flag, kind) in [
                (1, WebMouseKind::LeftUp),
                (2, WebMouseKind::RightUp),
                (16, WebMouseKind::MiddleUp),
            ] {
                if navigation.capture_buttons & flag != 0 {
                    web.actions.push(WebAction::Mouse {
                        id: id.clone(),
                        kind,
                        position,
                        modifiers: 0,
                        wheel_delta: 0,
                    });
                }
            }
            if navigation.pointer_inside || navigation.pointer_capture {
                web.actions.push(WebAction::Mouse {
                    id: id.clone(),
                    kind: WebMouseKind::Leave,
                    position: [0, 0],
                    modifiers: 0,
                    wheel_delta: 0,
                });
            }
        }
        navigation.pointer_inside = false;
        navigation.pointer_capture = false;
        navigation.capture_buttons = 0;
        navigation.pointer_position = None;
        navigation.moved_window = None;
        navigation.drag_distance = 0.0;
        layout.hovered = None;
        return;
    }
    let control = keys.pressed(KeyCode::ControlLeft)
        || keys.pressed(KeyCode::ControlRight)
        || unsafe {
            windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(
                windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL.0 as i32,
            ) < 0
        };
    let shift = keys.pressed(KeyCode::ShiftLeft)
        || keys.pressed(KeyCode::ShiftRight)
        || unsafe {
            windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(
                windows::Win32::UI::Input::KeyboardAndMouse::VK_SHIFT.0 as i32,
            ) < 0
        };
    if keys.just_pressed(KeyCode::Enter) && control && workspace.selected.is_some() {
        actions.0.push(if workspace.expanded {
            WorkspaceAction::Collapse
        } else {
            WorkspaceAction::Expand
        });
    }
    if keys.just_pressed(KeyCode::Escape) && workspace.expanded {
        actions.0.push(WorkspaceAction::Collapse);
    }
    if keys.just_pressed(KeyCode::Home) && workspace.selected.is_none() {
        actions.0.push(WorkspaceAction::Galaxy);
    }
    if keys.just_pressed(KeyCode::Escape) && workspace.selected.is_none() {
        actions.0.push(WorkspaceAction::Galaxy);
    }
    let Some(cursor) = window.cursor_position() else {
        if navigation.pointer_capture {
            if let Some(id) = &workspace.selected {
                let position = navigation.pointer_position.unwrap_or([0, 0]);
                for (button, flag, kind) in [
                    (MouseButton::Left, 1, WebMouseKind::LeftUp),
                    (MouseButton::Right, 2, WebMouseKind::RightUp),
                    (MouseButton::Middle, 16, WebMouseKind::MiddleUp),
                ] {
                    if navigation.capture_buttons & flag != 0 && !buttons.pressed(button) {
                        web.actions.push(WebAction::Mouse {
                            id: id.clone(),
                            kind,
                            position,
                            modifiers: 0,
                            wheel_delta: 0,
                        });
                        navigation.capture_buttons &= !flag;
                    }
                }
            }
            navigation.pointer_capture = navigation.capture_buttons != 0;
        }
        return;
    };
    let modifiers = (buttons.pressed(MouseButton::Left) as u32)
        | ((buttons.pressed(MouseButton::Right) as u32) << 1)
        | ((shift as u32) << 2)
        | ((control as u32) << 3)
        | ((buttons.pressed(MouseButton::Middle) as u32) << 4);
    let orbit_gesture = cursor.y >= 64.0
        && (scroll.length_squared() > 0.0
            || ((buttons.pressed(MouseButton::Left) || buttons.pressed(MouseButton::Right))
                && motion.delta.length_squared() > 0.0));
    if orbit_gesture && navigation.flight.is_some() {
        navigation.flight = None;
        navigation.displayed = None;
        workspace.shell_dirty = true;
    }
    let physical_cursor = cursor * window.scale_factor();
    let page_position = navigation
        .surface_corners
        .and_then(|corners| surface_position(corners, physical_cursor, false));
    let inside = navigation.flight.is_none()
        && navigation.surface_visible
        && cursor.y >= 64.0
        && page_position.is_some();
    if let Some(id) = workspace
        .selected
        .as_ref()
        .filter(|_| navigation.flight.is_none())
    {
        if inside || navigation.pointer_capture {
            let position = page_position
                .or_else(|| {
                    navigation
                        .surface_corners
                        .and_then(|corners| surface_position(corners, physical_cursor, true))
                })
                .unwrap_or([0, 0]);
            if navigation.pointer_position != Some(position) || !navigation.pointer_inside {
                web.actions.push(WebAction::Mouse {
                    id: id.clone(),
                    kind: WebMouseKind::Move,
                    position,
                    modifiers,
                    wheel_delta: 0,
                });
                navigation.pointer_position = Some(position);
            }
            navigation.pointer_inside = inside;
            for (button, flag, down, up) in [
                (
                    MouseButton::Left,
                    1,
                    WebMouseKind::LeftDown,
                    WebMouseKind::LeftUp,
                ),
                (
                    MouseButton::Right,
                    2,
                    WebMouseKind::RightDown,
                    WebMouseKind::RightUp,
                ),
                (
                    MouseButton::Middle,
                    16,
                    WebMouseKind::MiddleDown,
                    WebMouseKind::MiddleUp,
                ),
            ] {
                if buttons.just_pressed(button) {
                    web.actions.push(WebAction::Focus { id: id.clone() });
                    let double_click = button == MouseButton::Left
                        && navigation.last_click.is_some_and(|(elapsed, previous)| {
                            time.elapsed_secs() - elapsed < 0.45 && previous.distance(cursor) < 5.0
                        });
                    web.actions.push(WebAction::Mouse {
                        id: id.clone(),
                        kind: if double_click {
                            WebMouseKind::LeftDoubleClick
                        } else {
                            down
                        },
                        position,
                        modifiers,
                        wheel_delta: 0,
                    });
                    navigation.pointer_capture = true;
                    navigation.capture_buttons = modifiers & 19;
                    if button == MouseButton::Left {
                        navigation.last_click = Some((time.elapsed_secs(), cursor));
                    }
                }
                if buttons.just_released(button)
                    || (navigation.capture_buttons & flag != 0 && !buttons.pressed(button))
                {
                    web.actions.push(WebAction::Mouse {
                        id: id.clone(),
                        kind: up,
                        position,
                        modifiers,
                        wheel_delta: 0,
                    });
                    navigation.capture_buttons = modifiers & 19;
                }
            }
            if scroll.y != 0.0 {
                web.actions.push(WebAction::Mouse {
                    id: id.clone(),
                    kind: WebMouseKind::Wheel,
                    position,
                    modifiers,
                    wheel_delta: (scroll.y * 120.0) as i32,
                });
            }
            if scroll.x != 0.0 {
                web.actions.push(WebAction::Mouse {
                    id: id.clone(),
                    kind: WebMouseKind::HorizontalWheel,
                    position,
                    modifiers,
                    wheel_delta: (scroll.x * 120.0) as i32,
                });
            }
            if !buttons.pressed(MouseButton::Left)
                && !buttons.pressed(MouseButton::Right)
                && !buttons.pressed(MouseButton::Middle)
            {
                navigation.pointer_capture = false;
            }
            layout.hovered = Some(id.clone());
            return;
        }
        if navigation.pointer_inside {
            web.actions.push(WebAction::Mouse {
                id: id.clone(),
                kind: WebMouseKind::Leave,
                position: [0, 0],
                modifiers: 0,
                wheel_delta: 0,
            });
            navigation.pointer_inside = false;
            navigation.pointer_position = None;
        }
    }
    if workspace.expanded || navigation.expanded_amount > 0.0 || cursor.y < 64.0 {
        return;
    }
    let ray = camera.0.viewport_to_world(camera.1, cursor).ok();
    let hovered = ray.and_then(|ray| {
        let mut intersections: Vec<(f32, String, bool)> = Vec::new();
        for (panel, transform) in &panels {
            if !layout.visible.contains(&panel.id)
                || workspace.current_project.as_ref() != Some(&panel.project_id)
            {
                continue;
            }
            let facing = (transform.rotation() * Vec3::Z)
                .dot((camera.1.translation() - transform.translation()).normalize());
            if facing <= 0.05 {
                continue;
            }
            let frame =
                transform.mul_transform(Transform::from_translation(Vec3::new(0.0, 0.15, -0.015)));
            if let Some(distance) = ray_panel(ray, &frame, panel.width + 0.12, panel.height + 0.48)
            {
                intersections.push((distance, panel.id.clone(), false));
            }
        }
        for (planet, transform) in &planets {
            let offset = ray.origin - transform.translation();
            let along = offset.dot(*ray.direction);
            let discriminant = along * along - offset.length_squared() + 2.3 * 2.3;
            if discriminant >= 0.0 {
                let distance = -along - discriminant.sqrt();
                if distance > 0.0 {
                    intersections.push((distance, planet.id.clone(), true));
                }
            }
        }
        intersections
            .into_iter()
            .min_by(|first, second| first.0.total_cmp(&second.0))
    });
    layout.hovered = hovered.as_ref().map(|(_, id, _)| id.clone());
    if buttons.just_pressed(MouseButton::Left) {
        navigation.drag_distance = 0.0;
        navigation.moved_window = if shift {
            hovered
                .as_ref()
                .filter(|(_, _, planet)| !planet)
                .map(|(_, id, _)| id.clone())
        } else {
            None
        };
    }
    if buttons.pressed(MouseButton::Left) || buttons.pressed(MouseButton::Right) {
        navigation.drag_distance += motion.delta.length();
        if let Some(id) = &navigation.moved_window {
            if let Some(anchor) = spatial.anchors.get_mut(id) {
                anchor[0] -= motion.delta.x * 0.009;
                anchor[1] = (anchor[1] + motion.delta.y * 0.012).clamp(-4.0, 4.0);
                spatial.dirty = true;
            }
        } else {
            if motion.delta.length_squared() > 0.0 {
                spatial.camera.look_target = None;
            }
            spatial.camera.yaw -= motion.delta.x * 0.005;
            spatial.camera.pitch =
                (spatial.camera.pitch + motion.delta.y * 0.005).clamp(-1.30, 1.30);
            spatial.dirty |= motion.delta.length_squared() > 0.0;
        }
    }
    if buttons.just_released(MouseButton::Left) && navigation.drag_distance < 5.0 {
        if let Some((_, id, planet)) = hovered {
            actions.0.push(if planet {
                WorkspaceAction::Project(id)
            } else {
                WorkspaceAction::Select(id)
            });
        }
    }
    if (buttons.just_released(MouseButton::Left) || buttons.just_released(MouseButton::Right))
        && navigation.surface_visible
    {
        if let Some(id) = &workspace.selected {
            web.actions.push(WebAction::Focus { id: id.clone() });
        }
    }
    if scroll.y != 0.0 {
        spatial.camera.look_target = None;
        spatial.camera.distance =
            (spatial.camera.distance * (-scroll.y * 0.11).exp()).clamp(5.0, 140.0);
        spatial.dirty = true;
    }
    let horizontal = keys.pressed(KeyCode::ArrowRight) as u8 as f32
        - keys.pressed(KeyCode::ArrowLeft) as u8 as f32;
    let vertical =
        keys.pressed(KeyCode::ArrowUp) as u8 as f32 - keys.pressed(KeyCode::ArrowDown) as u8 as f32;
    if workspace.selected.is_none() && (horizontal != 0.0 || vertical != 0.0) {
        if navigation.flight.take().is_some() {
            workspace.shell_dirty = true;
        }
        spatial.camera.look_target = None;
        spatial.camera.yaw += horizontal * time.delta_secs();
        spatial.camera.pitch =
            (spatial.camera.pitch + vertical * time.delta_secs()).clamp(-1.30, 1.30);
        spatial.dirty = true;
    }
}

pub fn move_camera(
    mut spatial: ResMut<SpatialState>,
    mut workspace: ResMut<OrbitWorkspace>,
    mut navigation: ResMut<NavigationState>,
    mut web: ResMut<WebCommands>,
    mut camera: Single<&mut Transform, With<OrbitCamera>>,
    time: Res<Time>,
) {
    let direction = if workspace.expanded { 1.0 } else { -1.0 };
    navigation.expanded_amount =
        (navigation.expanded_amount + direction * time.delta_secs() / 0.48).clamp(0.0, 1.0);
    let Some(mut flight) = navigation.flight.take() else {
        let target = spatial.camera.transform();
        let fraction = 1.0 - (-time.delta_secs() * 9.0).exp();
        camera.translation = camera.translation.lerp(target.translation, fraction);
        camera.rotation = camera.rotation.slerp(target.rotation, fraction);
        return;
    };
    if workspace.current_project.as_ref() != Some(&flight.project_id)
        || workspace
            .tab(&flight.id)
            .is_none_or(|tab| tab.project_id != flight.project_id)
    {
        workspace.shell_dirty = true;
        return;
    }
    if navigation.expanded_amount > 0.0 {
        navigation.flight = Some(flight);
        return;
    }
    if workspace.selected.take().is_some() {
        web.actions.push(WebAction::Overview);
        navigation.surface_visible = false;
        navigation.surface_corners = None;
        navigation.displayed = None;
        workspace.shell_dirty = true;
    }
    flight.elapsed += time.delta_secs();
    let progress = (flight.elapsed / flight.duration).clamp(0.0, 1.0);
    let eased = progress * progress * (3.0 - 2.0 * progress);
    spatial.camera = flight.pose(eased);
    spatial.dirty = true;
    **camera = spatial.camera.transform();
    if progress >= 1.0 {
        workspace.selected = Some(flight.id);
        workspace.shell_dirty = true;
    } else {
        navigation.flight = Some(flight);
    }
}

pub fn ray_panel(ray: Ray3d, transform: &GlobalTransform, width: f32, height: f32) -> Option<f32> {
    let inverse = transform.affine().inverse();
    let origin = inverse.transform_point3(ray.origin);
    let direction = inverse.transform_vector3(*ray.direction);
    if direction.z.abs() < 0.00001 {
        return None;
    }
    let distance = -origin.z / direction.z;
    let intersection = origin + direction * distance;
    (distance > 0.0 && intersection.x.abs() <= width * 0.5 && intersection.y.abs() <= height * 0.5)
        .then_some(distance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::{
        input::keyboard::{Key, KeyboardFocusLost, KeyboardInput, NativeKey},
        window::WindowFocused,
    };

    fn launcher_app() -> (App, Entity) {
        let mut app = App::new();
        app.add_message::<WindowEvent>()
            .init_resource::<WebCommands>()
            .add_systems(Update, open_launcher);
        let window = app.world_mut().spawn(PrimaryWindow).id();
        (app, window)
    }

    fn send_key(
        app: &mut App,
        window: Entity,
        key_code: KeyCode,
        state: ButtonState,
        repeat: bool,
    ) {
        app.world_mut()
            .write_message(WindowEvent::KeyboardInput(KeyboardInput {
                key_code,
                logical_key: Key::Unidentified(NativeKey::Unidentified),
                state,
                text: None,
                repeat,
                window,
            }));
    }

    fn launcher_count(app: &App) -> usize {
        app.world()
            .resource::<WebCommands>()
            .actions
            .iter()
            .filter(|action| matches!(action, WebAction::Launcher))
            .count()
    }

    #[test]
    fn complete_shortcut_within_one_frame_opens_once_and_releases_modifiers() {
        for control in [KeyCode::ControlLeft, KeyCode::ControlRight] {
            let (mut app, window) = launcher_app();
            send_key(&mut app, window, control, ButtonState::Pressed, false);
            send_key(&mut app, window, KeyCode::KeyT, ButtonState::Pressed, false);
            send_key(
                &mut app,
                window,
                KeyCode::KeyT,
                ButtonState::Released,
                false,
            );
            send_key(&mut app, window, control, ButtonState::Released, false);
            app.update();
            assert_eq!(launcher_count(&app), 1);

            send_key(&mut app, window, KeyCode::KeyT, ButtonState::Pressed, false);
            send_key(
                &mut app,
                window,
                KeyCode::KeyT,
                ButtonState::Released,
                false,
            );
            app.update();
            assert_eq!(launcher_count(&app), 1);
        }
    }

    #[test]
    fn additional_modifiers_preserve_other_shortcuts() {
        for modifier in [
            KeyCode::AltLeft,
            KeyCode::AltRight,
            KeyCode::ShiftLeft,
            KeyCode::ShiftRight,
            KeyCode::SuperLeft,
            KeyCode::SuperRight,
        ] {
            let (mut app, window) = launcher_app();
            send_key(
                &mut app,
                window,
                KeyCode::ControlLeft,
                ButtonState::Pressed,
                false,
            );
            send_key(&mut app, window, modifier, ButtonState::Pressed, false);
            send_key(&mut app, window, KeyCode::KeyT, ButtonState::Pressed, false);
            send_key(
                &mut app,
                window,
                KeyCode::KeyT,
                ButtonState::Released,
                false,
            );
            send_key(&mut app, window, modifier, ButtonState::Released, false);
            send_key(
                &mut app,
                window,
                KeyCode::ControlLeft,
                ButtonState::Released,
                false,
            );
            app.update();
            assert_eq!(launcher_count(&app), 0);
        }
    }

    #[test]
    fn key_repeats_do_not_reopen_launcher_and_a_new_press_does() {
        let (mut app, window) = launcher_app();
        send_key(
            &mut app,
            window,
            KeyCode::ControlLeft,
            ButtonState::Pressed,
            false,
        );
        send_key(&mut app, window, KeyCode::KeyT, ButtonState::Pressed, false);
        send_key(&mut app, window, KeyCode::KeyT, ButtonState::Pressed, true);
        app.update();
        send_key(&mut app, window, KeyCode::KeyT, ButtonState::Pressed, true);
        app.update();
        assert_eq!(launcher_count(&app), 1);

        send_key(
            &mut app,
            window,
            KeyCode::KeyT,
            ButtonState::Released,
            false,
        );
        send_key(&mut app, window, KeyCode::KeyT, ButtonState::Pressed, false);
        app.update();
        assert_eq!(launcher_count(&app), 2);
    }

    #[test]
    fn focus_loss_resets_modifiers_in_event_order() {
        let (mut app, window) = launcher_app();
        send_key(
            &mut app,
            window,
            KeyCode::ControlLeft,
            ButtonState::Pressed,
            false,
        );
        app.world_mut()
            .write_message(WindowEvent::WindowFocused(WindowFocused {
                window,
                focused: false,
            }));
        send_key(&mut app, window, KeyCode::KeyT, ButtonState::Pressed, false);
        app.update();
        assert_eq!(launcher_count(&app), 0);

        send_key(
            &mut app,
            window,
            KeyCode::ControlRight,
            ButtonState::Pressed,
            false,
        );
        app.world_mut()
            .write_message(WindowEvent::KeyboardFocusLost(KeyboardFocusLost));
        send_key(&mut app, window, KeyCode::KeyT, ButtonState::Pressed, false);
        app.update();
        assert_eq!(launcher_count(&app), 0);

        app.world_mut()
            .write_message(WindowEvent::WindowFocused(WindowFocused {
                window,
                focused: true,
            }));
        send_key(
            &mut app,
            window,
            KeyCode::ControlLeft,
            ButtonState::Pressed,
            false,
        );
        send_key(&mut app, window, KeyCode::KeyT, ButtonState::Pressed, false);
        app.update();
        assert_eq!(launcher_count(&app), 1);
    }
}
