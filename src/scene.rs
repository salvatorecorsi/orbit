use crate::{
    navigation::{NavigationState, SpatialState},
    workspace::OrbitWorkspace,
};
use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    window::PrimaryWindow,
};
use std::{
    collections::{HashMap, HashSet},
    f32::consts::{PI, TAU},
    path::Path,
};

#[derive(Component)]
pub struct OrbitCamera;

#[derive(Component)]
pub struct Planet {
    pub id: String,
}

#[derive(Component)]
pub struct WindowPanel {
    pub id: String,
    pub project_id: String,
    pub width: f32,
    pub height: f32,
}

#[derive(Component)]
pub struct WindowFrame {
    id: String,
}

#[derive(Component)]
pub struct SceneObject;

#[derive(Component)]
pub struct WorldLabel {
    id: String,
    project: bool,
}

#[derive(Resource, Default)]
pub struct SpatialLayout {
    revision: u64,
    pub transforms: HashMap<String, Transform>,
    pub materials: HashMap<String, Handle<StandardMaterial>>,
    pub previews: HashMap<String, Handle<Image>>,
    pub visible: HashSet<String>,
    pub hovered: Option<String>,
}

pub fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    spatial: Res<SpatialState>,
    assets: Res<AssetServer>,
) {
    commands.spawn((
        Camera3d::default(),
        OrbitCamera,
        spatial.camera.transform(),
        Msaa::Sample4,
    ));
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.62, 0.72, 0.98),
        brightness: 180.0,
        ..default()
    });
    commands.spawn((
        DirectionalLight {
            illuminance: 8500.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(-10.0, 15.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    let star_mesh = meshes.add(Sphere::new(1.0).mesh().uv(5, 4));
    let star_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.63, 0.70, 0.88),
        unlit: true,
        ..default()
    });
    let mut random = 83_u64;
    for _ in 0..950 {
        let mut next = || {
            random = random
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((random >> 33) as f32) / (u32::MAX as f32 * 0.5)
        };
        let longitude = next() * TAU;
        let latitude = (next() * 2.0 - 1.0).asin();
        let radius = 150.0 + next() * 170.0;
        let size = 0.025 + next().powi(6) * 0.22;
        let position = Vec3::new(
            latitude.cos() * longitude.cos(),
            latitude.sin(),
            latitude.cos() * longitude.sin(),
        ) * radius;
        commands.spawn((
            Mesh3d(star_mesh.clone()),
            MeshMaterial3d(star_material.clone()),
            Transform::from_translation(position).with_scale(Vec3::splat(size)),
        ));
    }
    commands.spawn((Node { position_type: PositionType::Absolute, bottom: px(24), left: px(26), ..default() }, Text::new("Trascina per orbitare   /   Alt + Tab per cambiare finestra   /   Clic per attivare   /   Ctrl + Invio per ingrandire   /   Shift + trascina per spostare"), TextFont { font: assets.load("fonts/space-grotesk.ttf"), font_size: 13.0, ..default() }, TextColor(Color::srgb(0.52, 0.54, 0.64))));
}

pub fn build_workspace(
    mut commands: Commands,
    workspace: Res<OrbitWorkspace>,
    mut spatial: ResMut<SpatialState>,
    mut layout: ResMut<SpatialLayout>,
    objects: Query<Entity, With<SceneObject>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    assets: Res<AssetServer>,
) {
    if layout.revision == workspace.revision || workspace.revision == 0 {
        return;
    }
    for entity in &objects {
        commands.entity(entity).despawn();
    }
    layout.transforms.clear();
    layout.materials.clear();
    layout.revision = workspace.revision;
    let font = assets.load("fonts/space-grotesk.ttf");
    for (project_index, project) in workspace.projects.iter().enumerate() {
        let texture = images.add(planet_image(project_index));
        let surface = materials.add(StandardMaterial {
            base_color_texture: Some(texture),
            perceptual_roughness: 0.78,
            metallic: 0.06,
            ..default()
        });
        commands.spawn((
            SceneObject,
            Planet {
                id: project.id.clone(),
            },
            Mesh3d(meshes.add(Sphere::new(2.3).mesh().uv(64, 32))),
            MeshMaterial3d(surface),
            Transform::from_translation(project.center),
        ));
        let halo = materials.add(StandardMaterial {
            base_color: project.color.with_alpha(0.055),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            cull_mode: None,
            ..default()
        });
        commands.spawn((
            SceneObject,
            Mesh3d(meshes.add(Sphere::new(2.42).mesh().uv(40, 20))),
            MeshMaterial3d(halo),
            Transform::from_translation(project.center),
        ));
        spawn_label(
            &mut commands,
            &project.id,
            true,
            &project.title,
            font.clone(),
            project.color,
        );
        let project_tabs: Vec<_> = workspace
            .tabs
            .iter()
            .filter(|tab| tab.project_id == project.id)
            .collect();
        for (index, tab) in project_tabs.iter().enumerate() {
            let anchor = spatial.anchors.entry(tab.id.clone()).or_insert_with(|| {
                [
                    index as f32 * TAU / project_tabs.len().max(4) as f32 + 0.12,
                    if index % 2 == 0 { 0.45 } else { -0.45 },
                    6.2,
                ]
            });
            let transform = panel_transform(project.center, *anchor);
            let width = 5.1;
            let height = width * 9.0 / 16.0;
            let frame = materials.add(StandardMaterial {
                base_color: Color::srgb(0.10, 0.12, 0.19),
                unlit: true,
                cull_mode: None,
                ..default()
            });
            commands.spawn((
                SceneObject,
                WindowFrame { id: tab.id.clone() },
                Mesh3d(meshes.add(Rectangle::new(width + 0.12, height + 0.48))),
                MeshMaterial3d(frame),
                transform,
            ));
            let preview = layout.previews.get(&tab.id).cloned().or_else(|| {
                let path = preview_path(&tab.id);
                let bytes = std::fs::read(path).ok()?;
                let image = decode_preview(&bytes)?;
                Some(images.add(image))
            });
            if let Some(preview) = &preview {
                layout.previews.insert(tab.id.clone(), preview.clone());
            }
            let material = materials.add(StandardMaterial {
                base_color: if preview.is_some() {
                    Color::WHITE
                } else {
                    Color::srgb(0.07, 0.08, 0.13)
                },
                base_color_texture: preview,
                unlit: true,
                cull_mode: None,
                ..default()
            });
            let normal = transform.rotation * Vec3::Z;
            let content_transform = Transform {
                translation: transform.translation + normal * 0.015
                    - transform.rotation * Vec3::Y * 0.15,
                ..transform
            };
            commands.spawn((
                SceneObject,
                WindowPanel {
                    id: tab.id.clone(),
                    project_id: project.id.clone(),
                    width,
                    height,
                },
                Mesh3d(meshes.add(Rectangle::new(width, height))),
                MeshMaterial3d(material.clone()),
                content_transform,
            ));
            layout.transforms.insert(tab.id.clone(), content_transform);
            layout.materials.insert(tab.id.clone(), material);
            let title = format!(
                "{}  /  {}",
                kind_label(&tab.kind),
                truncate_title(&tab.title, 46)
            );
            spawn_label(
                &mut commands,
                &tab.id,
                false,
                &title,
                font.clone(),
                Color::srgb(0.87, 0.88, 0.93),
            );
        }
    }
    spatial.dirty = true;
}

fn spawn_label(
    commands: &mut Commands,
    id: &str,
    project: bool,
    title: &str,
    font: Handle<Font>,
    color: Color,
) {
    commands.spawn((
        SceneObject,
        WorldLabel {
            id: id.into(),
            project,
        },
        Node {
            position_type: PositionType::Absolute,
            padding: UiRect::axes(px(9.0), px(5.0)),
            border_radius: BorderRadius::all(px(4.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.035, 0.042, 0.07, 0.90)),
        Text::new(title),
        TextFont {
            font,
            font_size: 17.0,
            ..default()
        },
        TextColor(color),
    ));
}

pub fn update_scene(
    workspace: Res<OrbitWorkspace>,
    spatial: Res<SpatialState>,
    navigation: Res<NavigationState>,
    mut layout: ResMut<SpatialLayout>,
    camera: Single<(&Camera, &GlobalTransform), With<OrbitCamera>>,
    mut panels: Query<(&WindowPanel, &mut Transform, &mut Visibility), Without<WindowFrame>>,
    mut frames: Query<
        (&WindowFrame, &mut Transform, &mut Visibility),
        (Without<WindowPanel>, Without<WorldLabel>),
    >,
    mut labels: Query<
        (
            &WorldLabel,
            &mut Node,
            &mut TextFont,
            &mut TextColor,
            &mut Visibility,
        ),
        (Without<WindowPanel>, Without<WindowFrame>),
    >,
    window: Single<&Window, With<PrimaryWindow>>,
    mut gizmos: Gizmos,
) {
    layout.visible.clear();
    for project in &workspace.projects {
        let distance = camera.1.translation().distance(project.center);
        if distance < 48.0 && !workspace.expanded {
            gizmos.circle(
                Isometry3d::new(project.center, Quat::from_rotation_x(PI * 0.5)),
                6.2,
                project.color.with_alpha(0.18),
            );
            gizmos.circle(
                Isometry3d::new(project.center, Quat::from_rotation_x(PI * 0.5)),
                2.9,
                project.color.with_alpha(0.12),
            );
        }
    }
    for (panel, mut transform, mut visibility) in &mut panels {
        let Some(project) = workspace
            .projects
            .iter()
            .find(|project| project.id == panel.project_id)
        else {
            continue;
        };
        let Some(anchor) = spatial.anchors.get(&panel.id) else {
            continue;
        };
        let base = panel_transform(project.center, *anchor);
        transform.translation =
            base.translation + base.rotation * Vec3::Z * 0.015 - base.rotation * Vec3::Y * 0.15;
        transform.rotation = base.rotation;
        layout.transforms.insert(panel.id.clone(), *transform);
        let show = camera.1.translation().distance(project.center) < 43.0;
        let active_surface =
            workspace.selected.as_ref() == Some(&panel.id) && navigation.surface_visible;
        *visibility = if show && !active_surface {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if show {
            layout.visible.insert(panel.id.clone());
            let tint = if layout.hovered.as_ref() == Some(&panel.id)
                || workspace.selected.as_ref() == Some(&panel.id)
            {
                project.color.with_alpha(0.68)
            } else {
                project.color.with_alpha(0.11)
            };
            gizmos.line(project.center, transform.translation, tint);
            if layout.hovered.as_ref() == Some(&panel.id)
                && workspace.selected.as_ref() != Some(&panel.id)
            {
                let half = Vec2::new(panel.width, panel.height) * 0.5;
                let corners = [
                    Vec3::new(-half.x, -half.y, 0.04),
                    Vec3::new(half.x, -half.y, 0.04),
                    Vec3::new(half.x, half.y, 0.04),
                    Vec3::new(-half.x, half.y, 0.04),
                ];
                for index in 0..4 {
                    gizmos.line(
                        transform.transform_point(corners[index]),
                        transform.transform_point(corners[(index + 1) % 4]),
                        tint,
                    );
                }
            }
        }
    }
    for (frame, mut transform, mut visibility) in &mut frames {
        if let Some(content) = layout.transforms.get(&frame.id) {
            *transform = Transform {
                translation: content.translation - content.rotation * Vec3::Z * 0.015
                    + content.rotation * Vec3::Y * 0.15,
                ..*content
            };
        }
        let moving_surface =
            workspace.selected.as_ref() == Some(&frame.id) && navigation.expanded_amount > 0.0;
        *visibility = if layout.visible.contains(&frame.id) && !moving_surface {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    for (label, mut node, mut font, mut text_color, mut visibility) in &mut labels {
        let position = if label.project {
            workspace
                .projects
                .iter()
                .find(|project| project.id == label.id)
                .map(|project| project.center + Vec3::Y * 3.15)
        } else {
            layout
                .transforms
                .get(&label.id)
                .map(|transform| transform.translation + transform.rotation * Vec3::Y * 1.70)
        };
        let Some(position) = position else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let distance = camera.1.translation().distance(position);
        let Ok(viewport) = camera.0.world_to_viewport(camera.1, position) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let front_facing = label.project
            || layout.transforms.get(&label.id).is_some_and(|transform| {
                let direction = (camera.1.translation() - transform.translation).normalize();
                (transform.rotation * Vec3::Z).dot(direction) > 0.25
            });
        let visible = navigation.expanded_amount == 0.0
            && front_facing
            && !planet_occludes_label(camera.1.translation(), position, &workspace)
            && viewport.x > 0.0
            && viewport.x < window.width()
            && viewport.y > 72.0
            && viewport.y < window.height() - 38.0
            && (label.project || layout.visible.contains(&label.id));
        *visibility = if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        node.left = px(viewport.x - if label.project { 70.0 } else { 95.0 });
        node.top = px(viewport.y);
        font.font_size = if label.project {
            (520.0 / distance).clamp(7.0, 23.0)
        } else {
            (260.0 / distance).clamp(9.0, 15.0)
        };
        text_color.0 = text_color
            .0
            .with_alpha(if layout.hovered.as_ref() == Some(&label.id) {
                1.0
            } else {
                0.85
            });
    }
    if let Some(links) = workspace.document["links"].as_array() {
        for link in links {
            let from = link["from"].as_str().unwrap_or_default();
            let to = link["to"].as_str().unwrap_or_default();
            if layout.visible.contains(from) && layout.visible.contains(to) {
                if let (Some(first), Some(second)) =
                    (layout.transforms.get(from), layout.transforms.get(to))
                {
                    gizmos.line(
                        first.translation,
                        second.translation,
                        Color::srgba(0.66, 0.64, 0.85, 0.30),
                    );
                }
            }
        }
    }
}

pub fn planet_occludes_label(camera: Vec3, label: Vec3, workspace: &OrbitWorkspace) -> bool {
    let direction = label - camera;
    let distance = direction.length();
    let direction = direction / distance;
    workspace.projects.iter().any(|project| {
        let offset = camera - project.center;
        let along = offset.dot(direction);
        let discriminant = along * along - offset.length_squared() + 2.42_f32.powi(2);
        if discriminant < 0.0 {
            return false;
        }
        let intersection = -along - discriminant.sqrt();
        intersection > 0.0 && intersection < distance - 0.05
    })
}

pub fn preview_path(id: &str) -> std::path::PathBuf {
    let hash = id.bytes().fold(14695981039346656037_u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(1099511628211)
    });
    state_directory()
        .join("previews")
        .join(format!("{hash:016x}.png"))
}

pub fn state_directory() -> std::path::PathBuf {
    std::env::var_os("ORBIT_RUST_STATE_DIRECTORY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(".runtime").to_path_buf())
}

pub fn decode_preview(bytes: &[u8]) -> Option<Image> {
    let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = decoded.dimensions();
    Some(Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        decoded.into_raw(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    ))
}

pub fn panel_transform(center: Vec3, anchor: [f32; 3]) -> Transform {
    let offset = Vec3::new(
        anchor[0].sin() * anchor[2],
        anchor[1],
        anchor[0].cos() * anchor[2],
    );
    Transform::from_translation(center + offset).looking_at(center + Vec3::Y * anchor[1], Vec3::Y)
}

fn kind_label(kind: &str) -> &str {
    match kind {
        "terminal" => "TERMINALE",
        "code" => "VS CODE",
        "chat" => "CHAT",
        "figma" => "FIGMA",
        "pdf" => "PDF",
        "image" => "IMMAGINE",
        "file" => "FILE",
        _ => "BROWSER",
    }
}

fn truncate_title(title: &str, limit: usize) -> String {
    let shortened: String = title.chars().take(limit).collect();
    if title.chars().count() > limit {
        format!("{shortened}…")
    } else {
        shortened
    }
}

fn planet_image(index: usize) -> Image {
    let width = 512_u32;
    let height = 256_u32;
    let palettes = [
        ([20.0, 46.0, 77.0], [70.0, 145.0, 161.0]),
        ([37.0, 28.0, 70.0], [159.0, 104.0, 164.0]),
        ([77.0, 38.0, 28.0], [203.0, 142.0, 88.0]),
        ([18.0, 50.0, 50.0], [100.0, 169.0, 105.0]),
    ];
    let (sea, land) = palettes[index % palettes.len()];
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let latitude = row as f32 / height as f32 * PI;
        for column in 0..width {
            let longitude = column as f32 / width as f32 * TAU;
            let position = Vec3::new(
                latitude.sin() * longitude.cos(),
                latitude.cos(),
                latitude.sin() * longitude.sin(),
            );
            let offset = index as f32 * 7.4;
            let terrain = (position.x * 6.0 + (position.z * 4.8 + offset).sin() * 1.6).sin()
                * (position.y * 5.0 + (position.x * 3.0).cos()).cos()
                + (position.z * 16.0 + position.y * 13.0).sin() * 0.17;
            let mix = ((terrain - 0.04) * 3.0).clamp(0.0, 1.0);
            let clouds = ((position.x * 19.0 + (position.y * 12.0).sin() * 5.0).sin()
                + (position.z * 21.0).cos()
                - 1.45)
                .max(0.0)
                * 0.28;
            for channel in 0..3 {
                let surface = sea[channel] * (1.0 - mix) + land[channel] * mix;
                pixels.push((surface * (1.0 - clouds) + 220.0 * clouds).clamp(0.0, 255.0) as u8);
            }
            pixels.push(255);
        }
    }
    Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}
