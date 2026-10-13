#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod compositor;
mod controller;
mod navigation;
mod scene;
mod service;
mod shortcuts;
mod smoke;
mod surfaces;
mod webviews;
mod workspace;

use bevy::{
    camera::CameraUpdateSystems, prelude::*, transform::TransformSystems, winit::WinitSettings,
};
use navigation::{NavigationState, SpatialState};
use scene::SpatialLayout;
use service::ServiceProcess;
use std::path::PathBuf;
use workspace::{OrbitWorkspace, WorkspaceActions};

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--native-input-probe")
    {
        if let Some(parent) = arguments
            .get(index + 1)
            .and_then(|value| value.parse().ok())
        {
            if let Err(error) = smoke::run_native_input_probe(parent) {
                eprintln!("Native input probe: {error}");
                std::process::exit(1);
            }
        } else {
            std::process::exit(1);
        }
        return;
    }
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::env::set_current_dir(&directory).expect("project directory");
    let projects = arguments
        .iter()
        .position(|argument| argument == "--projects")
        .and_then(|index| arguments.get(index + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| directory.join("projects"));
    let service = match ServiceProcess::start(&directory, &projects) {
        Ok(service) => service,
        Err(error) => {
            eprintln!("Impossibile avviare i servizi locali: {error}");
            std::process::exit(1);
        }
    };
    let spatial: SpatialState = std::fs::read(scene::state_directory().join("scene.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "ORBIT / Rust 3D".into(),
                        resolution: (1440, 900).into(),
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    file_path: directory.join("services").to_string_lossy().into_owned(),
                    ..default()
                }),
        )
        .insert_resource(ClearColor(Color::srgb_u8(9, 11, 20)))
        .insert_resource(WinitSettings::game())
        .insert_resource(service)
        .insert_resource(spatial)
        .init_resource::<OrbitWorkspace>()
        .init_resource::<WorkspaceActions>()
        .init_resource::<NavigationState>()
        .init_resource::<SpatialLayout>()
        .add_plugins((
            webviews::WebviewsPlugin,
            shortcuts::ShortcutsPlugin,
            smoke::SmokePlugin,
        ))
        .add_systems(Startup, scene::setup_scene)
        .add_systems(
            Update,
            (
                controller::receive_service,
                controller::receive_web,
                shortcuts::receive_shortcuts,
                navigation::open_launcher,
                navigation::navigate,
                controller::handle_actions,
                scene::build_workspace,
                scene::update_scene,
                navigation::move_camera,
                controller::sync_shell,
                controller::save_spatial,
            )
                .chain(),
        )
        .add_systems(
            PostUpdate,
            surfaces::update_surface
                .after(TransformSystems::Propagate)
                .after(CameraUpdateSystems),
        )
        .run();
}
