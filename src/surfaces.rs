use crate::{
    navigation::{NavigationState, ray_panel},
    scene::{OrbitCamera, WindowPanel, planet_occludes_label},
    webviews::{WebAction, WebCommands},
    workspace::OrbitWorkspace,
};
use bevy::{prelude::*, window::PrimaryWindow};

pub const PAGE_VIEWPORT: [u32; 2] = [1280, 720];

pub fn surface_corners(
    panel: &WindowPanel,
    transform: &GlobalTransform,
    camera: &Camera,
    camera_transform: &GlobalTransform,
    window: &Window,
) -> Option<[[f32; 2]; 4]> {
    let half = Vec2::new(panel.width, panel.height) * 0.5;
    let local = [
        Vec3::new(-half.x, half.y, 0.0),
        Vec3::new(half.x, half.y, 0.0),
        Vec3::new(half.x, -half.y, 0.0),
        Vec3::new(-half.x, -half.y, 0.0),
    ];
    let mut corners = [[0.0; 2]; 4];
    for (index, point) in local.into_iter().enumerate() {
        let viewport = camera
            .world_to_viewport(camera_transform, transform.transform_point(point))
            .ok()?;
        corners[index] = (viewport * window.scale_factor()).to_array();
    }
    Some(corners)
}

pub fn surface_position(corners: [[f32; 2]; 4], cursor: Vec2, outside: bool) -> Option<[i32; 2]> {
    let [first, second, third, fourth] = corners.map(Vec2::from_array);
    let difference = first - second + third - fourth;
    let right = second - third;
    let left = fourth - third;
    let determinant = right.x * left.y - left.x * right.y;
    if determinant.abs() < 0.0001 {
        return None;
    }
    let perspective_x = (difference.x * left.y - left.x * difference.y) / determinant;
    let perspective_y = (right.x * difference.y - difference.x * right.y) / determinant;
    let horizontal = second - first + second * perspective_x;
    let vertical = fourth - first + fourth * perspective_y;
    let projection = Mat3::from_cols(
        Vec3::new(horizontal.x, horizontal.y, perspective_x),
        Vec3::new(vertical.x, vertical.y, perspective_y),
        Vec3::new(first.x, first.y, 1.0),
    );
    if projection.determinant().abs() < 0.0001 {
        return None;
    }
    let point = projection.inverse() * cursor.extend(1.0);
    let coordinates = point.truncate() / point.z;
    if !coordinates.is_finite()
        || (!outside && (coordinates.min_element() < 0.0 || coordinates.max_element() > 1.0))
    {
        return None;
    }
    Some([
        (coordinates.x * PAGE_VIEWPORT[0] as f32).round() as i32,
        (coordinates.y * PAGE_VIEWPORT[1] as f32).round() as i32,
    ])
}

pub fn update_surface(
    workspace: Res<OrbitWorkspace>,
    mut navigation: ResMut<NavigationState>,
    mut web: ResMut<WebCommands>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    panels: Query<(&WindowPanel, &GlobalTransform)>,
    mut gizmos: Gizmos,
) {
    let Some(id) = &workspace.selected else {
        if navigation.displayed.is_some() {
            hide_surface(&mut navigation, &mut web);
        }
        return;
    };
    let Some((panel, transform)) = panels.iter().find(|(panel, _)| &panel.id == id) else {
        return;
    };
    let Some(project) = workspace
        .projects
        .iter()
        .find(|project| project.id == panel.project_id)
    else {
        return;
    };
    let Some(mut corners) = surface_corners(panel, transform, camera.0, camera.1, &window) else {
        hide_surface(&mut navigation, &mut web);
        return;
    };
    let facing = (transform.rotation() * Vec3::Z)
        .dot((camera.1.translation() - transform.translation()).normalize());
    let half = Vec2::new(panel.width, panel.height) * 0.5;
    let unobstructed = [
        Vec3::ZERO,
        Vec3::new(-half.x, half.y, 0.0),
        Vec3::new(half.x, half.y, 0.0),
        Vec3::new(half.x, -half.y, 0.0),
        Vec3::new(-half.x, -half.y, 0.0),
    ]
    .into_iter()
    .all(|point| {
        let position = transform.transform_point(point);
        if planet_occludes_label(camera.1.translation(), position, &workspace) {
            return false;
        }
        let difference = position - camera.1.translation();
        let Ok(direction) = Dir3::new(difference) else {
            return false;
        };
        let ray = Ray3d::new(camera.1.translation(), direction);
        !panels.iter().any(|(other, other_transform)| {
            if other.id == panel.id {
                return false;
            }
            let shown = workspace
                .projects
                .iter()
                .find(|project| project.id == other.project_id)
                .is_some_and(|project| camera.1.translation().distance(project.center) < 43.0);
            shown
                && ray_panel(ray, other_transform, other.width, other.height)
                    .is_some_and(|distance| distance < difference.length() - 0.05)
        })
    });
    let minimum = corners
        .map(Vec2::from_array)
        .into_iter()
        .fold(Vec2::splat(f32::INFINITY), Vec2::min);
    let maximum = corners
        .map(Vec2::from_array)
        .into_iter()
        .fold(Vec2::splat(f32::NEG_INFINITY), Vec2::max);
    let intersects_viewport = maximum.x > 0.0
        && minimum.x < window.physical_width() as f32
        && maximum.y > 64.0 * window.scale_factor()
        && minimum.y < window.physical_height() as f32;
    let visible = workspace.current_project.as_ref() == Some(&panel.project_id)
        && workspace.resolved.contains_key(id)
        && (navigation.expanded_amount > 0.0
            || (facing > 0.05 && unobstructed && intersects_viewport));
    let progress = navigation.expanded_amount;
    let eased = progress * progress * (3.0 - 2.0 * progress);
    let width = window.physical_width() as f32;
    let height = window.physical_height() as f32;
    let top = 64.0 * window.scale_factor();
    let margin = 4.0 * window.scale_factor();
    let display_width = (width - margin * 2.0).min((height - top - margin * 2.0) * 16.0 / 9.0);
    let display_height = display_width * 9.0 / 16.0;
    let left = (width - display_width) * 0.5;
    let upper = top + (height - top - display_height) * 0.5;
    let expanded = [
        [left, upper],
        [left + display_width, upper],
        [left + display_width, upper + display_height],
        [left, upper + display_height],
    ];
    for index in 0..4 {
        corners[index] = Vec2::from_array(corners[index])
            .lerp(Vec2::from_array(expanded[index]), eased)
            .to_array();
    }
    navigation.surface_corners = Some(corners);
    web.actions.push(WebAction::Surface {
        id: id.clone(),
        corners,
        viewport: PAGE_VIEWPORT,
        visible,
    });
    if visible {
        if navigation.flight.is_none()
            && (!navigation.surface_visible || navigation.displayed.as_ref() != Some(id))
        {
            web.actions.push(WebAction::Focus { id: id.clone() });
            navigation.displayed = Some(id.clone());
        }
        if navigation.flight.is_some() {
            navigation.surface_visible = visible;
            return;
        }
        let center = corners.map(Vec2::from_array).into_iter().sum::<Vec2>() * 0.25;
        let outline = corners.map(|point| {
            let point = Vec2::from_array(point);
            point + (point - center).normalize_or_zero() * 2.5 * window.scale_factor()
        });
        for index in 0..4 {
            let first = camera
                .0
                .viewport_to_world(camera.1, outline[index] / window.scale_factor());
            let second = camera
                .0
                .viewport_to_world(camera.1, outline[(index + 1) % 4] / window.scale_factor());
            if let (Ok(first), Ok(second)) = (first, second) {
                gizmos.line(
                    first.origin + first.direction * 2.0,
                    second.origin + second.direction * 2.0,
                    project.color,
                );
            }
        }
    } else if navigation.surface_visible {
        web.actions.push(WebAction::Overview);
        navigation.displayed = None;
    }
    navigation.surface_visible = visible;
}

fn hide_surface(navigation: &mut NavigationState, web: &mut WebCommands) {
    if navigation.surface_visible {
        web.actions.push(WebAction::Overview);
    }
    navigation.surface_visible = false;
    navigation.surface_corners = None;
    navigation.displayed = None;
}
