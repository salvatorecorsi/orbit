use crate::{
    navigation::{CameraFlight, NavigationState, OrbitPose, SpatialState, camera_pose},
    scene::{OrbitCamera, SpatialLayout, decode_preview, panel_transform, preview_path},
    service::{PendingRequest, ServiceProcess},
    smoke::SmokeResults,
    webviews::{WebAction, WebCommands, WebEvent, WebEvents},
    workspace::{OrbitWorkspace, WorkspaceAction, WorkspaceActions},
};
use bevy::{prelude::*, window::PrimaryWindow};
use serde_json::{Value, json};
use std::f32::consts::{FRAC_PI_4, TAU};

pub fn receive_service(
    mut service: ResMut<ServiceProcess>,
    mut workspace: ResMut<OrbitWorkspace>,
    mut web: ResMut<WebCommands>,
    mut actions: ResMut<WorkspaceActions>,
    mut spatial: ResMut<SpatialState>,
) {
    for message in service.drain() {
        match message["type"].as_str().unwrap_or_default() {
            "ready" => {
                workspace.origin = message["origin"].as_str().unwrap_or_default().into();
                workspace.token = message["token"].as_str().unwrap_or_default().into();
                workspace.apply(message["workspace"].clone());
                if let Some(id) = &spatial.current_project {
                    if workspace.projects.iter().any(|project| &project.id == id) {
                        workspace.current_project = Some(id.clone());
                    }
                }
                workspace.status = "Servizi pronti".into();
                web.actions.push(WebAction::InitShell {
                    url: format!("{}/shell.html?token={}", workspace.origin, workspace.token),
                });
                let verification = std::env::args()
                    .any(|argument| matches!(argument.as_str(), "--smoke" | "--capture"));
                if !verification {
                    for tab in workspace.tabs.clone() {
                        if service
                            .request(
                                "resolveTab",
                                json!({"id":tab.id,"tab":tab.document}),
                                PendingRequest::Resolve(tab.id.clone()),
                            )
                            .is_ok()
                        {
                            workspace.resolving.push(tab.id);
                        }
                    }
                }
            }
            "event" => match message["event"].as_str().unwrap_or_default() {
                "workspace" => workspace.apply(message["workspace"].clone()),
                "focus" => {
                    if let Some(id) = message["id"].as_str() {
                        actions.0.push(WorkspaceAction::Select(id.into()));
                    }
                }
                _ => {}
            },
            "response" => {
                let id = message["id"].as_u64().unwrap_or_default();
                let purpose = service.pending.remove(&id);
                if let Some(error) = message["error"].as_str() {
                    workspace.status = format!("Errore: {error}");
                    if let Some(PendingRequest::Resolve(tab_id)) = purpose {
                        workspace.resolving.retain(|id| id != &tab_id);
                    }
                    workspace.shell_dirty = true;
                    continue;
                }
                let result = &message["result"];
                match purpose {
                    Some(PendingRequest::Resolve(tab_id)) => {
                        workspace.resolving.retain(|id| id != &tab_id);
                        if let Some(url) = result["url"].as_str() {
                            if workspace.resolved.contains_key(&tab_id) {
                                web.actions.push(WebAction::Navigate {
                                    id: tab_id.clone(),
                                    url: url.into(),
                                });
                            } else {
                                web.actions.push(WebAction::Open {
                                    id: tab_id.clone(),
                                    url: url.into(),
                                });
                                if workspace.selected.as_ref() != Some(&tab_id) {
                                    web.actions.push(WebAction::Warm { id: tab_id.clone() });
                                }
                            }
                            workspace.resolved.insert(tab_id, url.into());
                            workspace.status = "Pagina pronta".into();
                        }
                    }
                    Some(PendingRequest::Create) => {
                        if let Some(document) = result.get("workspace") {
                            workspace.apply(document.clone());
                        }
                        let id = result["id"]
                            .as_str()
                            .or_else(|| result["tab"]["id"].as_str());
                        if let Some(id) = id {
                            if let Some(tab) = workspace.tab(id) {
                                if workspace.current_project.as_ref() != Some(&tab.project_id) {
                                    actions
                                        .0
                                        .push(WorkspaceAction::Project(tab.project_id.clone()));
                                }
                            }
                            actions.0.push(WorkspaceAction::Select(id.into()));
                        }
                    }
                    Some(PendingRequest::CreateProject) => {
                        if let Some(id) = result["id"].as_str() {
                            actions.0.push(WorkspaceAction::Project(id.into()));
                        }
                    }
                    _ => {}
                }
                workspace.shell_dirty = true;
            }
            "stopped" => {
                workspace.status = "Errore: il servizio locale si è arrestato".into();
                workspace.shell_dirty = true;
            }
            "error" => {
                workspace.status = format!(
                    "Errore: {}",
                    message["message"].as_str().unwrap_or("servizio locale")
                );
                workspace.shell_dirty = true;
            }
            _ => {}
        }
    }
    if spatial.current_project.is_none() {
        spatial.current_project = workspace.current_project.clone();
    }
}

pub fn receive_web(
    mut events: ResMut<WebEvents>,
    mut workspace: ResMut<OrbitWorkspace>,
    mut actions: ResMut<WorkspaceActions>,
    mut layout: ResMut<SpatialLayout>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut service: ResMut<ServiceProcess>,
    mut results: ResMut<SmokeResults>,
) {
    for event in std::mem::take(&mut events.events) {
        match event {
            WebEvent::Shell(message) => {
                let id = message["id"].as_str().unwrap_or_default().to_owned();
                match message["action"].as_str().unwrap_or_default() {
                    "select" | "focus" => actions.0.push(WorkspaceAction::Select(id)),
                    "expand" => actions.0.push(WorkspaceAction::Expand),
                    "collapse" | "orbit" => actions.0.push(WorkspaceAction::Collapse),
                    "deselect" => actions.0.push(WorkspaceAction::Deselect),
                    "project" => actions.0.push(WorkspaceAction::Project(id)),
                    "home" => actions.0.push(WorkspaceAction::Galaxy),
                    "new" => actions.0.push(WorkspaceAction::Create(message)),
                    "newProject" => if let Some(title) = message["title"].as_str() { actions.0.push(WorkspaceAction::CreateProject(title.into())); },
                    "close" => actions.0.push(WorkspaceAction::Close(id)),
                    "navigate" => if let Some(url) = message["url"].as_str() { actions.0.push(WorkspaceAction::Navigate(url.into())); },
                    "back" => actions.0.push(WorkspaceAction::Back),
                    "forward" => actions.0.push(WorkspaceAction::Forward),
                    "popup" => actions.0.push(WorkspaceAction::Create(json!({"kind":"browser","url":message["url"],"projectId":workspace.current_project}))),
                    _ => {}
                }
            }
            WebEvent::Snapshot { id, bytes } => {
                if let Some(image) = decode_preview(&bytes) {
                    let handle = images.add(image);
                    if let Some(material) = layout
                        .materials
                        .get(&id)
                        .and_then(|handle| materials.get_mut(handle))
                    {
                        material.base_color_texture = Some(handle.clone());
                        material.base_color = Color::WHITE;
                    }
                    layout.previews.insert(id.clone(), handle);
                    let path = preview_path(&id);
                    let _ = std::fs::create_dir_all(path.parent().unwrap());
                    let _ = std::fs::write(path, &bytes);
                    results.snapshots.insert(id);
                }
            }
            WebEvent::Title { id, title } => {
                if let Some(tab) = workspace.tabs.iter_mut().find(|tab| tab.id == id) {
                    if !title.is_empty() {
                        tab.title = title;
                        workspace.shell_dirty = true;
                    }
                }
            }
            WebEvent::Navigation { id, url } | WebEvent::PageLoaded { id, url } => {
                if let Some(tab) = workspace.tabs.iter_mut().find(|tab| tab.id == id) {
                    if matches!(tab.kind.as_str(), "browser" | "figma") {
                        tab.url = url.clone();
                        let _ = service.request(
                            "capturePage",
                            json!({"id":id,"url":url,"title":tab.title,"text":""}),
                            PendingRequest::Other,
                        );
                    }
                    workspace.shell_dirty = true;
                }
            }
            WebEvent::Inspection { id, result } => {
                if let Some(tab) = workspace.tab(&id) {
                    if matches!(tab.kind.as_str(), "browser" | "figma") {
                        if let Some(text) = result["bodyExcerpt"].as_str() {
                            let _ = service.request("capturePage", json!({"id":id,"url":result["url"],"title":result["title"],"text":text}), PendingRequest::Other);
                        }
                    }
                }
                results.inspections.insert(id, result);
            }
            WebEvent::Error { id, message } => {
                workspace.status = format!("Errore: {message}");
                workspace.shell_dirty = true;
                results.errors.push(format!("{id}: {message}"));
                eprintln!("WebView {id}: {message}");
            }
        }
    }
}

pub fn handle_actions(
    mut actions: ResMut<WorkspaceActions>,
    mut workspace: ResMut<OrbitWorkspace>,
    mut spatial: ResMut<SpatialState>,
    mut navigation: ResMut<NavigationState>,
    mut web: ResMut<WebCommands>,
    mut service: ResMut<ServiceProcess>,
    camera: Single<(&Transform, &Projection), With<OrbitCamera>>,
    window: Single<&Window, With<PrimaryWindow>>,
) {
    for action in std::mem::take(&mut actions.0) {
        match action {
            WorkspaceAction::Select(id) => {
                let Some(tab) = workspace.tab(&id).cloned() else {
                    continue;
                };
                if workspace.current_project.as_ref() != Some(&tab.project_id) {
                    continue;
                }
                navigation.flight = None;
                navigation.displayed = None;
                if workspace.selected.as_ref() == Some(&id) {
                    if navigation.surface_visible {
                        web.actions.push(WebAction::Focus { id });
                    }
                    continue;
                }
                deselect(&mut workspace, &mut navigation, &mut web);
                workspace.selected = Some(id.clone());
                if !workspace.resolved.contains_key(&id) && !workspace.resolving.contains(&id) {
                    workspace.status = format!("Apertura di {}…", tab.title);
                    match service.request(
                        "resolveTab",
                        json!({"id":id,"tab":tab.document}),
                        PendingRequest::Resolve(id.clone()),
                    ) {
                        Ok(_) => workspace.resolving.push(id),
                        Err(error) => workspace.status = format!("Errore: {error}"),
                    }
                }
                workspace.shell_dirty = true;
            }
            WorkspaceAction::Cycle(direction) => {
                if direction == 0 {
                    continue;
                }
                let Some(project) = workspace
                    .current_project
                    .as_ref()
                    .and_then(|id| workspace.projects.iter().find(|project| &project.id == id))
                    .cloned()
                else {
                    continue;
                };
                let tabs: Vec<_> = workspace
                    .tabs
                    .iter()
                    .filter(|tab| tab.project_id == project.id)
                    .collect();
                let mut ordered: Vec<_> = tabs
                    .iter()
                    .enumerate()
                    .map(|(index, tab)| {
                        let anchor = spatial.anchors.get(&tab.id).copied().unwrap_or([
                            index as f32 * TAU / tabs.len().max(4) as f32 + 0.12,
                            if index % 2 == 0 { 0.45 } else { -0.45 },
                            6.2,
                        ]);
                        (tab.id.clone(), anchor)
                    })
                    .collect();
                ordered.sort_by(|first, second| {
                    first.1[0]
                        .rem_euclid(TAU)
                        .total_cmp(&second.1[0].rem_euclid(TAU))
                        .then(first.0.cmp(&second.0))
                });
                if ordered.is_empty() {
                    continue;
                }
                let reference = navigation
                    .flight
                    .as_ref()
                    .map(|flight| &flight.id)
                    .or(workspace.selected.as_ref());
                let start = camera_pose(camera.0, project.center);
                let index = if let Some(index) =
                    reference.and_then(|id| ordered.iter().position(|entry| &entry.0 == id))
                {
                    (index as i32 + direction.signum()).rem_euclid(ordered.len() as i32) as usize
                } else if direction > 0 {
                    ordered
                        .iter()
                        .position(|entry| {
                            entry.1[0].rem_euclid(TAU) > start.yaw.rem_euclid(TAU) + 0.001
                        })
                        .unwrap_or(0)
                } else {
                    ordered
                        .iter()
                        .rposition(|entry| {
                            entry.1[0].rem_euclid(TAU) < start.yaw.rem_euclid(TAU) - 0.001
                        })
                        .unwrap_or(ordered.len() - 1)
                };
                let (id, anchor) = &ordered[index];
                let destination = frame_tab(project.center, *anchor, &window, camera.1);
                if let Some(previous) = &workspace.selected {
                    web.actions.push(WebAction::Inspect {
                        id: previous.clone(),
                    });
                    let position = navigation.pointer_position.unwrap_or([0, 0]);
                    for (flag, kind) in [
                        (1, crate::webviews::WebMouseKind::LeftUp),
                        (2, crate::webviews::WebMouseKind::RightUp),
                        (16, crate::webviews::WebMouseKind::MiddleUp),
                    ] {
                        if navigation.capture_buttons & flag != 0 {
                            web.actions.push(WebAction::Mouse {
                                id: previous.clone(),
                                kind,
                                position,
                                modifiers: 0,
                                wheel_delta: 0,
                            });
                        }
                    }
                }
                navigation.pointer_inside = false;
                navigation.pointer_capture = false;
                navigation.capture_buttons = 0;
                navigation.pointer_position = None;
                navigation.last_click = None;
                navigation.moved_window = None;
                workspace.expanded = false;
                web.actions.push(WebAction::ClosePanel);
                web.actions.push(WebAction::Blur);
                if navigation.expanded_amount == 0.0 {
                    workspace.selected = None;
                    navigation.surface_visible = false;
                    navigation.surface_corners = None;
                    navigation.displayed = None;
                    web.actions.push(WebAction::Overview);
                }
                navigation.flight = Some(CameraFlight::new(
                    id.clone(),
                    project.id,
                    start.clone(),
                    destination,
                ));
                spatial.camera = start;
                if let Some(tab) = workspace.tab(id).cloned() {
                    if !workspace.resolved.contains_key(id) && !workspace.resolving.contains(id) {
                        workspace.status = format!("Apertura di {}…", tab.title);
                        match service.request(
                            "resolveTab",
                            json!({"id":id,"tab":tab.document}),
                            PendingRequest::Resolve(id.clone()),
                        ) {
                            Ok(_) => workspace.resolving.push(id.clone()),
                            Err(error) => workspace.status = format!("Errore: {error}"),
                        }
                    }
                }
            }
            WorkspaceAction::Expand => {
                if workspace.selected.is_some()
                    && navigation.surface_visible
                    && navigation.flight.is_none()
                {
                    workspace.expanded = true;
                }
            }
            WorkspaceAction::Collapse => workspace.expanded = false,
            WorkspaceAction::Deselect => deselect(&mut workspace, &mut navigation, &mut web),
            WorkspaceAction::Project(id) => {
                deselect(&mut workspace, &mut navigation, &mut web);
                if let Some(project) = workspace.projects.iter().find(|project| project.id == id) {
                    spatial.camera.target = project.center.to_array();
                    spatial.camera.look_target = None;
                    spatial.camera.distance = 21.0;
                    spatial.camera.pitch = 0.32;
                    spatial.dirty = true;
                    spatial.current_project = Some(id.clone());
                    workspace.current_project = Some(id);
                }
            }
            WorkspaceAction::Galaxy => {
                deselect(&mut workspace, &mut navigation, &mut web);
                let count = workspace.projects.len().max(1) as f32;
                let center = workspace
                    .projects
                    .iter()
                    .map(|project| project.center)
                    .sum::<Vec3>()
                    / count;
                let radius = workspace
                    .projects
                    .iter()
                    .map(|project| project.center.distance(center))
                    .fold(0.0_f32, f32::max);
                spatial.camera.target = center.to_array();
                spatial.camera.look_target = None;
                spatial.camera.distance = (radius * 2.8 + 20.0).max(32.0);
                spatial.camera.pitch = 0.5;
                spatial.dirty = true;
                workspace.current_project = None;
                spatial.current_project = None;
            }
            WorkspaceAction::Create(mut parameters) => {
                if parameters["projectId"].as_str().is_none_or(str::is_empty) {
                    parameters["projectId"] = workspace
                        .current_project
                        .clone()
                        .or_else(|| workspace.projects.first().map(|project| project.id.clone()))
                        .map(Value::String)
                        .unwrap_or(Value::Null);
                }
                if let Err(error) = service.request("createTab", parameters, PendingRequest::Create)
                {
                    workspace.status = format!("Errore: {error}");
                }
                workspace.shell_dirty = true;
            }
            WorkspaceAction::CreateProject(title) => {
                if let Err(error) = service.request(
                    "project",
                    json!({"title":title}),
                    PendingRequest::CreateProject,
                ) {
                    workspace.status = format!("Errore: {error}");
                }
            }
            WorkspaceAction::Close(id) => {
                if navigation
                    .flight
                    .as_ref()
                    .is_some_and(|flight| flight.id == id)
                {
                    navigation.flight = None;
                    navigation.displayed = None;
                }
                if workspace.selected.as_ref() == Some(&id) {
                    deselect(&mut workspace, &mut navigation, &mut web);
                }
                web.actions.push(WebAction::Close { id: id.clone() });
                let _ = service.request("closeTab", json!({"id":id}), PendingRequest::Other);
                workspace.resolved.remove(&id);
            }
            WorkspaceAction::Navigate(url) => {
                if let Some(id) = &workspace.selected {
                    let _ = service.request(
                        "navigate",
                        json!({"id":id,"url":url}),
                        PendingRequest::Resolve(id.clone()),
                    );
                }
            }
            WorkspaceAction::Back => web.actions.push(WebAction::Back),
            WorkspaceAction::Forward => web.actions.push(WebAction::Forward),
        }
        workspace.shell_dirty = true;
    }
}

fn deselect(
    workspace: &mut OrbitWorkspace,
    navigation: &mut NavigationState,
    web: &mut WebCommands,
) {
    navigation.flight = None;
    if let Some(id) = &workspace.selected {
        web.actions.push(WebAction::Inspect { id: id.clone() });
    }
    workspace.selected = None;
    workspace.expanded = false;
    navigation.displayed = None;
    navigation.expanded_amount = 0.0;
    navigation.surface_corners = None;
    navigation.surface_visible = false;
    navigation.pointer_inside = false;
    navigation.pointer_capture = false;
    navigation.capture_buttons = 0;
    navigation.pointer_position = None;
    navigation.last_click = None;
    web.actions.push(WebAction::Overview);
}

fn frame_tab(
    center: Vec3,
    anchor: [f32; 3],
    window: &Window,
    projection: &Projection,
) -> OrbitPose {
    let frame = panel_transform(center, anchor);
    let position =
        frame.translation + frame.rotation * Vec3::Z * 0.015 - frame.rotation * Vec3::Y * 0.15;
    let vertical_field = match projection {
        Projection::Perspective(projection) => projection.fov,
        _ => FRAC_PI_4,
    };
    let tangent = (vertical_field * 0.5).tan();
    let aspect = window.width() / window.height().max(1.0);
    let usable_height = ((window.height() - 64.0) / window.height()).clamp(0.2, 1.0);
    let distance = (3.35 / (2.0 * tangent * usable_height * 0.68))
        .max(5.1 / (2.0 * tangent * aspect * 0.68))
        .max(5.8);
    let eye = position
        + frame.rotation * Vec3::Z * distance
        + frame.rotation * Vec3::X * 1.1
        + Vec3::Y * 0.7;
    let transform = Transform::from_translation(eye).looking_at(position, Vec3::Y);
    let offset = transform.rotation
        * Vec3::Y
        * (64.0 / window.height().max(1.0))
        * eye.distance(position)
        * tangent;
    let mut pose = camera_pose(&transform, center);
    pose.look_target = Some((position + offset).to_array());
    pose
}

pub fn sync_shell(
    mut workspace: ResMut<OrbitWorkspace>,
    navigation: Res<NavigationState>,
    mut web: ResMut<WebCommands>,
    mut previous_visibility: Local<Option<bool>>,
) {
    if (workspace.shell_dirty || *previous_visibility != Some(navigation.surface_visible))
        && !workspace.origin.is_empty()
    {
        let mut state = workspace.shell_state();
        state["selectedVisible"] =
            Value::Bool(navigation.surface_visible && navigation.flight.is_none());
        state["traveling"] = Value::Bool(navigation.flight.is_some());
        web.actions.push(WebAction::ShellState(state));
        workspace.shell_dirty = false;
        *previous_visibility = Some(navigation.surface_visible);
    }
}

pub fn save_spatial(
    mut spatial: ResMut<SpatialState>,
    time: Res<Time>,
    mut previous_save: Local<f32>,
) {
    if !spatial.dirty || time.elapsed_secs() - *previous_save < 0.65 {
        return;
    }
    *previous_save = time.elapsed_secs();
    if let Ok(document) = serde_json::to_vec_pretty(&*spatial) {
        let directory = crate::scene::state_directory();
        let _ = std::fs::create_dir_all(&directory);
        if std::fs::write(directory.join("scene.pending"), document).is_ok()
            && std::fs::rename(
                directory.join("scene.pending"),
                directory.join("scene.json"),
            )
            .is_ok()
        {
            spatial.dirty = false;
        }
    }
}
