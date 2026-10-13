use crate::{
    navigation::{NavigationState, OrbitPose, SpatialState},
    scene::{OrbitCamera, SpatialLayout, WindowPanel, preview_path},
    service::{PendingRequest, ServiceProcess},
    shortcuts::NativeShortcuts,
    surfaces::{PAGE_VIEWPORT, surface_position},
    webviews::{WebAction, WebCommands, WebMouseKind},
    workspace::{OrbitWorkspace, WorkspaceAction, WorkspaceActions},
};
use bevy::{
    app::AppExit,
    ecs::system::NonSendMarker,
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
    window::PrimaryWindow,
    winit::WINIT_WINDOWS,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, ClientToScreen,
        CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
        GetDIBits, GetMonitorInfoW, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO,
        MonitorFromWindow, ReleaseDC, SRCCOPY, SelectObject,
    },
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
            KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
            MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput,
            VIRTUAL_KEY, VK_MENU, VK_RETURN, VK_SHIFT, VK_TAB,
        },
        WindowsAndMessaging::{
            GA_ROOT, GetAncestor, GetClientRect, GetForegroundWindow, GetSystemMetrics,
            GetWindowThreadProcessId, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
            SM_YVIRTUALSCREEN, SetForegroundWindow,
        },
    },
};
use wry::raw_window_handle::RawWindowHandle;

#[derive(Resource, Default)]
pub struct SmokeResults {
    pub inspections: HashMap<String, Value>,
    pub snapshots: HashSet<String>,
    pub errors: Vec<String>,
}

struct NativeInputProbe {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Drop for NativeInputProbe {
    fn drop(&mut self) {
        drop(self.input.take());
        for _ in 0..10 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn launch_native_input_probe() -> Result<NativeInputProbe, String> {
    use std::os::windows::process::CommandExt;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut child = Command::new(executable)
        .args(["--native-input-probe", &std::process::id().to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|error| error.to_string())?;
    let input = child.stdin.take().expect("native input probe stdin");
    let output = child.stdout.take().expect("native input probe stdout");
    eprintln!(
        "QA external input probe started: parent={}, child={}",
        std::process::id(),
        child.id()
    );
    Ok(NativeInputProbe {
        child,
        input: Some(input),
        output: BufReader::new(output),
    })
}

fn request_native_keys(
    probe: &mut NativeInputProbe,
    hwnd: HWND,
    keys: &[(VIRTUAL_KEY, bool)],
) -> Result<(), String> {
    let request = json!({
        "hwnd":hwnd.0 as usize,
        "keys":keys.iter().map(|(key,release)| (key.0,*release)).collect::<Vec<_>>()
    });
    let input = probe
        .input
        .as_mut()
        .ok_or_else(|| "Native input probe chiuso".to_owned())?;
    writeln!(input, "{request}").map_err(|error| error.to_string())?;
    input.flush().map_err(|error| error.to_string())?;
    let mut reply = String::new();
    if probe
        .output
        .read_line(&mut reply)
        .map_err(|error| error.to_string())?
        == 0
    {
        return Err("Native input probe terminato".into());
    }
    let reply: Value = serde_json::from_str(&reply).map_err(|error| error.to_string())?;
    if let Some(error) = reply["error"].as_str() {
        return Err(error.into());
    }
    if reply["sent"].as_u64() != Some(keys.len() as u64) {
        return Err("Native input probe: eventi incompleti".into());
    }
    Ok(())
}

pub fn run_native_input_probe(parent: u32) -> std::io::Result<()> {
    let input = std::io::stdin();
    let mut output = std::io::stdout().lock();
    for line in input.lock().lines() {
        let line = line?;
        let result = (|| -> Result<usize, String> {
            let request: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
            let handle = request["hwnd"]
                .as_u64()
                .ok_or_else(|| "Handle QA non valido".to_owned())?;
            let hwnd = HWND(handle as usize as *mut _);
            let mut owner = 0;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
            if owner != parent || parent == 0 {
                return Err("Native input probe: finestra di un altro processo".into());
            }
            if unsafe { GetAncestor(GetForegroundWindow(), GA_ROOT) } != hwnd {
                return Err("Native input probe: ORBIT non in primo piano".into());
            }
            let requested = request["keys"]
                .as_array()
                .ok_or_else(|| "Tasti QA non validi".to_owned())?;
            if requested.is_empty() || requested.len() > 16 {
                return Err("Sequenza QA non valida".into());
            }
            let keys = requested
                .iter()
                .map(|entry| {
                    let key = entry[0]
                        .as_u64()
                        .filter(|key| *key <= u16::MAX as u64)
                        .ok_or_else(|| "Tasto QA non valido".to_owned())?;
                    let release = entry[1]
                        .as_bool()
                        .ok_or_else(|| "Stato tasto QA non valido".to_owned())?;
                    Ok((VIRTUAL_KEY(key as u16), release))
                })
                .collect::<Result<Vec<_>, String>>()?;
            send_native_key_events(hwnd, &keys)?;
            Ok(keys.len())
        })();
        let reply = match result {
            Ok(sent) => json!({"sent":sent}),
            Err(error) => json!({"sent":0,"error":error}),
        };
        writeln!(output, "{reply}")?;
        output.flush()?;
    }
    Ok(())
}

pub struct SmokePlugin;

impl Plugin for SmokePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SmokeResults>();
        let arguments: Vec<String> = std::env::args().collect();
        let full = arguments.iter().any(|argument| argument == "--smoke");
        if full || arguments.iter().any(|argument| argument == "--capture") {
            app.insert_resource(SmokeRun::new(full))
                .add_systems(Update, run_smoke);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Stage {
    Workspace,
    Project,
    ProjectCapture,
    Orbit,
    OrbitCapture,
    Galaxy,
    GalaxyCapture,
    BrowserCreated,
    BrowserProject,
    BrowserReady,
    BrowserInputPrepared,
    BrowserPointerMove,
    BrowserPointerDown,
    BrowserPointerUp,
    BrowserChanged,
    BrowserExpanded,
    BrowserCollapsed,
    BrowserOrbit,
    BrowserDeselected,
    BrowserHidden,
    BrowserReactivated,
    BrowserProjectChanged,
    BrowserInactive,
    BrowserReturned,
    BrowserRestored,
    BrowserAuthPrepared,
    BrowserAuthPopup,
    BrowserAuthRestored,
    CycleCreated,
    CycleReady,
    CycleExpanded,
    CycleInputPrepared,
    CycleModifierProbe,
    CycleCollapse,
    CycleForward,
    CycleForwardArrival,
    CycleDestinationPrepared,
    CycleDestinationInputReady,
    CycleDestinationTyped,
    CycleReverse,
    CycleReverseArrival,
    CycleRapidStarted,
    CycleRapidRetargeted,
    CycleRapidArrival,
    CycleCancelStarted,
    CycleCancelled,
    CycleRestored,
    BrowserCapture,
    Application,
    ApplicationProject,
    ApplicationReady,
    TerminalType,
    TerminalOutput,
    ApplicationCapture,
    LoadedProject,
    LoadedProjectCapture,
    Cleanup,
    Finished,
}

#[derive(Resource)]
struct SmokeRun {
    full: bool,
    simulated_cycle: bool,
    stage: Stage,
    started: Instant,
    entered: Instant,
    next_probe: Instant,
    directory: PathBuf,
    screenshots: Vec<PathBuf>,
    checks: Vec<Value>,
    anchors: HashMap<String, [f32; 3]>,
    transforms: HashMap<String, Transform>,
    browser: Option<String>,
    existing_tabs: HashSet<String>,
    popup_tabs: HashSet<String>,
    marker: String,
    auth_marker: String,
    terminal_marker: String,
    camera: OrbitPose,
    viewpoint: Transform,
    counter_position: [i32; 2],
    auth_position: [i32; 2],
    pointer_client: [i32; 2],
    pointer_down: bool,
    native_entity: Option<Entity>,
    browser_project: Option<String>,
    application_index: usize,
    application: Option<String>,
    creation_requested: bool,
    cycle_existing_tabs: HashSet<String>,
    cycle_tabs: Vec<String>,
    cycle_anchors: HashMap<String, [f32; 3]>,
    cycle_camera: OrbitPose,
    cycle_viewpoint: Transform,
    cycle_departure_viewpoint: Transform,
    cycle_min_radius: f32,
    cycle_transit_samples: usize,
    cycle_foreground: bool,
    cycle_unexpected_selection: bool,
    cycle_retarget_viewpoint: Vec3,
    cycle_native_marker: String,
    input_probe: Option<NativeInputProbe>,
    timeout_state: Value,
}

impl SmokeRun {
    fn new(full: bool) -> Self {
        let now = Instant::now();
        Self {
            full,
            simulated_cycle: full && std::env::var("ORBIT_QA_SIMULATE_CYCLE").as_deref() == Ok("1"),
            stage: Stage::Workspace,
            started: now,
            entered: now,
            next_probe: now,
            directory: PathBuf::from(".runtime/qa").join(std::process::id().to_string()),
            screenshots: Vec::new(),
            checks: Vec::new(),
            anchors: HashMap::new(),
            transforms: HashMap::new(),
            browser: None,
            existing_tabs: HashSet::new(),
            popup_tabs: HashSet::new(),
            marker: format!("orbit-rust-smoke-{}", std::process::id()),
            auth_marker: format!("ORBIT_AUTH_SMOKE_{}", std::process::id()),
            terminal_marker: format!("ORBIT_NATIVE_SMOKE_{}", std::process::id()),
            camera: OrbitPose::default(),
            viewpoint: Transform::default(),
            counter_position: [0, 0],
            auth_position: [0, 0],
            pointer_client: [0, 0],
            pointer_down: false,
            native_entity: None,
            browser_project: None,
            application_index: 0,
            application: None,
            creation_requested: false,
            cycle_existing_tabs: HashSet::new(),
            cycle_tabs: Vec::new(),
            cycle_anchors: HashMap::new(),
            cycle_camera: OrbitPose::default(),
            cycle_viewpoint: Transform::default(),
            cycle_departure_viewpoint: Transform::default(),
            cycle_min_radius: f32::INFINITY,
            cycle_transit_samples: 0,
            cycle_foreground: true,
            cycle_unexpected_selection: false,
            cycle_retarget_viewpoint: Vec3::ZERO,
            cycle_native_marker: format!("ORBIT_CYCLE_NATIVE_{}", std::process::id()),
            input_probe: None,
            timeout_state: Value::Null,
        }
    }

    fn enter(&mut self, stage: Stage) {
        self.stage = stage;
        self.entered = Instant::now();
        self.next_probe = self.entered;
        eprintln!("QA: {stage:?}");
    }

    fn elapsed(&self, seconds: u64) -> bool {
        self.entered.elapsed() >= Duration::from_secs(seconds)
    }

    fn check(&mut self, name: &str, passed: bool, details: Value) {
        self.checks
            .push(json!({"name":name,"passed":passed,"details":details}));
    }

    fn check_cycle(&mut self, name: &str, passed: bool, details: Value) {
        let name = if self.simulated_cycle {
            format!(
                "simulated_{}",
                name.trim_start_matches("native_")
                    .replace("alt_shift_tab", "reverse_cycle")
                    .replace("alt_tab", "cycle")
            )
        } else {
            name.into()
        };
        self.check(&name, passed, details);
    }

    fn screenshot(&mut self, commands: &mut Commands, name: &str) {
        let path = self.directory.join(format!("{name}.png"));
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path.clone()));
        self.screenshots.push(path);
    }

    fn screenshot_ready(&self) -> bool {
        self.screenshots
            .last()
            .is_some_and(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 1024))
    }

    fn inspect(&mut self, id: &str, commands: &mut WebCommands) {
        if Instant::now() >= self.next_probe {
            commands.actions.push(WebAction::Inspect { id: id.into() });
            self.next_probe = Instant::now() + Duration::from_secs(1);
        }
    }

    fn kind(&self) -> Option<&'static str> {
        ["terminal", "chat", "code"]
            .get(self.application_index)
            .copied()
    }
}

fn run_smoke(
    mut commands: Commands,
    mut run: ResMut<SmokeRun>,
    mut results: ResMut<SmokeResults>,
    workspace: Res<OrbitWorkspace>,
    mut actions: ResMut<WorkspaceActions>,
    mut web: ResMut<WebCommands>,
    mut spatial: ResMut<SpatialState>,
    layout: Res<SpatialLayout>,
    navigation: Res<NavigationState>,
    services: (Res<NativeShortcuts>, Res<ServiceProcess>),
    camera: Single<(&mut Transform, &Camera, &GlobalTransform), With<OrbitCamera>>,
    window: Single<(Entity, &Window), With<PrimaryWindow>>,
    panels: Query<(&WindowPanel, &GlobalTransform)>,
    buttons: Res<ButtonInput<MouseButton>>,
    _main_thread: NonSendMarker,
    mut exit: MessageWriter<AppExit>,
) {
    let (shortcuts, service) = services;
    let (mut viewpoint, render_camera, camera_global) = camera.into_inner();
    let (window_entity, primary_window) = *window;
    run.native_entity = Some(window_entity);
    if matches!(run.stage, Stage::Finished) {
        return;
    }
    if navigation.flight.is_some() {
        if let Some(project) = workspace
            .projects
            .iter()
            .find(|project| Some(&project.id) == run.browser_project.as_ref())
        {
            run.cycle_min_radius = run
                .cycle_min_radius
                .min(viewpoint.translation.distance(project.center));
            run.cycle_transit_samples += 1;
        }
        run.cycle_foreground &= native_window_handle(window_entity)
            .is_ok_and(|hwnd| unsafe { GetForegroundWindow() } == hwnd);
        run.cycle_unexpected_selection |=
            navigation.expanded_amount <= 0.001 && workspace.selected.is_some();
    }
    let stage_timeout = match (run.stage, run.kind()) {
        (Stage::ApplicationReady, Some("code")) => 120,
        (Stage::ApplicationReady, Some("chat")) => 60,
        (Stage::ApplicationReady, Some("terminal")) => 45,
        _ => 30,
    };
    if run.started.elapsed() > Duration::from_secs(360) || run.elapsed(stage_timeout) {
        run.timeout_state = json!({
            "stage": format!("{:?}",run.stage),
            "selected": workspace.selected,
            "currentProject": workspace.current_project,
            "expanded": workspace.expanded,
            "expandedAmount": navigation.expanded_amount,
            "surfaceVisible": navigation.surface_visible,
            "flight": navigation.flight.as_ref().map(|flight| json!({"id":flight.id,"projectId":flight.project_id,"elapsed":flight.elapsed,"duration":flight.duration})),
            "expectedCycleTabs": run.cycle_tabs,
            "shortcutReady": shortcuts.ready(),
            "shortcutCallbackCount": shortcuts.callback_count(),
            "shortcutCallbackStage": shortcuts.callback_stage(),
            "shortcutThreadAlive": shortcuts.thread_alive(),
            "externalInputPid": run.input_probe.as_ref().map(|probe| probe.child.id()),
            "ownForeground": native_window_handle(window_entity).is_ok_and(|hwnd| unsafe {GetForegroundWindow()} == hwnd),
            "pendingCreations": service.pending.values().filter(|request| matches!(request,PendingRequest::Create)).count(),
            "resolvingCycleTabs": workspace.resolving.iter().filter(|id| run.cycle_tabs.contains(id)).collect::<Vec<_>>()
        });
        eprintln!("QA timeout state: {}", run.timeout_state);
        let reason = if matches!(run.stage, Stage::CycleCreated) {
            let new_tabs: Vec<_> = workspace
                .tabs
                .iter()
                .filter(|tab| !run.cycle_existing_tabs.contains(&tab.id))
                .collect();
            let scene_ready = new_tabs
                .iter()
                .filter(|tab| layout.transforms.contains_key(&tab.id))
                .count();
            format!(
                "Timeout nella verifica {:?}: nuove finestre={}, scena pronta={}, hook pronto={}",
                run.stage,
                new_tabs.len(),
                scene_ready,
                shortcuts.ready()
            )
        } else {
            format!("Timeout nella verifica {:?}", run.stage)
        };
        results.errors.push(reason);
        finish(&mut run, &results, &mut exit);
        return;
    }
    match run.stage {
        Stage::Workspace => {
            if workspace.revision == 0
                || workspace.projects.is_empty()
                || layout.transforms.is_empty()
            {
                return;
            }
            if let Err(error) = std::fs::create_dir_all(&run.directory) {
                results.errors.push(error.to_string());
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.check(
                "workspace_loaded",
                true,
                json!({"projects":workspace.projects.len(),"windows":workspace.tabs.len()}),
            );
            actions.0.push(WorkspaceAction::Deselect);
            actions
                .0
                .push(WorkspaceAction::Project(workspace.projects[0].id.clone()));
            run.enter(Stage::Project);
        }
        Stage::Project => {
            if !run.elapsed(2) || workspace.selected.is_some() {
                return;
            }
            run.anchors = spatial.anchors.clone();
            run.transforms = layout.transforms.clone();
            run.viewpoint = *viewpoint;
            run.screenshot(&mut commands, "project");
            run.enter(Stage::ProjectCapture);
        }
        Stage::ProjectCapture => {
            if !run.screenshot_ready() {
                return;
            }
            spatial.camera.yaw += 1.0;
            spatial.dirty = true;
            run.enter(Stage::Orbit);
        }
        Stage::Orbit => {
            if !run.elapsed(2) {
                return;
            }
            let anchors_stable = run.anchors == spatial.anchors;
            let transforms_stable = same_transforms(&run.transforms, &layout.transforms);
            let camera_moved = run.viewpoint.translation.distance(viewpoint.translation) > 0.01;
            run.check("orbit_preserves_window_positions", anchors_stable && transforms_stable, json!({"anchors":anchors_stable,"transforms":transforms_stable,"windows":layout.transforms.len()}));
            let camera_comparison = json!({"before":run.viewpoint.translation.to_array(),"after":viewpoint.translation.to_array()});
            run.check("orbit_moves_render_camera", camera_moved, camera_comparison);
            run.screenshot(&mut commands, "orbit");
            run.enter(Stage::OrbitCapture);
        }
        Stage::OrbitCapture => {
            if !run.screenshot_ready() {
                return;
            }
            actions.0.push(WorkspaceAction::Galaxy);
            run.enter(Stage::Galaxy);
        }
        Stage::Galaxy => {
            if !run.elapsed(2) {
                return;
            }
            run.screenshot(&mut commands, "galaxy");
            run.enter(Stage::GalaxyCapture);
        }
        Stage::GalaxyCapture => {
            if !run.screenshot_ready() {
                return;
            }
            let screenshot_paths = json!(run.screenshots);
            run.check("scene_screenshots_saved", true, screenshot_paths);
            if !run.full {
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.existing_tabs = workspace.tabs.iter().map(|tab| tab.id.clone()).collect();
            actions.0.push(WorkspaceAction::Create(json!({"kind":"browser","url":format!("{}/content/qa.html?token={}",workspace.origin,workspace.token),"projectId":workspace.current_project})));
            run.enter(Stage::BrowserCreated);
        }
        Stage::BrowserCreated => {
            let Some(tab) = workspace
                .tabs
                .iter()
                .find(|tab| tab.kind == "browser" && !run.existing_tabs.contains(&tab.id))
            else {
                return;
            };
            let id = tab.id.clone();
            run.browser = Some(id.clone());
            run.browser_project = Some(tab.project_id.clone());
            results.inspections.remove(&id);
            actions
                .0
                .push(WorkspaceAction::Project(tab.project_id.clone()));
            run.enter(Stage::BrowserProject);
        }
        Stage::BrowserProject => {
            if !run.elapsed(1) || workspace.current_project != run.browser_project {
                return;
            }
            let Some(id) = run.browser.clone() else {
                return;
            };
            position_test_surface(&id, &mut spatial);
            *viewpoint = spatial.camera.transform();
            run.camera = spatial.camera.clone();
            run.viewpoint = *viewpoint;
            actions.0.push(WorkspaceAction::Select(id));
            run.enter(Stage::BrowserReady);
        }
        Stage::BrowserReady => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if !run.elapsed(1)
                || workspace.selected.as_ref() != Some(&id)
                || workspace.expanded
                || !navigation.surface_visible
                || !view_ready(inspection)
                || inspection["hasQa"] != true
                || inspection["nativeSurface"]["visible"] != true
            {
                return;
            }
            run.check("browser_real_dom_loaded", true, inspection.clone());
            let preserved = same_pose(&run.camera, &spatial.camera)
                && run.viewpoint.translation.distance(viewpoint.translation) < 0.0001;
            let camera_details = json!({"before":run.camera,"after":spatial.camera});
            run.check(
                "selection_preserves_orbit_camera",
                preserved,
                camera_details,
            );
            let Some(corners) = navigation.surface_corners else {
                return;
            };
            let inclined = (corners[0][1] - corners[1][1]).abs() > 1.0
                || (corners[0][0] - corners[3][0]).abs() > 1.0;
            run.check("selected_surface_remains_inclined", inclined, json!({"corners":corners,"viewport":inspection["viewport"],"expanded":workspace.expanded}));
            let Some(position) = element_center(inspection, "qaCounterRect") else {
                return;
            };
            run.counter_position = position;
            let projected = projected_page_position(
                &id,
                position,
                &panels,
                render_camera,
                camera_global,
                primary_window,
            );
            let recovered = projected.and_then(|point| surface_position(corners, point, false));
            let projection_matches = recovered.is_some_and(|actual| {
                (actual[0] - position[0]).abs() <= 1 && (actual[1] - position[1]).abs() <= 1
            });
            run.check("inclined_pointer_projection_matches_real_mesh", projection_matches, json!({"pagePosition":position,"clientPosition":projected.map(|point| point.to_array()),"recovered":recovered}));
            let path = run.directory.join("inclined-live-client.png");
            match capture_native_client(window_entity, &path) {
                Ok(()) => {
                    run.screenshots.push(path.clone());
                    run.check("inclined_live_client_screenshot", true, json!(path));
                }
                Err(error) => {
                    results.errors.push(error);
                    finish(&mut run, &results, &mut exit);
                    return;
                }
            }
            let marker = serde_json::to_string(&run.marker).unwrap_or_default();
            web.actions.push(WebAction::Evaluate { id: id.clone(), script: format!("(() => {{ try {{ window.orbitSmokeMarker = {marker}; const input = document.querySelector('#qa-input'); input.value = 'ORBIT SMOKE INPUT'; input.dispatchEvent(new Event('input', {{ bubbles: true }})); return {{ ok:true, smokeInputPrepared:true }}; }} catch (error) {{ return {{ ok:false, error:String(error) }}; }} }})()") });
            results.inspections.remove(&id);
            run.enter(Stage::BrowserInputPrepared);
        }
        Stage::BrowserInputPrepared => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if inspection["smokeInputPrepared"] != true {
                return;
            }
            let Some(position) = projected_page_position(
                &id,
                run.counter_position,
                &panels,
                render_camera,
                camera_global,
                primary_window,
            ) else {
                return;
            };
            run.pointer_client = [position.x.round() as i32, position.y.round() as i32];
            results.inspections.remove(&id);
            run.enter(Stage::BrowserPointerMove);
        }
        Stage::BrowserPointerMove => {
            if let Err(error) =
                send_native_pointer(window_entity, run.pointer_client, MOUSEEVENTF_MOVE)
            {
                results.errors.push(error);
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.enter(Stage::BrowserPointerDown);
        }
        Stage::BrowserPointerDown => {
            let Some(position) = primary_window.cursor_position() else {
                return;
            };
            let expected = Vec2::new(run.pointer_client[0] as f32, run.pointer_client[1] as f32);
            if !navigation.pointer_inside
                || (position * primary_window.scale_factor()).distance(expected) > 3.0
            {
                return;
            }
            if let Err(error) =
                send_native_pointer(window_entity, run.pointer_client, MOUSEEVENTF_LEFTDOWN)
            {
                results.errors.push(error);
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.pointer_down = true;
            run.enter(Stage::BrowserPointerUp);
        }
        Stage::BrowserPointerUp => {
            if !buttons.pressed(MouseButton::Left) || !navigation.pointer_capture {
                return;
            }
            if let Err(error) =
                send_native_pointer(window_entity, run.pointer_client, MOUSEEVENTF_LEFTUP)
            {
                results.errors.push(error);
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.pointer_down = false;
            let details = json!({"clientPosition":run.pointer_client,"browserPosition":navigation.pointer_position,"pointerInside":navigation.pointer_inside,"captured":navigation.pointer_capture});
            run.check(
                "os_pointer_reaches_winit_and_inclined_navigation",
                true,
                details,
            );
            run.enter(Stage::BrowserChanged);
        }
        Stage::BrowserChanged => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if !browser_state_matches(inspection, &run.marker) {
                return;
            }
            run.check("browser_dom_input_and_counter", true, inspection.clone());
            let trusted = inspection["qaLastClick"]["trusted"] == true
                && inspection["qaLastClick"]["activation"] == true;
            run.check(
                "inclined_native_click_is_trusted_and_activated",
                trusted,
                inspection["qaLastClick"].clone(),
            );
            actions.0.push(WorkspaceAction::Expand);
            results.inspections.remove(&id);
            run.enter(Stage::BrowserExpanded);
        }
        Stage::BrowserExpanded => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if !run.elapsed(1)
                || !workspace.expanded
                || navigation.expanded_amount < 0.999
                || !navigation.surface_visible
                || !browser_state_matches(inspection, &run.marker)
            {
                return;
            }
            let preserved =
                workspace.selected.as_ref() == Some(&id) && same_pose(&run.camera, &spatial.camera);
            run.check(
                "expand_preserves_camera_and_same_dom",
                preserved,
                inspection.clone(),
            );
            let path = run.directory.join("expanded-live-client.png");
            match capture_native_client(window_entity, &path) {
                Ok(()) => {
                    run.screenshots.push(path.clone());
                    run.check("expanded_live_client_screenshot", true, json!(path));
                }
                Err(error) => {
                    results.errors.push(error);
                    finish(&mut run, &results, &mut exit);
                    return;
                }
            }
            actions.0.push(WorkspaceAction::Collapse);
            results.inspections.remove(&id);
            run.enter(Stage::BrowserCollapsed);
        }
        Stage::BrowserCollapsed => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if !run.elapsed(1)
                || workspace.expanded
                || navigation.expanded_amount > 0.001
                || !navigation.surface_visible
                || !browser_state_matches(inspection, &run.marker)
            {
                return;
            }
            let preserved =
                workspace.selected.as_ref() == Some(&id) && same_pose(&run.camera, &spatial.camera);
            run.check(
                "collapse_preserves_camera_and_same_dom",
                preserved,
                inspection.clone(),
            );
            run.anchors = spatial.anchors.clone();
            run.viewpoint = *viewpoint;
            spatial.camera.yaw += 0.15;
            spatial.dirty = true;
            results.inspections.remove(&id);
            run.enter(Stage::BrowserOrbit);
        }
        Stage::BrowserOrbit => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if !run.elapsed(1)
                || !navigation.surface_visible
                || !browser_state_matches(inspection, &run.marker)
            {
                return;
            }
            let preserved = workspace.selected.as_ref() == Some(&id)
                && !workspace.expanded
                && run.anchors == spatial.anchors
                && run.viewpoint.translation.distance(viewpoint.translation) > 0.01;
            let details = json!({"selected":workspace.selected,"visible":navigation.surface_visible,"camera":spatial.camera,"nativeSurface":inspection["nativeSurface"]});
            run.check(
                "orbit_keeps_current_planet_selected_surface_live",
                preserved,
                details,
            );
            run.camera = spatial.camera.clone();
            actions.0.push(WorkspaceAction::Deselect);
            run.enter(Stage::BrowserDeselected);
        }
        Stage::BrowserDeselected => {
            if workspace.selected.is_some() || navigation.surface_visible {
                return;
            }
            let Some(id) = run.browser.clone() else {
                return;
            };
            native_click(&id, run.counter_position, &mut web);
            results.inspections.remove(&id);
            run.enter(Stage::BrowserHidden);
        }
        Stage::BrowserHidden => {
            if !run.elapsed(1) || workspace.selected.is_some() {
                return;
            }
            let Some(id) = run.browser.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if !view_ready(inspection) {
                return;
            }
            let preserved = same_pose(&run.camera, &spatial.camera);
            let camera_comparison = json!({"before":run.camera,"after":spatial.camera});
            run.check(
                "deselect_preserves_orbit_camera",
                preserved,
                camera_comparison,
            );
            let input_rejected = browser_state_matches(inspection, &run.marker)
                && inspection["nativeSurface"]["visible"] == false;
            run.check(
                "deselected_surface_rejects_native_input",
                input_rejected,
                inspection.clone(),
            );
            actions.0.push(WorkspaceAction::Select(id.clone()));
            results.inspections.remove(&id);
            run.enter(Stage::BrowserReactivated);
        }
        Stage::BrowserReactivated => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            if workspace.selected.as_ref() != Some(&id)
                || !navigation.surface_visible
                || workspace.expanded
            {
                return;
            }
            let Some(project) = workspace
                .projects
                .iter()
                .find(|project| Some(&project.id) != run.browser_project.as_ref())
            else {
                results.errors.push(
                    "La fixture richiede due pianeti per verificare l'isolamento degli input"
                        .into(),
                );
                finish(&mut run, &results, &mut exit);
                return;
            };
            actions.0.push(WorkspaceAction::Project(project.id.clone()));
            run.enter(Stage::BrowserProjectChanged);
        }
        Stage::BrowserProjectChanged => {
            if !run.elapsed(1)
                || workspace.current_project == run.browser_project
                || workspace.selected.is_some()
                || navigation.surface_visible
            {
                return;
            }
            let Some(id) = run.browser.clone() else {
                return;
            };
            actions.0.push(WorkspaceAction::Select(id.clone()));
            native_click(&id, run.counter_position, &mut web);
            results.inspections.remove(&id);
            run.enter(Stage::BrowserInactive);
        }
        Stage::BrowserInactive => {
            if !run.elapsed(1) {
                return;
            }
            let Some(id) = run.browser.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if !view_ready(inspection) {
                return;
            }
            let isolated = workspace.selected.is_none()
                && !navigation.surface_visible
                && inspection["nativeSurface"]["visible"] == false
                && browser_state_matches(inspection, &run.marker);
            run.check(
                "other_planet_cannot_select_or_input_old_surface",
                isolated,
                inspection.clone(),
            );
            if let Some(project) = run.browser_project.clone() {
                actions.0.push(WorkspaceAction::Project(project));
            }
            run.enter(Stage::BrowserReturned);
        }
        Stage::BrowserReturned => {
            if !run.elapsed(1) || workspace.current_project != run.browser_project {
                return;
            }
            let Some(id) = run.browser.clone() else {
                return;
            };
            position_test_surface(&id, &mut spatial);
            *viewpoint = spatial.camera.transform();
            run.camera = spatial.camera.clone();
            results.inspections.remove(&id);
            actions.0.push(WorkspaceAction::Select(id));
            run.enter(Stage::BrowserRestored);
        }
        Stage::BrowserRestored => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if workspace.selected.as_ref() != Some(&id)
                || !navigation.surface_visible
                || workspace.expanded
                || !view_ready(inspection)
                || !browser_state_matches(inspection, &run.marker)
            {
                return;
            }
            run.check(
                "browser_dom_preserved_after_refocus",
                true,
                inspection.clone(),
            );
            run.popup_tabs = workspace.tabs.iter().map(|tab| tab.id.clone()).collect();
            let marker = serde_json::to_string(&run.auth_marker).unwrap_or_default();
            let Some(position) = element_center(inspection, "qaAuthPopupRect") else {
                return;
            };
            run.auth_position = position;
            web.actions.push(WebAction::Evaluate { id:id.clone(), script:format!("(() => {{ try {{ const marker = {marker}; window.orbitSmokeAuthMarker = marker; window.orbitSmokeAuthReply = null; const receive = event => {{ if(event.origin !== location.origin || event.data?.type !== 'orbit-smoke-auth' || event.data.marker !== marker) return; window.orbitSmokeAuthReply = {{ marker:event.data.marker, origin:event.origin, sourceMatches:event.source === window.orbitSmokePopup }}; window.removeEventListener('message',receive); }}; window.addEventListener('message',receive); return {{ ok:true, smokeAuthPrepared:true }}; }} catch(error) {{ return {{ ok:false, error:String(error) }}; }} }})()") });
            results.inspections.remove(&id);
            run.enter(Stage::BrowserAuthPrepared);
        }
        Stage::BrowserAuthPrepared => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if inspection["smokeAuthPrepared"] != true {
                return;
            }
            native_click(&id, run.auth_position, &mut web);
            results.inspections.remove(&id);
            run.enter(Stage::BrowserAuthPopup);
        }
        Stage::BrowserAuthPopup => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if run.elapsed(3) && inspection["authPopupOpened"] == false {
                let message = format!("Auth popup did not open: {}", inspection["error"]);
                results.errors.push(message);
                finish(&mut run, &results, &mut exit);
                return;
            }
            if inspection["authPopupReply"]["marker"] != run.auth_marker
                || inspection["authPopupClosed"] != true
            {
                return;
            }
            let opener_preserved = inspection["authPopupReply"]["sourceMatches"] == true;
            let trusted = inspection["qaAuthClick"]["trusted"] == true
                && inspection["qaAuthClick"]["activation"] == true;
            run.check(
                "native_popup_has_real_user_gesture",
                trusted,
                inspection["qaAuthClick"].clone(),
            );
            run.check(
                "native_popup_preserves_opener_postmessage",
                opener_preserved,
                inspection["authPopupReply"].clone(),
            );
            run.enter(Stage::BrowserAuthRestored);
        }
        Stage::BrowserAuthRestored => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if !run.elapsed(1) || !view_ready(inspection) || inspection["authPopupClosed"] != true {
                return;
            }
            let current_tabs: HashSet<String> =
                workspace.tabs.iter().map(|tab| tab.id.clone()).collect();
            let unchanged_tabs = current_tabs == run.popup_tabs;
            let parent_preserved = workspace.selected.as_ref() == Some(&id)
                && navigation.surface_visible
                && !workspace.expanded
                && browser_state_matches(inspection, &run.marker);
            run.check(
                "auth_popup_keeps_workspace_tabs",
                unchanged_tabs,
                json!({"windows":current_tabs.len()}),
            );
            run.check(
                "auth_popup_returns_to_same_parent_dom",
                parent_preserved,
                inspection.clone(),
            );
            run.cycle_existing_tabs = current_tabs;
            run.cycle_tabs = vec![id];
            run.cycle_anchors = spatial.anchors.clone();
            run.cycle_camera = spatial.camera.clone();
            run.cycle_viewpoint = *viewpoint;
            for _ in 0..2 {
                actions.0.push(WorkspaceAction::Create(json!({
                    "kind":"browser",
                    "url":format!("{}/content/qa.html?token={}",workspace.origin,workspace.token),
                    "projectId":run.browser_project
                })));
            }
            run.enter(Stage::CycleCreated);
        }
        Stage::CycleCreated => {
            let new_tabs: Vec<String> = workspace
                .tabs
                .iter()
                .filter(|tab| !run.cycle_existing_tabs.contains(&tab.id))
                .map(|tab| tab.id.clone())
                .collect();
            if new_tabs.len() != 2
                || new_tabs
                    .iter()
                    .any(|id| !layout.transforms.contains_key(id))
                || !shortcuts.ready()
                || service
                    .pending
                    .values()
                    .any(|request| matches!(request, PendingRequest::Create))
                || new_tabs.iter().any(|id| {
                    !workspace.resolved.contains_key(id) || workspace.resolving.contains(id)
                })
                || actions.0.iter().any(
                    |action| matches!(action, WorkspaceAction::Select(id) if new_tabs.contains(id)),
                )
            {
                return;
            }
            run.cycle_tabs.extend(new_tabs);
            for anchor in spatial.anchors.values_mut() {
                anchor[0] = 5.8;
                anchor[2] = 6.2;
            }
            for (index, id) in run.cycle_tabs.iter().enumerate() {
                spatial
                    .anchors
                    .insert(id.clone(), [0.4 + index as f32 * 2.0, 0.8, 9.2]);
            }
            spatial.camera.yaw = -0.05;
            spatial.camera.look_target = None;
            spatial.dirty = true;
            *viewpoint = spatial.camera.transform();
            actions
                .0
                .push(WorkspaceAction::Select(run.cycle_tabs[0].clone()));
            run.enter(Stage::CycleReady);
        }
        Stage::CycleReady => {
            let id = run.cycle_tabs[0].clone();
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if workspace.selected.as_ref() != Some(&id)
                || !navigation.surface_visible
                || !view_ready(inspection)
                || !browser_state_matches(inspection, &run.marker)
            {
                return;
            }
            actions.0.push(WorkspaceAction::Expand);
            run.enter(Stage::CycleExpanded);
        }
        Stage::CycleExpanded => {
            let id = run.cycle_tabs[0].clone();
            if !workspace.expanded
                || navigation.expanded_amount < 0.999
                || workspace.selected.as_ref() != Some(&id)
                || !navigation.surface_visible
            {
                return;
            }
            run.cycle_departure_viewpoint = *viewpoint;
            web.actions.push(WebAction::Evaluate {
                id: id.clone(),
                script: "document.querySelector('#qa-input').focus(); ({ok:true,cycleInputPrepared:true})".into(),
            });
            web.actions.push(WebAction::Focus { id: id.clone() });
            results.inspections.remove(&id);
            run.enter(Stage::CycleInputPrepared);
        }
        Stage::CycleInputPrepared => {
            let id = run.cycle_tabs[0].clone();
            if run.entered.elapsed() < Duration::from_millis(200)
                || !results
                    .inspections
                    .get(&id)
                    .is_some_and(|inspection| inspection["cycleInputPrepared"] == true)
            {
                return;
            }
            if run.simulated_cycle {
                actions.0.push(WorkspaceAction::Cycle(1));
                run.enter(Stage::CycleCollapse);
                return;
            }
            eprintln!(
                "QA Shift probe before: callbacks={}, stage={}, threadAlive={}",
                shortcuts.callback_count(),
                shortcuts.callback_stage(),
                shortcuts.thread_alive()
            );
            if let Err(error) = send_native_keys(
                window_entity,
                &[(VK_SHIFT, false), (VK_SHIFT, true)],
                &mut run.input_probe,
            ) {
                results.errors.push(error);
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.enter(Stage::CycleModifierProbe);
        }
        Stage::CycleModifierProbe => {
            if run.entered.elapsed() < Duration::from_millis(300) {
                return;
            }
            eprintln!(
                "QA Shift probe after: callbacks={}, stage={}, threadAlive={}",
                shortcuts.callback_count(),
                shortcuts.callback_stage(),
                shortcuts.thread_alive()
            );
            eprintln!(
                "QA first Alt+Tab hook: ready={}, callbacks={}, stage={}, threadAlive={}",
                shortcuts.ready(),
                shortcuts.callback_count(),
                shortcuts.callback_stage(),
                shortcuts.thread_alive()
            );
            if let Err(error) = send_native_cycle(window_entity, false, &mut run.input_probe) {
                results.errors.push(error);
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.enter(Stage::CycleCollapse);
        }
        Stage::CycleCollapse => {
            let Some(flight) = &navigation.flight else {
                return;
            };
            if flight.id != run.cycle_tabs[1] {
                return;
            }
            let camera_stable = run
                .cycle_departure_viewpoint
                .translation
                .distance(viewpoint.translation)
                < 0.0001
                && run
                    .cycle_departure_viewpoint
                    .rotation
                    .abs_diff_eq(viewpoint.rotation, 0.0001);
            let smooth_collapse = workspace.selected.as_ref() == Some(&run.cycle_tabs[0])
                && navigation.surface_visible
                && navigation.expanded_amount > 0.0
                && !workspace.expanded
                && flight.elapsed == 0.0
                && camera_stable;
            let details = json!({"destination":flight.id,"selected":workspace.selected,"visible":navigation.surface_visible,"expandedAmount":navigation.expanded_amount,"flightElapsed":flight.elapsed,"cameraStable":camera_stable});
            run.check_cycle(
                "alt_tab_from_expanded_collapses_same_surface_before_camera_flight",
                smooth_collapse,
                details,
            );
            run.enter(Stage::CycleForward);
        }
        Stage::CycleForward => {
            let Some(flight) = &navigation.flight else {
                return;
            };
            if flight.id != run.cycle_tabs[1]
                || navigation.expanded_amount > 0.001
                || workspace.selected.is_some()
                || navigation.surface_visible
            {
                return;
            }
            let forward = workspace.selected.is_none()
                && !workspace.expanded
                && !navigation.surface_visible
                && Some(&flight.project_id) == run.browser_project.as_ref();
            let details = json!({"destination":flight.id,"project":flight.project_id,"selected":workspace.selected,"expanded":workspace.expanded,"visible":navigation.surface_visible});
            run.check_cycle(
                "native_alt_tab_from_webview_starts_current_planet_flight",
                forward,
                details,
            );
            let id = run.cycle_tabs[0].clone();
            native_click(&id, run.counter_position, &mut web);
            results.inspections.remove(&id);
            run.enter(Stage::CycleForwardArrival);
        }
        Stage::CycleForwardArrival => {
            let previous = run.cycle_tabs[0].clone();
            run.inspect(&previous, &mut web);
            let Some(inspection) = results.inspections.get(&previous) else {
                return;
            };
            if !view_ready(inspection) {
                return;
            }
            if navigation.flight.is_some() {
                let rejected = browser_state_matches(inspection, &run.marker)
                    && inspection["nativeSurface"]["visible"] == false;
                run.check_cycle(
                    "cycling_disables_previous_surface_and_preserves_dom",
                    rejected,
                    inspection.clone(),
                );
                run.enter(Stage::CycleDestinationPrepared);
            }
        }
        Stage::CycleDestinationPrepared => {
            let id = run.cycle_tabs[1].clone();
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if navigation.flight.is_some()
                || workspace.selected.as_ref() != Some(&id)
                || !navigation.surface_visible
                || !view_ready(inspection)
                || inspection["hasQa"] != true
            {
                return;
            }
            let centered =
                panel_center_error(&id, &panels, render_camera, camera_global, primary_window);
            run.check_cycle("cycle_arrival_centers_live_window_without_fullscreen", !workspace.expanded && centered.is_some_and(|distance| distance < 30.0), json!({"destination":id,"centerError":centered,"expanded":workspace.expanded,"visible":navigation.surface_visible}));
            web.actions.push(WebAction::Evaluate {
                id: id.clone(),
                script: "(() => { const input = document.querySelector('#qa-input'); input.value = ''; input.focus(); return {ok:true,cycleDestinationPrepared:true}; })()".into(),
            });
            web.actions.push(WebAction::Focus { id: id.clone() });
            results.inspections.remove(&id);
            run.enter(Stage::CycleDestinationInputReady);
        }
        Stage::CycleDestinationInputReady => {
            let id = run.cycle_tabs[1].clone();
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if inspection["cycleDestinationPrepared"] == true {
                if run.entered.elapsed() < Duration::from_millis(200) {
                    return;
                }
                if let Err(error) = send_native_text(window_entity, &run.cycle_native_marker) {
                    results.errors.push(error);
                    finish(&mut run, &results, &mut exit);
                    return;
                }
                results.inspections.remove(&id);
                run.enter(Stage::CycleDestinationTyped);
                return;
            }
        }
        Stage::CycleDestinationTyped => {
            let id = run.cycle_tabs[1].clone();
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if !view_ready(inspection)
                || inspection["qaInput"] != run.cycle_native_marker
                || inspection["qaValue"] != run.cycle_native_marker
            {
                return;
            }
            run.check("cycle_destination_accepts_native_keyboard", !workspace.expanded && navigation.surface_visible, json!({"input":inspection["qaInput"],"selected":workspace.selected,"expanded":workspace.expanded}));
            if let Err(error) = request_window_cycle(&mut run, window_entity, true, &mut actions) {
                results.errors.push(error);
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.enter(Stage::CycleReverse);
        }
        Stage::CycleReverse => {
            let Some(flight) = &navigation.flight else {
                return;
            };
            if flight.id != run.cycle_tabs[0] {
                return;
            }
            run.check_cycle(
                "native_alt_shift_tab_reverses_flight_order",
                true,
                json!({"destination":flight.id,"project":flight.project_id}),
            );
            let id = run.cycle_tabs[0].clone();
            results.inspections.remove(&id);
            run.enter(Stage::CycleReverseArrival);
        }
        Stage::CycleReverseArrival => {
            let id = run.cycle_tabs[0].clone();
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if navigation.flight.is_some()
                || workspace.selected.as_ref() != Some(&id)
                || !navigation.surface_visible
                || !view_ready(inspection)
            {
                return;
            }
            let retained = browser_state_matches(inspection, &run.marker);
            run.check_cycle(
                "cycle_return_retains_same_browser_dom",
                retained,
                inspection.clone(),
            );
            if let Err(error) = request_window_cycle(&mut run, window_entity, false, &mut actions) {
                results.errors.push(error);
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.enter(Stage::CycleRapidStarted);
        }
        Stage::CycleRapidStarted => {
            let Some(flight) = &navigation.flight else {
                return;
            };
            if flight.id != run.cycle_tabs[1] || flight.elapsed < 0.1 {
                return;
            }
            run.cycle_retarget_viewpoint = viewpoint.translation;
            if let Err(error) = request_window_cycle(&mut run, window_entity, false, &mut actions) {
                results.errors.push(error);
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.enter(Stage::CycleRapidRetargeted);
        }
        Stage::CycleRapidRetargeted => {
            let Some(flight) = &navigation.flight else {
                return;
            };
            if flight.id != run.cycle_tabs[2] {
                return;
            }
            let continuous = viewpoint.translation.distance(run.cycle_retarget_viewpoint) < 2.0;
            let details = json!({"destination":flight.id,"elapsed":flight.elapsed,"movement":viewpoint.translation.distance(run.cycle_retarget_viewpoint)});
            run.check_cycle(
                "rapid_alt_tab_retargets_current_flight_without_jump",
                continuous && flight.elapsed < 0.2,
                details,
            );
            run.enter(Stage::CycleRapidArrival);
        }
        Stage::CycleRapidArrival => {
            let id = run.cycle_tabs[2].clone();
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if navigation.flight.is_some()
                || workspace.selected.as_ref() != Some(&id)
                || !navigation.surface_visible
                || !view_ready(inspection)
                || inspection["hasQa"] != true
            {
                return;
            }
            let exterior = run.cycle_transit_samples > 3 && run.cycle_min_radius >= 4.999;
            let current_tabs: HashSet<String> =
                workspace.tabs.iter().map(|tab| tab.id.clone()).collect();
            let expected_tabs: HashSet<String> = run
                .cycle_existing_tabs
                .iter()
                .chain(run.cycle_tabs.iter())
                .cloned()
                .collect();
            let foreground = run.cycle_foreground;
            let details = json!({"minimumRadius":run.cycle_min_radius,"samples":run.cycle_transit_samples,"foregroundRetained":foreground});
            run.check_cycle(
                "flight_stays_outside_planet_and_retains_foreground",
                exterior && foreground,
                details,
            );
            let final_destination = !run.cycle_unexpected_selection
                && current_tabs == expected_tabs
                && !workspace.expanded;
            let details = json!({"destination":id,"windows":current_tabs.len(),"intermediateSelection":run.cycle_unexpected_selection});
            run.check_cycle(
                "rapid_cycle_activates_only_final_destination_without_new_tabs",
                final_destination,
                details,
            );
            let path = run.directory.join("cycle-arrival-live-client.png");
            if let Err(error) = capture_native_client(window_entity, &path) {
                results.errors.push(error);
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.screenshots.push(path.clone());
            run.check_cycle("cycle_arrival_live_client_screenshot", true, json!(path));
            if let Err(error) = request_window_cycle(&mut run, window_entity, true, &mut actions) {
                results.errors.push(error);
                finish(&mut run, &results, &mut exit);
                return;
            }
            run.enter(Stage::CycleCancelStarted);
        }
        Stage::CycleCancelStarted => {
            if navigation.flight.is_none() {
                return;
            }
            let Some(project) = workspace
                .projects
                .iter()
                .find(|project| Some(&project.id) != run.browser_project.as_ref())
            else {
                return;
            };
            actions.0.push(WorkspaceAction::Project(project.id.clone()));
            run.enter(Stage::CycleCancelled);
        }
        Stage::CycleCancelled => {
            if !run.elapsed(2) {
                return;
            }
            let cancelled = navigation.flight.is_none()
                && workspace.selected.is_none()
                && !navigation.surface_visible
                && workspace.current_project != run.browser_project;
            run.check_cycle("project_change_cancels_flight_without_late_focus", cancelled, json!({"project":workspace.current_project,"selected":workspace.selected,"visible":navigation.surface_visible}));
            if let Some(project) = run.browser_project.clone() {
                actions.0.push(WorkspaceAction::Project(project));
            }
            run.enter(Stage::CycleRestored);
        }
        Stage::CycleRestored => {
            if !run.elapsed(1) || workspace.current_project != run.browser_project {
                return;
            }
            spatial.anchors = run.cycle_anchors.clone();
            spatial.camera = run.cycle_camera.clone();
            spatial.dirty = true;
            *viewpoint = run.cycle_viewpoint;
            let id = run.cycle_tabs[0].clone();
            actions.0.push(WorkspaceAction::Select(id.clone()));
            results.snapshots.remove(&id);
            web.actions.push(WebAction::Capture { id });
            run.enter(Stage::BrowserCapture);
        }
        Stage::BrowserCapture => {
            let Some(id) = run.browser.clone() else {
                return;
            };
            if !snapshot_ready(&id, &results) {
                return;
            }
            run.check(
                "browser_webview_snapshot",
                true,
                json!({"id":id,"file":preview_path(&id)}),
            );
            actions.0.push(WorkspaceAction::Deselect);
            run.enter(Stage::Application);
        }
        Stage::Application => {
            let Some(kind) = run.kind() else {
                actions.0.push(WorkspaceAction::Deselect);
                if let Some(project) = workspace.projects.first() {
                    actions.0.push(WorkspaceAction::Project(project.id.clone()));
                }
                run.enter(Stage::LoadedProject);
                return;
            };
            let Some(tab) = workspace.tabs.iter().find(|tab| tab.kind == kind) else {
                if !run.creation_requested {
                    actions.0.push(WorkspaceAction::Create(
                        json!({"kind":kind,"projectId":workspace.current_project}),
                    ));
                    run.creation_requested = true;
                }
                return;
            };
            let id = tab.id.clone();
            run.application = Some(id.clone());
            results.inspections.remove(&id);
            actions
                .0
                .push(WorkspaceAction::Project(tab.project_id.clone()));
            run.enter(Stage::ApplicationProject);
        }
        Stage::ApplicationProject => {
            let Some(id) = run.application.clone() else {
                return;
            };
            let Some(tab) = workspace.tab(&id) else {
                return;
            };
            if !run.elapsed(1) || workspace.current_project.as_ref() != Some(&tab.project_id) {
                return;
            }
            position_test_surface(&id, &mut spatial);
            *viewpoint = spatial.camera.transform();
            actions.0.push(WorkspaceAction::Select(id));
            run.enter(Stage::ApplicationReady);
        }
        Stage::ApplicationReady => {
            let Some(id) = run.application.clone() else {
                return;
            };
            let Some(kind) = run.kind() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            let has_application = match kind {
                "terminal" => {
                    inspection["hasTerminal"] == true && inspection["terminalReady"] == true
                }
                "chat" => {
                    inspection["hasChat"] == true
                        && inspection["chatReady"] == true
                        && inspection["chatBridge"] == true
                        && inspection["chatProviderDisabled"] == false
                        && inspection["chatProvider"]["available"] == true
                        && inspection["chatProvider"]["models"]
                            .as_array()
                            .is_some_and(|models| !models.is_empty())
                }
                "code" => {
                    inspection["hasEditor"] == true
                        && inspection["editorReady"] == true
                        && inspection["editorPartsReady"] == true
                        && inspection["title"].as_str().is_some_and(|title| {
                            title.contains("Visual Studio Code") && !title.contains("undefined")
                        })
                }
                _ => false,
            };
            if workspace.selected.as_ref() != Some(&id)
                || workspace.expanded
                || !navigation.surface_visible
                || inspection["nativeSurface"]["visible"] != true
                || !view_ready(inspection)
                || !has_application
            {
                return;
            }
            run.check(&format!("{kind}_runtime_ready"), true, inspection.clone());
            if kind == "terminal" {
                web.actions.push(WebAction::Focus { id });
                run.enter(Stage::TerminalType);
                return;
            }
            results.snapshots.remove(&id);
            web.actions.push(WebAction::Capture { id });
            run.enter(Stage::ApplicationCapture);
        }
        Stage::TerminalType => {
            if run.entered.elapsed() < Duration::from_millis(200) {
                return;
            }
            let Some(id) = run.application.clone() else {
                return;
            };
            if workspace.selected.as_ref() != Some(&id)
                || !navigation.surface_visible
                || workspace.expanded
            {
                return;
            }
            let command = format!(
                "Write-Output ('ORBIT_NATIVE_' + 'SMOKE_{}')",
                std::process::id()
            );
            match send_native_text(window_entity, &command) {
                Ok(count) => {
                    let details = json!({"events":count,"fullscreen":workspace.expanded,"surfaceCorners":navigation.surface_corners});
                    run.check("inclined_terminal_accepts_native_keyboard", true, details);
                }
                Err(error) => {
                    results.errors.push(error);
                    finish(&mut run, &results, &mut exit);
                    return;
                }
            }
            results.inspections.remove(&id);
            run.enter(Stage::TerminalOutput);
        }
        Stage::TerminalOutput => {
            let Some(id) = run.application.clone() else {
                return;
            };
            run.inspect(&id, &mut web);
            let Some(inspection) = results.inspections.get(&id) else {
                return;
            };
            if inspection["terminalReady"] != true
                || !inspection["terminalOutput"]
                    .as_str()
                    .is_some_and(|output| output.contains(&run.terminal_marker))
            {
                return;
            }
            let terminal_details = json!({"expected":run.terminal_marker,"inspection":inspection});
            run.check("terminal_real_pty_input_output", true, terminal_details);
            results.snapshots.remove(&id);
            web.actions.push(WebAction::Capture { id });
            run.enter(Stage::ApplicationCapture);
        }
        Stage::ApplicationCapture => {
            let Some(id) = run.application.clone() else {
                return;
            };
            if !snapshot_ready(&id, &results) {
                return;
            }
            let snapshot_details = json!({"id":id,"kind":run.kind(),"file":preview_path(&id)});
            run.check("application_webview_snapshot", true, snapshot_details);
            actions.0.push(WorkspaceAction::Deselect);
            run.application_index += 1;
            run.creation_requested = false;
            run.enter(Stage::Application);
        }
        Stage::LoadedProject => {
            if !run.elapsed(2) || workspace.selected.is_some() {
                return;
            }
            let preview_count = workspace.projects.first().map_or(0, |project| {
                workspace
                    .tabs
                    .iter()
                    .filter(|tab| {
                        tab.project_id == project.id && layout.previews.contains_key(&tab.id)
                    })
                    .count()
            });
            run.check(
                "project_contains_real_webview_previews",
                preview_count > 0,
                json!({"previews":preview_count}),
            );
            run.screenshot(&mut commands, "loaded-project");
            run.enter(Stage::LoadedProjectCapture);
        }
        Stage::LoadedProjectCapture => {
            if !run.screenshot_ready() {
                return;
            }
            let screenshot_path = json!(run.screenshots.last());
            run.check("loaded_project_scene_screenshot", true, screenshot_path);
            if let Some(id) = run.browser.clone() {
                actions.0.push(WorkspaceAction::Close(id));
            }
            for id in run.cycle_tabs.iter().skip(1) {
                actions.0.push(WorkspaceAction::Close(id.clone()));
            }
            run.enter(Stage::Cleanup);
        }
        Stage::Cleanup => {
            if workspace.selected.is_some()
                || run
                    .browser
                    .as_ref()
                    .is_some_and(|id| workspace.tab(id).is_some())
                || run.cycle_tabs.iter().any(|id| workspace.tab(id).is_some())
            {
                return;
            }
            let browser_details = json!({"id":run.browser});
            run.check("qa_browser_closed", true, browser_details);
            finish(&mut run, &results, &mut exit);
        }
        Stage::Finished => {}
    }
}

fn position_test_surface(id: &str, spatial: &mut SpatialState) {
    spatial.camera.look_target = None;
    for anchor in spatial.anchors.values_mut() {
        anchor[2] = 6.2;
    }
    spatial
        .anchors
        .insert(id.into(), [spatial.camera.yaw + 0.45, 0.8, 9.2]);
    spatial.dirty = true;
}

fn element_center(inspection: &Value, field: &str) -> Option<[i32; 2]> {
    let rectangle = &inspection[field];
    let width = rectangle["width"].as_f64()?;
    let height = rectangle["height"].as_f64()?;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some([
        (rectangle["x"].as_f64()? + width * 0.5).round() as i32,
        (rectangle["y"].as_f64()? + height * 0.5).round() as i32,
    ])
}

fn native_click(id: &str, position: [i32; 2], commands: &mut WebCommands) {
    for (kind, modifiers) in [
        (WebMouseKind::Move, 0),
        (WebMouseKind::LeftDown, 1),
        (WebMouseKind::LeftUp, 0),
    ] {
        commands.actions.push(WebAction::Mouse {
            id: id.into(),
            kind,
            position,
            modifiers,
            wheel_delta: 0,
        });
    }
}

fn projected_page_position(
    id: &str,
    position: [i32; 2],
    panels: &Query<(&WindowPanel, &GlobalTransform)>,
    camera: &Camera,
    camera_transform: &GlobalTransform,
    window: &Window,
) -> Option<Vec2> {
    let (panel, transform) = panels.iter().find(|(panel, _)| panel.id == id)?;
    let local = Vec3::new(
        (position[0] as f32 / PAGE_VIEWPORT[0] as f32 - 0.5) * panel.width,
        (0.5 - position[1] as f32 / PAGE_VIEWPORT[1] as f32) * panel.height,
        0.0,
    );
    camera
        .world_to_viewport(camera_transform, transform.transform_point(local))
        .ok()
        .map(|position| position * window.scale_factor())
}

fn native_window_handle(entity: Entity) -> Result<HWND, String> {
    let parent = WINIT_WINDOWS.with(|windows| {
        let windows = windows.borrow();
        let window = windows
            .get_window(entity)
            .ok_or_else(|| "Finestra nativa QA non disponibile".to_owned())?;
        bevy::window::RawHandleWrapper::new(window).map_err(|error| error.to_string())
    })?;
    let RawWindowHandle::Win32(handle) = parent.get_window_handle() else {
        return Err("Handle QA non Win32".into());
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    Ok(hwnd)
}

fn own_foreground_window(entity: Entity) -> Result<HWND, String> {
    let hwnd = native_window_handle(entity)?;
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
    if unsafe { GetForegroundWindow() } != hwnd {
        return Err("Input o cattura QA annullati: la nostra finestra non è in primo piano".into());
    }
    Ok(hwnd)
}

fn request_window_cycle(
    run: &mut SmokeRun,
    entity: Entity,
    reverse: bool,
    actions: &mut WorkspaceActions,
) -> Result<(), String> {
    if run.simulated_cycle {
        actions
            .0
            .push(WorkspaceAction::Cycle(if reverse { -1 } else { 1 }));
        Ok(())
    } else {
        send_native_cycle(entity, reverse, &mut run.input_probe)
    }
}

fn send_native_cycle(
    entity: Entity,
    reverse: bool,
    probe: &mut Option<NativeInputProbe>,
) -> Result<(), String> {
    let mut keys = vec![(VK_MENU, false)];
    if reverse {
        keys.push((VK_SHIFT, false));
    }
    keys.extend([(VK_TAB, false), (VK_TAB, true)]);
    if reverse {
        keys.push((VK_SHIFT, true));
    }
    keys.push((VK_MENU, true));
    send_native_keys(entity, &keys, probe)
}

fn send_native_keys(
    entity: Entity,
    keys: &[(VIRTUAL_KEY, bool)],
    probe: &mut Option<NativeInputProbe>,
) -> Result<(), String> {
    let hwnd = own_foreground_window(entity)?;
    if std::env::var("ORBIT_QA_EXTERNAL_INPUT").as_deref() == Ok("1") {
        if probe.is_none() {
            *probe = Some(launch_native_input_probe()?);
        }
        return request_native_keys(probe.as_mut().unwrap(), hwnd, keys);
    }
    send_native_key_events(hwnd, keys)
}

fn send_native_key_events(hwnd: HWND, keys: &[(VIRTUAL_KEY, bool)]) -> Result<(), String> {
    let inputs: Vec<INPUT> = keys
        .iter()
        .copied()
        .map(|(key, release)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    wScan: 0,
                    dwFlags: if release {
                        KEYEVENTF_KEYUP
                    } else {
                        Default::default()
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
        .collect();
    if unsafe { GetAncestor(GetForegroundWindow(), GA_ROOT) } != hwnd {
        return Err("Ciclo QA annullato: focus nativo cambiato".into());
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        let releases: Vec<INPUT> = [VK_TAB, VK_SHIFT, VK_MENU]
            .into_iter()
            .map(|key| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: key,
                        dwFlags: KEYEVENTF_KEYUP,
                        ..Default::default()
                    },
                },
            })
            .collect();
        unsafe { SendInput(&releases, std::mem::size_of::<INPUT>() as i32) };
        return Err(format!(
            "Scorciatoia QA incompleta: {sent}/{} eventi",
            inputs.len()
        ));
    }
    Ok(())
}

fn panel_center_error(
    id: &str,
    panels: &Query<(&WindowPanel, &GlobalTransform)>,
    camera: &Camera,
    camera_transform: &GlobalTransform,
    window: &Window,
) -> Option<f32> {
    let (_, transform) = panels.iter().find(|(panel, _)| panel.id == id)?;
    let projected = camera
        .world_to_viewport(camera_transform, transform.translation())
        .ok()?;
    let expected = Vec2::new(window.width() * 0.5, window.height() * 0.5 + 32.0);
    Some(projected.distance(expected))
}

fn send_native_pointer(
    entity: Entity,
    client: [i32; 2],
    flags: MOUSE_EVENT_FLAGS,
) -> Result<(), String> {
    let hwnd = own_foreground_window(entity)?;
    let mut rectangle = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rectangle) }.map_err(|error| error.to_string())?;
    if client[0] < rectangle.left
        || client[0] >= rectangle.right
        || client[1] < rectangle.top
        || client[1] >= rectangle.bottom
    {
        return Err("Puntatore QA fuori dal client della nostra app".into());
    }
    let mut screen = POINT {
        x: client[0],
        y: client[1],
    };
    if !unsafe { ClientToScreen(hwnd, &mut screen) }.as_bool() {
        return Err("Coordinate puntatore QA non disponibili".into());
    }
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut monitor_info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool()
        || screen.x < monitor_info.rcWork.left
        || screen.x >= monitor_info.rcWork.right
        || screen.y < monitor_info.rcWork.top
        || screen.y >= monitor_info.rcWork.bottom
    {
        return Err("Puntatore QA fuori dall'area visibile della nostra app".into());
    }
    let (desktop_left, desktop_top, desktop_width, desktop_height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if desktop_width <= 1 || desktop_height <= 1 {
        return Err("Desktop QA non disponibile".into());
    }
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: (((screen.x - desktop_left) as f64 * 65535.0) / (desktop_width - 1) as f64)
                    .round() as i32,
                dy: (((screen.y - desktop_top) as f64 * 65535.0) / (desktop_height - 1) as f64)
                    .round() as i32,
                mouseData: 0,
                dwFlags: flags | MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    if unsafe { GetForegroundWindow() } != hwnd {
        return Err("Puntatore QA annullato: focus nativo cambiato".into());
    }
    if unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) } != 1 {
        return Err("Evento puntatore QA non inviato".into());
    }
    Ok(())
}

fn send_native_text(entity: Entity, text: &str) -> Result<usize, String> {
    let hwnd = own_foreground_window(entity)?;
    let mut inputs = Vec::with_capacity(text.len() * 2 + 2);
    for character in text.encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: character,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }
    }
    for flags in [Default::default(), KEYEVENTF_KEYUP] {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_RETURN,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
    if unsafe { GetForegroundWindow() } != hwnd {
        return Err("Input QA annullato: focus nativo cambiato".into());
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(format!(
            "Input QA incompleto: {sent}/{} eventi",
            inputs.len()
        ));
    }
    Ok(inputs.len())
}

fn capture_native_client(entity: Entity, path: &Path) -> Result<(), String> {
    let hwnd = own_foreground_window(entity)?;
    let mut rectangle = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rectangle) }.map_err(|error| error.to_string())?;
    let mut origin = POINT::default();
    if !unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
        return Err("Coordinate client QA non disponibili".into());
    }
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut monitor_info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
        return Err("Area monitor QA non disponibile".into());
    }
    let left = (origin.x + rectangle.left).max(monitor_info.rcWork.left);
    let top = (origin.y + rectangle.top).max(monitor_info.rcWork.top);
    let right = (origin.x + rectangle.right).min(monitor_info.rcWork.right);
    let bottom = (origin.y + rectangle.bottom).min(monitor_info.rcWork.bottom);
    let width = right - left;
    let height = bottom - top;
    if width <= 0 || height <= 0 {
        return Err("Client QA fuori dall'area del monitor".into());
    }
    origin.x = left;
    origin.y = top;
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    unsafe {
        let desktop = GetDC(None);
        if desktop.0.is_null() {
            return Err("DC QA non disponibile".into());
        }
        let buffer = CreateCompatibleDC(Some(desktop));
        if buffer.0.is_null() {
            ReleaseDC(None, desktop);
            return Err("DC bitmap QA non disponibile".into());
        }
        let bitmap = CreateCompatibleBitmap(desktop, width, height);
        if bitmap.0.is_null() {
            let _ = DeleteDC(buffer);
            ReleaseDC(None, desktop);
            return Err("Bitmap QA non disponibile".into());
        }
        let original = SelectObject(buffer, HGDIOBJ(bitmap.0));
        let copied = if GetForegroundWindow() == hwnd {
            BitBlt(
                buffer,
                0,
                0,
                width,
                height,
                Some(desktop),
                origin.x,
                origin.y,
                SRCCOPY | CAPTUREBLT,
            )
            .map_err(|error| error.to_string())
        } else {
            Err("Cattura QA annullata: focus nativo cambiato".into())
        };
        SelectObject(buffer, original);
        let mut description = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let rows = if copied.is_ok() {
            GetDIBits(
                buffer,
                bitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr().cast()),
                &mut description,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(buffer);
        ReleaseDC(None, desktop);
        copied?;
        if rows != height {
            return Err(format!("Bitmap QA incompleta: {rows}/{height} righe"));
        }
    }
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    image::save_buffer(
        path,
        &pixels,
        width as u32,
        height as u32,
        image::ColorType::Rgba8,
    )
    .map_err(|error| error.to_string())
}

fn view_ready(inspection: &Value) -> bool {
    inspection["ok"] == true
        && inspection["readyState"] == "complete"
        && inspection["viewport"]["width"]
            .as_u64()
            .is_some_and(|width| width >= 640)
        && inspection["viewport"]["height"]
            .as_u64()
            .is_some_and(|height| height >= 300)
}

fn browser_state_matches(inspection: &Value, marker: &str) -> bool {
    inspection["smokeMarker"] == marker
        && inspection["qaInput"] == "ORBIT SMOKE INPUT"
        && inspection["qaValue"] == "ORBIT SMOKE INPUT"
        && inspection["qaCounter"] == "1"
}

fn snapshot_ready(id: &str, results: &SmokeResults) -> bool {
    results.snapshots.contains(id)
        && std::fs::metadata(preview_path(id)).is_ok_and(|metadata| metadata.len() > 1024)
}

fn same_transforms(
    first: &HashMap<String, Transform>,
    second: &HashMap<String, Transform>,
) -> bool {
    first.len() == second.len()
        && first.iter().all(|(id, before)| {
            second.get(id).is_some_and(|after| {
                before.translation.distance(after.translation) < 0.00001
                    && before.rotation.abs_diff_eq(after.rotation, 0.00001)
                    && before.scale.distance(after.scale) < 0.00001
            })
        })
}

fn same_pose(first: &OrbitPose, second: &OrbitPose) -> bool {
    Vec3::from_array(first.target).distance(Vec3::from_array(second.target)) < 0.00001
        && (first.yaw - second.yaw).abs() < 0.00001
        && (first.pitch - second.pitch).abs() < 0.00001
        && (first.distance - second.distance).abs() < 0.00001
        && first.look_target == second.look_target
}

fn finish(run: &mut SmokeRun, results: &SmokeResults, exit: &mut MessageWriter<AppExit>) {
    if run.pointer_down {
        if let Some(entity) = run.native_entity {
            let _ = send_native_pointer(entity, run.pointer_client, MOUSEEVENTF_LEFTUP);
        }
        run.pointer_down = false;
    }
    run.input_probe = None;
    let passed =
        results.errors.is_empty() && run.checks.iter().all(|check| check["passed"] == true);
    let mut limits = vec![
        "Scene PNGs use Bevy; inclined and expanded live PNGs capture only this application's foreground client rectangle including native composition.",
        "QA text field state is set through DOM; pointer clicks use native WebView2 input, and the terminal command uses native keyboard events.",
        "Chat runtime UI is inspected without sending an AI prompt.",
    ];
    if run.simulated_cycle {
        limits.push("Shortcut interception was not exercised; cycle actions were simulated; pointer and destination text input remain native.");
    }
    let mode = if run.simulated_cycle {
        "cycle-simulated"
    } else if run.full {
        "smoke"
    } else {
        "capture"
    };
    let report = json!({"passed":passed,"mode":mode,"stage":format!("{:?}",run.stage),"durationSeconds":run.started.elapsed().as_secs_f64(),"checks":run.checks,"errors":results.errors,"timeoutState":run.timeout_state,"inspections":results.inspections,"screenshots":run.screenshots,"webSnapshots":results.snapshots.iter().map(|id|json!({"id":id,"file":preview_path(id)})).collect::<Vec<_>>(),"limits":limits});
    let written = std::fs::create_dir_all(".runtime/qa").and_then(|_| {
        let bytes = serde_json::to_vec_pretty(&report)?;
        std::fs::write(".runtime/qa/report.json", bytes)
    });
    if let Err(error) = &written {
        eprintln!("QA report: {error}");
    }
    eprintln!("QA completed: passed={passed}, report=.runtime/qa/report.json");
    run.enter(Stage::Finished);
    exit.write(if passed && written.is_ok() {
        AppExit::Success
    } else {
        AppExit::error()
    });
}
