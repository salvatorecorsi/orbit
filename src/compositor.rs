use std::{
    cell::Cell,
    fmt::Arguments,
    sync::{OnceLock, mpsc},
    time::Instant,
};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler,
    CreateCoreWebView2CompositionControllerCompletedHandler, ExecuteScriptCompletedHandler,
    Microsoft::Web::WebView2::Win32::*, PermissionRequestedEventHandler,
};
use windows::{
    Win32::{
        Foundation::{HWND, POINT, RECT},
        Graphics::Direct2D::Common::D2D_RECT_F,
        Graphics::DirectComposition::{
            DCompositionCreateDevice2, IDCompositionDesktopDevice, IDCompositionDevice,
            IDCompositionEffectGroup, IDCompositionMatrixTransform3D, IDCompositionTarget,
            IDCompositionVisual,
        },
        UI::Input::KeyboardAndMouse::SetFocus,
        UI::WindowsAndMessaging::{HCURSOR, SetCursor},
    },
    core::{HSTRING, IUnknown, Interface},
};
use wry::{Rect, dpi::LogicalPosition};

use crate::webviews::WebMouseKind;

pub fn trace_webview_step(message: Arguments<'_>) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    static STARTED: OnceLock<Instant> = OnceLock::new();
    if *ENABLED
        .get_or_init(|| std::env::var_os("ORBIT_TRACE_WEBVIEWS").is_some_and(|value| value != "0"))
    {
        let elapsed = STARTED.get_or_init(Instant::now).elapsed().as_secs_f64();
        eprintln!("WEBVIEW +{elapsed:.3}s {message}");
    }
}

pub struct Compositor {
    device: IDCompositionDevice,
    root: IDCompositionVisual,
    _target: IDCompositionTarget,
    parent: HWND,
}

pub struct CompositionRequest {
    receiver: mpsc::Receiver<Result<ICoreWebView2CompositionController, String>>,
}

impl CompositionRequest {
    pub fn poll(&self) -> Result<Option<ICoreWebView2CompositionController>, String> {
        match self.receiver.try_recv() {
            Ok(result) => result.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err("Creazione WebView2 interrotta".into()),
        }
    }
}

impl Drop for CompositionRequest {
    fn drop(&mut self) {
        for controller in self.receiver.try_iter().flatten() {
            close_composition_controller(&controller);
        }
    }
}

impl Compositor {
    pub fn clip(&self, width: f32, height: f32, top: f32) -> windows::core::Result<()> {
        unsafe {
            self.root.SetClip2(&D2D_RECT_F {
                left: 0.0,
                top: top.min(height),
                right: width,
                bottom: height,
            })?;
            self.device.Commit()
        }
    }
    pub fn new(parent: HWND) -> windows::core::Result<Self> {
        unsafe {
            let desktop: IDCompositionDesktopDevice = DCompositionCreateDevice2(None::<&IUnknown>)?;
            let device: IDCompositionDevice = desktop.cast()?;
            let target = desktop.CreateTargetForHwnd(parent, false)?;
            let root = device.CreateVisual()?;
            target.SetRoot(&root)?;
            device.Commit()?;
            Ok(Self {
                device,
                root,
                _target: target,
                parent,
            })
        }
    }

    pub fn request_page(
        &self,
        environment: &ICoreWebView2Environment,
    ) -> Result<CompositionRequest, String> {
        trace_webview_step(format_args!("composition.request_page.begin"));
        let (sender, receiver) = mpsc::channel();
        let completed = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(
            move |status, controller| {
                trace_webview_step(format_args!(
                    "composition.create_page.callback.begin success={}",
                    status.is_ok()
                ));
                let result = match status {
                    Ok(()) => {
                        controller.ok_or_else(|| "Controller WebView2 non disponibile".to_owned())
                    }
                    Err(error) => {
                        if let Some(controller) = controller {
                            close_composition_controller(&controller);
                        }
                        Err(error.to_string())
                    }
                };
                if let Err(mpsc::SendError(Ok(controller))) = sender.send(result) {
                    close_composition_controller(&controller);
                }
                trace_webview_step(format_args!("composition.create_page.callback.end"));
                Ok(())
            },
        ));
        trace_webview_step(format_args!("composition.create_page.request.begin"));
        unsafe {
            environment
                .cast::<ICoreWebView2Environment3>()
                .and_then(|environment| {
                    environment.CreateCoreWebView2CompositionController(self.parent, &completed)
                })
                .map_err(|error| error.to_string())?;
        }
        trace_webview_step(format_args!("composition.create_page.request.end"));
        Ok(CompositionRequest { receiver })
    }

    pub fn attach_page(
        &self,
        composition: ICoreWebView2CompositionController,
    ) -> Result<CompositionPage, String> {
        let result = self.build_page(composition.clone());
        if result.is_err() {
            close_composition_controller(&composition);
        }
        result
    }

    fn build_page(
        &self,
        composition: ICoreWebView2CompositionController,
    ) -> Result<CompositionPage, String> {
        trace_webview_step(format_args!("composition.attach_page.begin"));
        let controller: ICoreWebView2Controller =
            composition.cast().map_err(|error| error.to_string())?;
        let webview = unsafe { controller.CoreWebView2() }.map_err(|error| error.to_string())?;
        let visual = unsafe { self.device.CreateVisual() }.map_err(|error| error.to_string())?;
        let transform =
            unsafe { self.device.CreateMatrixTransform3D() }.map_err(|error| error.to_string())?;
        let effects =
            unsafe { self.device.CreateEffectGroup() }.map_err(|error| error.to_string())?;
        unsafe {
            effects
                .SetTransform3D(&transform)
                .map_err(|error| error.to_string())?;
            visual
                .SetEffect(&effects)
                .map_err(|error| error.to_string())?;
            self.root
                .AddVisual(&visual, true, None::<&IDCompositionVisual>)
                .map_err(|error| error.to_string())?;
            composition
                .SetRootVisualTarget(&visual)
                .map_err(|error| error.to_string())?;
            controller
                .SetIsVisible(false)
                .map_err(|error| error.to_string())?;
            if let Ok(scaling) = controller.cast::<ICoreWebView2Controller3>() {
                scaling
                    .SetShouldDetectMonitorScaleChanges(false)
                    .map_err(|error| error.to_string())?;
                scaling
                    .SetRasterizationScale(1.0)
                    .map_err(|error| error.to_string())?;
            }
            if let Ok(background) = controller.cast::<ICoreWebView2Controller2>() {
                background
                    .SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                        R: 9,
                        G: 11,
                        B: 20,
                        A: 255,
                    })
                    .map_err(|error| error.to_string())?;
            }
            webview
                .Settings()
                .and_then(|settings| settings.SetAreDevToolsEnabled(true))
                .map_err(|error| error.to_string())?;
            let permission = PermissionRequestedEventHandler::create(Box::new(|_, arguments| {
                if let Some(arguments) = arguments {
                    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                    arguments.PermissionKind(&mut kind)?;
                    if kind == COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ {
                        arguments.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
                    }
                }
                Ok(())
            }));
            let mut permission_token = 0;
            webview
                .add_PermissionRequested(&permission, &mut permission_token)
                .map_err(|error| error.to_string())?;
            self.device.Commit().map_err(|error| error.to_string())?;
        }
        let page = CompositionPage {
            composition,
            controller,
            webview,
            visual,
            transform,
            effects,
            device: self.device.clone(),
            root: self.root.clone(),
            parent: self.parent,
            viewport: Cell::new([0, 0]),
            visible: Cell::new(false),
            corners: Cell::new([[0.0; 2]; 4]),
        };
        trace_webview_step(format_args!("composition.attach_page.end"));
        Ok(page)
    }
}

pub struct CompositionPage {
    composition: ICoreWebView2CompositionController,
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
    visual: IDCompositionVisual,
    transform: IDCompositionMatrixTransform3D,
    effects: IDCompositionEffectGroup,
    device: IDCompositionDevice,
    root: IDCompositionVisual,
    parent: HWND,
    viewport: Cell<[u32; 2]>,
    visible: Cell<bool>,
    corners: Cell<[[f32; 2]; 4]>,
}

impl CompositionPage {
    pub fn webview(&self) -> ICoreWebView2 {
        self.webview.clone()
    }
    pub fn controller(&self) -> ICoreWebView2Controller {
        self.controller.clone()
    }
    pub fn surface_state(&self) -> serde_json::Value {
        serde_json::json!({ "visible":self.visible.get(),"viewport":self.viewport.get(),"corners":self.corners.get() })
    }

    pub fn present(
        &self,
        corners: [[f32; 2]; 4],
        viewport: [u32; 2],
        visible: bool,
    ) -> windows::core::Result<()> {
        let Some(matrix) = projective_matrix(corners, viewport) else {
            return self.set_visible(false);
        };
        unsafe {
            if self.viewport.get() != viewport {
                self.controller.SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: viewport[0] as i32,
                    bottom: viewport[1] as i32,
                })?;
                self.viewport.set(viewport);
            }
            for row in 0..4 {
                for column in 0..4 {
                    self.transform.SetMatrixElement2(
                        row,
                        column,
                        matrix[row as usize][column as usize],
                    )?;
                }
            }
            self.effects.SetOpacity2(if visible { 1.0 } else { 0.0 })?;
            self.controller.SetIsVisible(visible)?;
            self.visible.set(visible);
            self.corners.set(corners);
            self.device.Commit()
        }
    }

    pub fn set_bounds(&self, bounds: Rect) -> windows::core::Result<()> {
        let position: LogicalPosition<f64> = bounds.position.to_logical(1.0);
        let size = bounds.size.to_logical::<f64>(1.0);
        let left = position.x as f32;
        let top = position.y as f32;
        let right = left + size.width as f32;
        let bottom = top + size.height as f32;
        let was_visible = self.visible.get();
        self.present(
            [[left, top], [right, top], [right, bottom], [left, bottom]],
            [size.width.max(1.0) as u32, size.height.max(1.0) as u32],
            was_visible,
        )?;
        Ok(())
    }

    pub fn set_visible(&self, visible: bool) -> windows::core::Result<()> {
        trace_webview_step(format_args!(
            "composition.set_visible.begin visible={visible}"
        ));
        unsafe {
            self.effects.SetOpacity2(if visible { 1.0 } else { 0.0 })?;
            trace_webview_step(format_args!("composition.set_visible.controller.begin"));
            self.controller.SetIsVisible(visible)?;
            trace_webview_step(format_args!("composition.set_visible.controller.end"));
            self.visible.set(visible);
            let result = self.device.Commit();
            trace_webview_step(format_args!(
                "composition.set_visible.end success={}",
                result.is_ok()
            ));
            result
        }
    }

    pub fn conceal(&self) -> windows::core::Result<()> {
        unsafe {
            self.effects.SetOpacity2(0.0)?;
            self.visible.set(false);
            self.device.Commit()
        }
    }

    pub fn focus(&self) -> windows::core::Result<()> {
        trace_webview_step(format_args!("composition.focus.begin"));
        unsafe {
            self.root.RemoveVisual(&self.visual)?;
            self.root
                .AddVisual(&self.visual, true, None::<&IDCompositionVisual>)?;
            trace_webview_step(format_args!("composition.focus.commit.begin"));
            self.device.Commit()?;
            trace_webview_step(format_args!("composition.focus.commit.end"));
            trace_webview_step(format_args!("composition.focus.move_focus.begin"));
            let result = self
                .controller
                .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            trace_webview_step(format_args!(
                "composition.focus.move_focus.end success={}",
                result.is_ok()
            ));
            result
        }
    }

    pub fn focus_parent(&self) -> windows::core::Result<()> {
        unsafe {
            SetFocus(Some(self.parent))?;
        }
        Ok(())
    }

    pub fn send_mouse(
        &self,
        kind: WebMouseKind,
        position: [i32; 2],
        modifiers: u32,
        wheel_delta: i32,
    ) -> windows::core::Result<()> {
        if !self.visible.get() {
            return Ok(());
        }
        if matches!(
            kind,
            WebMouseKind::LeftDown
                | WebMouseKind::LeftDoubleClick
                | WebMouseKind::RightDown
                | WebMouseKind::MiddleDown
        ) {
            self.focus()?;
        }
        let event = match kind {
            WebMouseKind::Move => COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
            WebMouseKind::Leave => COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
            WebMouseKind::LeftDown => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
            WebMouseKind::LeftUp => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
            WebMouseKind::LeftDoubleClick => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK,
            WebMouseKind::RightDown => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN,
            WebMouseKind::RightUp => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP,
            WebMouseKind::MiddleDown => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
            WebMouseKind::MiddleUp => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP,
            WebMouseKind::Wheel => COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL,
            WebMouseKind::HorizontalWheel => COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL,
        };
        unsafe {
            self.composition.SendMouseInput(
                event,
                COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(modifiers as i32),
                wheel_delta as u32,
                POINT {
                    x: position[0],
                    y: position[1],
                },
            )?;
            if !matches!(kind, WebMouseKind::Leave) {
                let mut cursor = HCURSOR::default();
                if self.composition.Cursor(&mut cursor).is_ok() {
                    SetCursor(Some(cursor));
                }
            }
            Ok(())
        }
    }

    pub fn load_url(&self, url: &str) -> windows::core::Result<()> {
        unsafe { self.webview.Navigate(&HSTRING::from(url)) }
    }

    pub fn initialize_script(
        &self,
        script: &str,
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        trace_webview_step(format_args!("composition.initialize_script.begin"));
        let (sender, receiver) = mpsc::channel();
        let completed = AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(
            move |status, _| {
                trace_webview_step(format_args!(
                    "composition.initialize_script.callback.begin success={}",
                    status.is_ok()
                ));
                let _ = sender.send(status.map_err(|error| error.to_string()));
                trace_webview_step(format_args!("composition.initialize_script.callback.end"));
                Ok(())
            },
        ));
        trace_webview_step(format_args!("composition.initialize_script.request.begin"));
        unsafe {
            self.webview
                .AddScriptToExecuteOnDocumentCreated(&HSTRING::from(script), &completed)
        }
        .map_err(|error| error.to_string())?;
        trace_webview_step(format_args!("composition.initialize_script.request.end"));
        Ok(receiver)
    }

    pub fn evaluate_script_with_callback(
        &self,
        script: &str,
        callback: impl Fn(String) + Send + 'static,
    ) -> windows::core::Result<()> {
        let completed = ExecuteScriptCompletedHandler::create(Box::new(move |status, result| {
            if let Err(error) = status {
                callback(format!(
                    "{{\"ok\":false,\"error\":{}}}",
                    serde_json::to_string(&error.to_string()).unwrap_or_default()
                ));
            } else {
                callback(result);
            }
            Ok(())
        }));
        unsafe {
            self.webview
                .ExecuteScript(&HSTRING::from(script), &completed)
        }
    }

    pub fn set_editor_locale(&self) -> windows::core::Result<()> {
        unsafe {
            let cookies = self.webview.cast::<ICoreWebView2_2>()?.CookieManager()?;
            let cookie = cookies.CreateCookie(
                &HSTRING::from("vscode.nls.locale"),
                &HSTRING::from("en"),
                &HSTRING::from("127.0.0.1"),
                &HSTRING::from("/"),
            )?;
            cookie.SetSameSite(COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX)?;
            cookies.AddOrUpdateCookie(&cookie)
        }
    }
}

pub fn close_composition_controller(composition: &ICoreWebView2CompositionController) {
    if let Ok(controller) = composition.cast::<ICoreWebView2Controller>() {
        unsafe {
            let _ = controller.Close();
        }
    }
}

impl Drop for CompositionPage {
    fn drop(&mut self) {
        unsafe {
            let _ = self.composition.SetRootVisualTarget(None::<&IUnknown>);
            let _ = self.root.RemoveVisual(&self.visual);
            let _ = self.controller.Close();
            let _ = self.device.Commit();
        }
    }
}

fn projective_matrix(corners: [[f32; 2]; 4], viewport: [u32; 2]) -> Option<[[f32; 4]; 4]> {
    if viewport.contains(&0) || !corners.iter().flatten().all(|value| value.is_finite()) {
        return None;
    }
    let [first, second, third, fourth] = corners;
    let horizontal = [second[0] - third[0], second[1] - third[1]];
    let vertical = [fourth[0] - third[0], fourth[1] - third[1]];
    let perspective = [
        first[0] - second[0] + third[0] - fourth[0],
        first[1] - second[1] + third[1] - fourth[1],
    ];
    let determinant = horizontal[0] * vertical[1] - vertical[0] * horizontal[1];
    if determinant.abs() < 0.001 {
        return None;
    }
    let project_x = (perspective[0] * vertical[1] - vertical[0] * perspective[1]) / determinant;
    let project_y = (horizontal[0] * perspective[1] - perspective[0] * horizontal[1]) / determinant;
    let width = viewport[0] as f32;
    let height = viewport[1] as f32;
    Some([
        [
            (second[0] - first[0] + project_x * second[0]) / width,
            (second[1] - first[1] + project_x * second[1]) / width,
            0.0,
            project_x / width,
        ],
        [
            (fourth[0] - first[0] + project_y * fourth[0]) / height,
            (fourth[1] - first[1] + project_y * fourth[1]) / height,
            0.0,
            project_y / height,
        ],
        [0.0, 0.0, 1.0, 0.0],
        [first[0], first[1], 0.0, 1.0],
    ])
}
