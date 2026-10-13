use bevy::{prelude::*, window::PrimaryWindow, winit::WINIT_WINDOWS};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CapturePreviewCompletedHandler,
    DocumentTitleChangedEventHandler,
    Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
        COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN, ICoreWebView2, ICoreWebView2Controller,
        ICoreWebView2Deferral, ICoreWebView2NewWindowRequestedEventArgs,
    },
    NavigationCompletedEventHandler, NewWindowRequestedEventHandler, SourceChangedEventHandler,
    WindowCloseRequestedEventHandler, take_pwstr,
};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{
        CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, RGN_OR, SetWindowRgn,
    },
    System::Com::{IStream, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET},
    UI::{
        Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_SHIFT, VK_T},
        Shell::SHCreateMemStream,
        WindowsAndMessaging::{HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos},
    },
};
use wry::{
    NewWindowResponse, PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder,
    WebViewBuilderExtWindows, WebViewExtWindows,
    dpi::{LogicalPosition, LogicalSize},
    raw_window_handle::RawWindowHandle,
};

use crate::compositor::{CompositionPage, CompositionRequest, Compositor, trace_webview_step};

#[derive(Clone, Copy)]
pub enum WebMouseKind {
    Move,
    Leave,
    LeftDown,
    LeftUp,
    LeftDoubleClick,
    RightDown,
    RightUp,
    MiddleDown,
    MiddleUp,
    Wheel,
    HorizontalWheel,
}

#[derive(Resource, Default)]
pub struct WebCommands {
    pub actions: Vec<WebAction>,
}

#[derive(Resource, Default)]
pub struct ShellInteraction {
    pub overlay: bool,
    pub palette: Option<bevy::math::Rect>,
}

pub enum WebAction {
    InitShell {
        url: String,
    },
    Open {
        id: String,
        url: String,
    },
    Focus {
        id: String,
    },
    Blur,
    Overview,
    Navigate {
        id: String,
        url: String,
    },
    Back,
    Forward,
    Close {
        id: String,
    },
    Capture {
        id: String,
    },
    Warm {
        id: String,
    },
    Launcher,
    ClosePanel,
    ShellState(Value),
    Inspect {
        id: String,
    },
    Evaluate {
        id: String,
        script: String,
    },
    Surface {
        id: String,
        corners: [[f32; 2]; 4],
        viewport: [u32; 2],
        visible: bool,
    },
    Mouse {
        id: String,
        kind: WebMouseKind,
        position: [i32; 2],
        modifiers: u32,
        wheel_delta: i32,
    },
}

#[derive(Resource, Default)]
pub struct WebEvents {
    pub events: Vec<WebEvent>,
}

pub enum WebEvent {
    Snapshot { id: String, bytes: Vec<u8> },
    Title { id: String, title: String },
    Navigation { id: String, url: String },
    PageLoaded { id: String, url: String },
    Inspection { id: String, result: Value },
    Shell(Value),
    Error { id: String, message: String },
}

enum RuntimeEvent {
    Public(WebEvent),
    Captured {
        id: String,
        generation: u64,
        request: u64,
        result: Result<Vec<u8>, String>,
    },
    Launcher,
    PopupOpened,
    PopupClosed {
        id: u64,
    },
    PopupTitle {
        id: u64,
        title: String,
    },
}

struct PopupRequest {
    owner: String,
    parent: bevy::window::RawHandleWrapper,
    opener: wry::NewWindowOpener,
    arguments: ICoreWebView2NewWindowRequestedEventArgs,
    deferral: ICoreWebView2Deferral,
}

struct NativePopup {
    id: u64,
    owner: String,
    title: String,
    view: WebView,
}

struct PopupPool {
    views: Vec<NativePopup>,
    active_owner: Option<String>,
    next_id: u64,
    size: (f64, f64),
}

struct PendingPage {
    url: String,
    generation: u64,
    started: Instant,
    phase: Option<PendingPagePhase>,
    surface: Option<PageSurface>,
}

struct PageSurface {
    corners: [[f32; 2]; 4],
    viewport: [u32; 2],
    visible: bool,
}

enum PendingPagePhase {
    Controller(CompositionRequest),
    Script {
        page: CompositionPage,
        receiver: Receiver<Result<(), String>>,
    },
}

struct WebRuntime {
    shell: Option<WebView>,
    pages: HashMap<String, CompositionPage>,
    pending_pages: HashMap<String, PendingPage>,
    compositor: Option<Compositor>,
    popups: Rc<RefCell<PopupPool>>,
    popup_requests: Rc<RefCell<Vec<PopupRequest>>>,
    context: WebContext,
    sender: Sender<RuntimeEvent>,
    receiver: Receiver<RuntimeEvent>,
    active: Option<String>,
    desired_active: Option<String>,
    pending_hide: HashSet<String>,
    capturing: HashMap<String, u64>,
    page_generations: HashMap<String, u64>,
    next_generation: u64,
    next_capture: u64,
    capture_deadlines: HashMap<String, Instant>,
    capture_after: HashMap<String, Instant>,
    warm_queue: VecDeque<String>,
    warming: Option<String>,
    warm_deadline: Option<Instant>,
    loaded_pages: HashSet<String>,
    shell_state: Value,
    shell_height: f64,
    shell_overlay: bool,
    shell_palette: Option<[f64; 5]>,
    size: (f64, f64),
    physical_size: (f64, f64),
    expanded: Rc<Cell<bool>>,
}

impl Default for WebRuntime {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        let profile_directory = std::env::var_os("ORBIT_RUST_PROFILE_DIRECTORY")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir)
                    .join("OrbitRust")
                    .join("WebView2")
            });
        Self {
            shell: None,
            pages: HashMap::new(),
            pending_pages: HashMap::new(),
            compositor: None,
            popups: Rc::new(RefCell::new(PopupPool {
                views: Vec::new(),
                active_owner: None,
                next_id: 0,
                size: (1280.0, 800.0),
            })),
            context: WebContext::new(Some(profile_directory)),
            popup_requests: Rc::new(RefCell::new(Vec::new())),
            sender,
            receiver,
            active: None,
            desired_active: None,
            pending_hide: HashSet::new(),
            capturing: HashMap::new(),
            page_generations: HashMap::new(),
            next_generation: 0,
            next_capture: 0,
            capture_deadlines: HashMap::new(),
            capture_after: HashMap::new(),
            warm_queue: VecDeque::new(),
            warming: None,
            warm_deadline: None,
            loaded_pages: HashSet::new(),
            shell_state: json!({}),
            shell_height: 64.0,
            shell_overlay: false,
            shell_palette: None,
            size: (1280.0, 800.0),
            physical_size: (1280.0, 800.0),
            expanded: Rc::new(Cell::new(false)),
        }
    }
}

impl Drop for WebRuntime {
    fn drop(&mut self) {
        self.popups.borrow_mut().views.clear();
    }
}

pub struct WebviewsPlugin;

const OBSERVE_PAGE_ERRORS: &str = r#"
(() => {
    window.__orbitPageErrors = [];
    const record = message => {
        window.__orbitPageErrors.push(String(message).slice(0, 1200));
        if (window.__orbitPageErrors.length > 20) window.__orbitPageErrors.shift();
    };
    window.addEventListener('error', event => record(event.message || event.target?.src || event.target?.href || 'Resource load failed'), true);
    window.addEventListener('unhandledrejection', event => record(event.reason?.message || event.reason));
})()
"#;

const INSPECT_PAGE: &str = r#"
(() => {
    try {
        const workbench = document.querySelector('.monaco-workbench');
        const workbenchBounds = workbench?.getBoundingClientRect();
        const editorGroup = document.querySelector('.editor-group-container');
        const editorGroupBounds = editorGroup?.getBoundingClientRect();
        const terminalBuffer = typeof terminal !== 'undefined' ? terminal.buffer?.active : null;
        const terminalEnd = terminalBuffer ? Math.min(terminalBuffer.length, terminalBuffer.baseY + terminalBuffer.cursorY + 1) : 0;
        const terminalStart = Math.max(0, terminalEnd - 40);
        return {
            ok: true,
            url: location.href,
            title: document.title,
            readyState: document.readyState,
            viewport: { width: innerWidth, height: innerHeight },
            hasChat: Boolean(document.querySelector('#composer')),
            hasTerminal: Boolean(document.querySelector('.xterm')),
            hasEditor: Boolean(document.querySelector('.monaco-workbench')),
            terminalReady: typeof shellReady !== 'undefined' && shellReady && !restoringTranscript && socket.readyState === WebSocket.OPEN && !terminal.options.disableStdin,
            terminalSocketState: typeof socket !== 'undefined' ? socket.readyState : null,
            terminalOutput: terminalBuffer ? Array.from({ length: terminalEnd - terminalStart }, (_, index) => terminalBuffer.getLine(terminalStart + index)?.translateToString(true) || '').join('\n') : null,
            chatReady: typeof runtimeReady !== 'undefined' && runtimeReady && conversationReady,
            chatBridge: typeof window.orbitChat === 'object',
            chatProvider: typeof selectedProvider === 'function' ? selectedProvider() : null,
            chatProviderDisabled: document.querySelector('#chat-provider')?.disabled,
            editorReady: Boolean(document.getElementById('workbench.parts.editor')) && Boolean(document.querySelector('.monaco-workbench')),
            editorPartsReady: Boolean(document.getElementById('workbench.parts.editor')) && Boolean(document.getElementById('workbench.parts.statusbar')) && editorGroupBounds?.width > 100 && editorGroupBounds?.height > 100 && workbenchBounds?.width > 600 && workbenchBounds?.height > 300,
            editorStatus: document.getElementById('workbench.parts.statusbar')?.innerText,
            editorTitle: document.title,
            hasQa: Boolean(document.querySelector('#qa-counter')),
            qaInput: document.querySelector('#qa-input')?.value,
            qaValue: document.querySelector('#qa-value')?.textContent,
            qaCounter: document.querySelector('#qa-counter')?.textContent,
            qaCounterRect: (() => { const bounds = document.querySelector('#qa-counter')?.getBoundingClientRect(); return bounds ? {x:bounds.x,y:bounds.y,width:bounds.width,height:bounds.height} : null; })(),
            qaTrustedClick: window.orbitQaTrustedClick || null,
            qaPointer: window.orbitQaPointer || null,
            qaInputRect: (() => { const bounds = document.querySelector('#qa-input')?.getBoundingClientRect(); return bounds ? {x:bounds.x,y:bounds.y,width:bounds.width,height:bounds.height} : null; })(),
            qaAuthPopupRect: (() => { const bounds = document.querySelector('#qa-auth-popup')?.getBoundingClientRect(); return bounds ? {x:bounds.x,y:bounds.y,width:bounds.width,height:bounds.height} : null; })(),
            qaLastClick: window.orbitQaLastClick || null,
            qaAuthClick: window.orbitQaAuthClick || null,
            bodyExcerpt: document.body.innerText.slice(0, 1600),
            engineStatus: document.querySelector('#engine-status')?.textContent,
            terminalStatus: document.querySelector('#status')?.textContent,
            terminalStatusHidden: document.querySelector('#status')?.hidden,
            pageErrors: window.__orbitPageErrors || [],
            smokeMarker: window.orbitSmokeMarker || null,
            authPopupOpened: Boolean(window.orbitSmokePopup),
            authPopupClosed: window.orbitSmokePopup?.closed || false,
            authPopupReply: window.orbitSmokeAuthReply || null
        };
    } catch (error) {
        return { ok: false, error: String(error) };
    }
})()
"#;

impl Plugin for WebviewsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WebCommands>()
            .init_resource::<WebEvents>()
            .init_resource::<ShellInteraction>()
            .insert_non_send_resource(WebRuntime::default())
            .add_systems(PreUpdate, update_webviews)
            .add_systems(
                PostUpdate,
                update_webviews.after(crate::surfaces::update_surface),
            );
    }
}

fn bounds(width: f64, height: f64, top: f64) -> Rect {
    Rect {
        position: LogicalPosition::new(0.0, top).into(),
        size: LogicalSize::new(width.max(1.0), height.max(1.0)).into(),
    }
}

fn palette_geometry(palette: &Value) -> Option<[f64; 5]> {
    Some([
        palette["x"].as_f64()?,
        palette["y"].as_f64()?,
        palette["width"].as_f64()?,
        palette["height"].as_f64()?,
        palette["radius"].as_f64().unwrap_or(0.0),
    ])
}

impl WebRuntime {
    fn create_view(
        &mut self,
        entity: Entity,
        id: Option<&str>,
        url: &str,
    ) -> Result<WebView, String> {
        let sender = self.sender.clone();
        let title_sender = self.sender.clone();
        let load_sender = self.sender.clone();
        let title_id = id.unwrap_or("shell").to_owned();
        let load_id = title_id.clone();
        let page_id = title_id.clone();
        let is_shell = id.is_none();
        let shell_url = url.to_owned();
        let view_bounds = if is_shell {
            bounds(self.size.0, self.shell_height, 0.0)
        } else {
            bounds(self.size.0, self.size.1 - 64.0, 64.0)
        };
        let builder = WebViewBuilder::new_with_web_context(&mut self.context)
            .with_bounds(view_bounds)
            .with_visible(is_shell)
            .with_focused(false)
            .with_transparent(is_shell)
            .with_background_color((9, 11, 20, 255))
            .with_clipboard(true)
            .with_devtools(true)
            .with_initialization_script(OBSERVE_PAGE_ERRORS)
            .with_navigation_handler(move |candidate| !is_shell || candidate == shell_url)
            .with_document_title_changed_handler(move |title| {
                let _ = title_sender.send(RuntimeEvent::Public(WebEvent::Title {
                    id: title_id.clone(),
                    title,
                }));
            })
            .with_on_page_load_handler(move |event, url| {
                let public = match event {
                    PageLoadEvent::Started => WebEvent::Navigation {
                        id: load_id.clone(),
                        url,
                    },
                    PageLoadEvent::Finished => WebEvent::PageLoaded {
                        id: load_id.clone(),
                        url,
                    },
                };
                let _ = load_sender.send(RuntimeEvent::Public(public));
            });
        let builder = if is_shell {
            builder.with_url(url).with_ipc_handler(move |request| {
                if let Ok(message) = serde_json::from_str::<Value>(request.body()) {
                    let _ = sender.send(RuntimeEvent::Public(WebEvent::Shell(message)));
                }
            })
        } else {
            builder
        };
        let handle = WINIT_WINDOWS.with(|windows| {
            let windows = windows.borrow();
            let window = windows
                .get_window(entity)
                .ok_or_else(|| "Finestra nativa non ancora disponibile".to_owned())?;
            bevy::window::RawHandleWrapper::new(window).map_err(|error| error.to_string())
        })?;
        let builder = if !is_shell {
            let popup_parent = handle.clone();
            let popup_owner = page_id.clone();
            let popups = self.popups.clone();
            let popup_sender = self.sender.clone();
            builder.with_new_window_req_handler(move |_, features| {
                create_popup(
                    popup_owner.clone(),
                    popup_parent.clone(),
                    popups.clone(),
                    popup_sender.clone(),
                    features.opener,
                )
            })
        } else {
            builder
        };
        let handle = unsafe { handle.get_handle() };
        let view = builder
            .build_as_child(&handle)
            .map_err(|error| error.to_string())?;
        if !is_shell {
            let locale = wry::cookie::Cookie::build(("vscode.nls.locale", "en"))
                .domain("127.0.0.1")
                .path("/")
                .same_site(wry::cookie::SameSite::Lax)
                .build();
            view.set_cookie(&locale)
                .map_err(|error| error.to_string())?;
            attach_shortcuts(
                view.controller(),
                self.sender.clone(),
                self.expanded.clone(),
            )
            .map_err(|error| format!("{page_id}: {error}"))?;
            attach_navigation(view.webview(), page_id, self.sender.clone())
                .map_err(|error| error.to_string())?;
            view.load_url(url).map_err(|error| error.to_string())?;
        }
        Ok(view)
    }

    fn request_page(&mut self, entity: Entity, id: &str, url: &str) -> Result<(), String> {
        trace_webview_step(format_args!("runtime.request_page.begin id={id}"));
        let parent = WINIT_WINDOWS.with(|windows| {
            let windows = windows.borrow();
            let window = windows
                .get_window(entity)
                .ok_or_else(|| "Finestra nativa non ancora disponibile".to_owned())?;
            bevy::window::RawHandleWrapper::new(window).map_err(|error| error.to_string())
        })?;
        if self.compositor.is_none() {
            let RawWindowHandle::Win32(handle) = parent.get_window_handle() else {
                return Err("Handle Windows non disponibile".to_owned());
            };
            self.compositor = Some(
                Compositor::new(HWND(handle.hwnd.get() as *mut _))
                    .map_err(|error| error.to_string())?,
            );
            self.clip_compositor();
        }
        let environment = self
            .shell
            .as_ref()
            .ok_or_else(|| "Toolbar non ancora disponibile".to_owned())?
            .environment();
        let request = self
            .compositor
            .as_ref()
            .unwrap()
            .request_page(&environment)?;
        self.next_generation += 1;
        let generation = self.next_generation;
        self.page_generations.insert(id.to_owned(), generation);
        self.pending_pages.insert(
            id.to_owned(),
            PendingPage {
                url: url.to_owned(),
                generation,
                started: Instant::now(),
                phase: Some(PendingPagePhase::Controller(request)),
                surface: None,
            },
        );
        trace_webview_step(format_args!("runtime.request_page.end id={id}"));
        Ok(())
    }

    fn configure_page(
        &self,
        entity: Entity,
        id: &str,
        page: &CompositionPage,
    ) -> Result<(), String> {
        trace_webview_step(format_args!("runtime.configure_page.begin id={id}"));
        let parent = WINIT_WINDOWS.with(|windows| {
            let windows = windows.borrow();
            let window = windows
                .get_window(entity)
                .ok_or_else(|| "Finestra nativa non ancora disponibile".to_owned())?;
            bevy::window::RawHandleWrapper::new(window).map_err(|error| error.to_string())
        })?;
        let environment = self
            .shell
            .as_ref()
            .ok_or_else(|| "Toolbar non ancora disponibile".to_owned())?
            .environment();
        page.set_editor_locale()
            .map_err(|error| error.to_string())?;
        attach_shortcuts(
            page.controller(),
            self.sender.clone(),
            self.expanded.clone(),
        )
        .map_err(|error| error.to_string())?;
        attach_navigation(page.webview(), id.to_owned(), self.sender.clone())
            .map_err(|error| error.to_string())?;
        let title_id = id.to_owned();
        let title_sender = self.sender.clone();
        let title = DocumentTitleChangedEventHandler::create(Box::new(move |webview, _| {
            if let Some(webview) = webview {
                let mut title = windows::core::PWSTR::null();
                unsafe {
                    webview.DocumentTitle(&mut title)?;
                }
                let _ = title_sender.send(RuntimeEvent::Public(WebEvent::Title {
                    id: title_id.clone(),
                    title: take_pwstr(title),
                }));
            }
            Ok(())
        }));
        let loaded_id = id.to_owned();
        let loaded_sender = self.sender.clone();
        let loaded = NavigationCompletedEventHandler::create(Box::new(move |webview, _| {
            if let Some(webview) = webview {
                let mut source = windows::core::PWSTR::null();
                unsafe {
                    webview.Source(&mut source)?;
                }
                let _ = loaded_sender.send(RuntimeEvent::Public(WebEvent::PageLoaded {
                    id: loaded_id.clone(),
                    url: take_pwstr(source),
                }));
            }
            Ok(())
        }));
        let popup_owner = id.to_owned();
        let popup_requests = self.popup_requests.clone();
        let popup = NewWindowRequestedEventHandler::create(Box::new(move |webview, arguments| {
            let (Some(webview), Some(arguments)) = (webview, arguments) else {
                return Ok(());
            };
            unsafe {
                let deferral = arguments.GetDeferral()?;
                arguments.SetHandled(true)?;
                popup_requests.borrow_mut().push(PopupRequest {
                    owner: popup_owner.clone(),
                    parent: parent.clone(),
                    opener: wry::NewWindowOpener {
                        webview,
                        environment: environment.clone(),
                    },
                    arguments,
                    deferral,
                });
            }
            Ok(())
        }));
        let mut token = 0;
        unsafe {
            let webview = page.webview();
            webview
                .add_DocumentTitleChanged(&title, &mut token)
                .map_err(|error| error.to_string())?;
            webview
                .add_NavigationCompleted(&loaded, &mut token)
                .map_err(|error| error.to_string())?;
            webview
                .add_NewWindowRequested(&popup, &mut token)
                .map_err(|error| error.to_string())?;
        }
        page.set_bounds(bounds(1280.0, 720.0, 0.0))
            .map_err(|error| error.to_string())?;
        trace_webview_step(format_args!("runtime.configure_page.end id={id}"));
        Ok(())
    }

    fn advance_page(
        &self,
        entity: Entity,
        id: &str,
        pending: &mut PendingPage,
    ) -> Result<Option<CompositionPage>, String> {
        let phase = pending
            .phase
            .take()
            .ok_or_else(|| "Inizializzazione WebView2 interrotta".to_owned())?;
        match phase {
            PendingPagePhase::Controller(request) => {
                let Some(controller) = request.poll()? else {
                    pending.phase = Some(PendingPagePhase::Controller(request));
                    return Ok(None);
                };
                let page = self.compositor.as_ref().unwrap().attach_page(controller)?;
                self.configure_page(entity, id, &page)?;
                let receiver = page.initialize_script(OBSERVE_PAGE_ERRORS)?;
                pending.phase = Some(PendingPagePhase::Script { page, receiver });
                pending.started = Instant::now();
                Ok(None)
            }
            PendingPagePhase::Script { page, receiver } => match receiver.try_recv() {
                Ok(result) => {
                    result?;
                    page.load_url(&pending.url)
                        .map_err(|error| error.to_string())?;
                    trace_webview_step(format_args!("runtime.page_ready id={id}"));
                    Ok(Some(page))
                }
                Err(mpsc::TryRecvError::Empty) => {
                    pending.phase = Some(PendingPagePhase::Script { page, receiver });
                    Ok(None)
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("Inizializzazione script WebView2 interrotta".into())
                }
            },
        }
    }

    fn poll_pages(&mut self, entity: Entity, events: &mut Vec<WebEvent>) {
        let identifiers: Vec<_> = self.pending_pages.keys().cloned().collect();
        for id in identifiers {
            let Some(mut pending) = self.pending_pages.remove(&id) else {
                continue;
            };
            if self.page_generations.get(&id) != Some(&pending.generation) {
                continue;
            }
            if pending.started.elapsed() >= Duration::from_secs(30) {
                self.fail_page(id, "Inizializzazione WebView2 scaduta".into(), events);
                continue;
            }
            match self.advance_page(entity, &id, &mut pending) {
                Ok(Some(page)) => {
                    if let Some(surface) = &pending.surface {
                        if let Err(error) = page.present(surface.corners, surface.viewport, false) {
                            self.fail_page(id, error.to_string(), events);
                            continue;
                        }
                    }
                    self.pages.insert(id.clone(), page);
                    self.raise_shell();
                    self.sync_popups(false);
                    if self.desired_active.as_ref() == Some(&id)
                        && pending
                            .surface
                            .as_ref()
                            .is_some_and(|surface| surface.visible)
                    {
                        if let Err(message) =
                            self.run_action(entity, WebAction::Focus { id: id.clone() })
                        {
                            events.push(WebEvent::Error { id, message });
                        }
                    }
                }
                Ok(None) => {
                    self.pending_pages.insert(id, pending);
                }
                Err(message) => self.fail_page(id, message, events),
            }
        }
    }

    fn fail_page(&mut self, id: String, message: String, events: &mut Vec<WebEvent>) {
        self.page_generations.remove(&id);
        self.warm_queue.retain(|queued| queued != &id);
        if self.desired_active.as_ref() == Some(&id) {
            self.desired_active = None;
        }
        events.push(WebEvent::Error { id, message });
    }

    fn remember_surface(
        &mut self,
        id: &str,
        corners: [[f32; 2]; 4],
        viewport: [u32; 2],
        visible: bool,
    ) {
        if let Some(pending) = self.pending_pages.get_mut(id) {
            pending.surface = Some(PageSurface {
                corners,
                viewport,
                visible,
            });
        }
    }

    fn raise_shell(&self) {
        if let Some(shell) = &self.shell {
            raise_view(shell);
        }
    }

    fn resize(&mut self, size: (f64, f64)) -> Result<(), String> {
        self.size = size;
        self.clip_compositor();
        {
            let mut popups = self.popups.borrow_mut();
            popups.size = size;
            for popup in &popups.views {
                let _ = popup.view.set_bounds(bounds(size.0, size.1 - 64.0, 64.0));
            }
        }
        if let Some(shell) = &self.shell {
            let height = if self.shell_overlay {
                size.1
            } else {
                self.shell_height.min(size.1)
            };
            let _ = shell.set_bounds(bounds(size.0, height, 0.0));
        }
        if let Some(view) = self.warming.as_ref().and_then(|id| self.pages.get(id)) {
            let _ = view.set_bounds(self.warm_bounds());
        }
        self.raise_shell();
        self.clip_shell().map_err(|error| error.to_string())
    }

    fn clip_shell(&self) -> windows::core::Result<()> {
        let Some(shell) = &self.shell else {
            return Ok(());
        };
        let mut handle = HWND::default();
        unsafe {
            shell.controller().ParentWindow(&mut handle)?;
            if !self.shell_overlay {
                if SetWindowRgn(handle, None, true) == 0 {
                    return Err(windows::core::Error::from_win32());
                }
                return Ok(());
            }
            let scale = self.physical_size.1 / self.size.1.max(1.0);
            let region = CreateRectRgn(
                0,
                0,
                self.physical_size.0.ceil() as i32,
                (64.0 * scale).ceil() as i32,
            );
            if region.is_invalid() {
                return Err(windows::core::Error::from_win32());
            }
            if let Some([x, y, width, height, radius]) = self.shell_palette {
                let diameter = (radius * 2.0 * scale).ceil() as i32;
                let palette = CreateRoundRectRgn(
                    (x * scale).floor() as i32,
                    (y * scale).floor() as i32,
                    ((x + width) * scale).ceil() as i32,
                    ((y + height) * scale).ceil() as i32,
                    diameter,
                    diameter,
                );
                if palette.is_invalid() {
                    let error = windows::core::Error::from_win32();
                    let _ = DeleteObject(region.into());
                    return Err(error);
                }
                let combined = CombineRgn(Some(region), Some(region), Some(palette), RGN_OR);
                let _ = DeleteObject(palette.into());
                if combined.0 == 0 {
                    let error = windows::core::Error::from_win32();
                    let _ = DeleteObject(region.into());
                    return Err(error);
                }
            }
            if SetWindowRgn(handle, Some(region), true) == 0 {
                let error = windows::core::Error::from_win32();
                let _ = DeleteObject(region.into());
                return Err(error);
            }
        }
        Ok(())
    }

    fn sync_popups(&self, focus: bool) {
        let focus = focus && !self.shell_overlay;
        let mut popups = self.popups.borrow_mut();
        popups.active_owner = self.active.clone();
        for popup in &popups.views {
            let _ = popup
                .view
                .set_visible(Some(&popup.owner) == self.active.as_ref());
        }
        let current = popups
            .views
            .iter()
            .rev()
            .find(|popup| Some(&popup.owner) == self.active.as_ref());
        if let Some(popup) = current {
            raise_view(&popup.view);
            if focus {
                let _ = popup.view.focus();
            }
        } else if focus {
            if let Some(view) = self.active.as_ref().and_then(|id| self.pages.get(id)) {
                let _ = view.focus();
            }
        }
        if let Some(shell) = &self.shell {
            let title = current.map(|popup| popup.title.clone());
            let _ = shell.evaluate_script(&format!(
                "window.orbitPopupReceive && window.orbitPopupReceive({});",
                json!({ "title": title, "open": current.is_some() })
            ));
        }
        self.raise_shell();
        if self.shell_overlay {
            if let Some(shell) = &self.shell {
                let _ = shell.focus();
            }
        }
    }

    fn close_popup(&mut self, id: Option<u64>) {
        let mut popups = self.popups.borrow_mut();
        let selected = id.or_else(|| {
            popups
                .views
                .iter()
                .rev()
                .find(|popup| Some(&popup.owner) == self.active.as_ref())
                .map(|popup| popup.id)
        });
        if let Some(id) = selected {
            popups.views.retain(|popup| popup.id != id);
        }
        drop(popups);
        self.sync_popups(true);
    }

    fn update_shell(&self) {
        if let Some(shell) = &self.shell {
            let _ = shell.evaluate_script(&format!(
                "window.orbitReceive && window.orbitReceive({});",
                self.shell_state
            ));
        }
    }

    fn show_launcher(&self) {
        if let Some(shell) = &self.shell {
            let _ =
                shell.evaluate_script("window.orbitOpenLauncher && window.orbitOpenLauncher();");
            let _ = shell.focus();
        }
    }

    fn capture(&mut self, id: &str) -> Result<(), String> {
        if self.capturing.contains_key(id) {
            return Ok(());
        }
        let Some(view) = self.pages.get(id) else {
            return Ok(());
        };
        let stream = unsafe { SHCreateMemStream(None) }
            .ok_or_else(|| "Impossibile creare lo stream per l’anteprima".to_owned())?;
        self.next_capture += 1;
        let request = self.next_capture;
        let generation = self.page_generations.get(id).copied().unwrap_or_default();
        self.capturing.insert(id.to_owned(), request);
        self.capture_deadlines
            .insert(id.to_owned(), Instant::now() + Duration::from_secs(3));
        let output_stream = stream.clone();
        let sender = self.sender.clone();
        let captured_id = id.to_owned();
        let handler = CapturePreviewCompletedHandler::create(Box::new(move |status| {
            let result = status
                .and_then(|_| read_stream(&output_stream))
                .map_err(|error| error.to_string());
            let _ = sender.send(RuntimeEvent::Captured {
                id: captured_id,
                generation,
                request,
                result,
            });
            Ok(())
        }));
        let result = unsafe {
            view.webview().CapturePreview(
                COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                &stream,
                &handler,
            )
        };
        if let Err(error) = result {
            self.capturing.remove(id);
            self.capture_deadlines.remove(id);
            return Err(error.to_string());
        }
        Ok(())
    }

    fn leave_focus(&mut self) {
        let Some(id) = self.active.take() else {
            return;
        };
        self.pending_hide.insert(id.clone());
        if let Some(view) = self.pages.get(&id) {
            let _ = view.focus_parent();
        }
        self.sync_popups(false);
        if self.capture(&id).is_err() {
            self.hide(&id);
        } else if self
            .pages
            .get(&id)
            .is_some_and(|view| view.conceal().is_err())
        {
            self.hide(&id);
        }
    }

    fn hide(&mut self, id: &str) {
        self.pending_hide.remove(id);
        if self.active.as_deref() != Some(id) {
            if let Some(view) = self.pages.get(id) {
                let _ = view.set_visible(false);
                if self.active.is_none() {
                    let _ = view.focus_parent();
                }
            }
        }
    }

    fn warm_bounds(&self) -> Rect {
        Rect {
            position: LogicalPosition::new(
                (self.physical_size.0 - 2.0).max(0.0),
                (self.physical_size.1 - 2.0).max(0.0),
            )
            .into(),
            size: LogicalSize::new(1280.0, 720.0).into(),
        }
    }

    fn finish_warm(&mut self, requeue: bool) {
        let Some(id) = self.warming.take() else {
            return;
        };
        self.warm_deadline = None;
        self.capture_after.remove(&id);
        self.capture_deadlines.remove(&id);
        self.capturing.remove(&id);
        if let Some(view) = self.pages.get(&id) {
            let _ = view.set_visible(false);
            if requeue && !self.warm_queue.contains(&id) {
                self.warm_queue.push_front(id);
            }
        }
    }

    fn start_warm(&mut self) {
        if self.active.is_some() || self.warming.is_some() || !self.pending_hide.is_empty() {
            return;
        }
        for _ in 0..self.warm_queue.len() {
            let Some(id) = self.warm_queue.pop_front() else {
                break;
            };
            let Some(view) = self.pages.get(&id) else {
                if self.pending_pages.contains_key(&id) {
                    self.warm_queue.push_back(id);
                }
                continue;
            };
            let preparation = view
                .set_bounds(self.warm_bounds())
                .and_then(|_| view.set_visible(true));
            if let Err(error) = preparation {
                let _ = view.set_visible(false);
                eprintln!("Anteprima iniziale non disponibile per {id}: {error}");
                continue;
            }
            let page_loaded = self.loaded_pages.contains(&id);
            if page_loaded {
                self.capture_after
                    .insert(id.clone(), Instant::now() + Duration::from_secs(1));
            }
            self.warming = Some(id);
            self.warm_deadline = if page_loaded {
                None
            } else {
                Some(Instant::now() + Duration::from_secs(8))
            };
            self.raise_shell();
            break;
        }
    }

    fn run_action(&mut self, entity: Entity, action: WebAction) -> Result<(), String> {
        match action {
            WebAction::InitShell { url } => {
                if self.shell.is_none() {
                    self.shell = Some(self.create_view(entity, None, &url)?);
                    self.update_shell();
                }
            }
            WebAction::Open { id, url } => {
                if !self.pages.contains_key(&id) && !self.pending_pages.contains_key(&id) {
                    self.request_page(entity, &id, &url)?;
                }
            }
            WebAction::Focus { id } => {
                trace_webview_step(format_args!("runtime.focus.begin id={id}"));
                self.desired_active = Some(id.clone());
                let requeue = self.warming.as_ref().is_some_and(|warming| warming != &id);
                self.finish_warm(requeue);
                self.warm_queue.retain(|queued| queued != &id);
                if self.active.as_ref() != Some(&id) {
                    self.leave_focus();
                    if let Some(view) = self.pages.get(&id) {
                        view.set_visible(true).map_err(|error| error.to_string())?;
                        if !self.shell_overlay {
                            view.focus().map_err(|error| error.to_string())?;
                        }
                        self.active = Some(id.clone());
                        self.pending_hide.remove(&id);
                        self.capture_after
                            .insert(id, Instant::now() + Duration::from_millis(700));
                    }
                    self.raise_shell();
                    self.sync_popups(true);
                } else if !self.shell_overlay {
                    if let Some(view) = self.pages.get(&id) {
                        view.focus().map_err(|error| error.to_string())?;
                    }
                }
                if self.shell_overlay {
                    if let Some(shell) = &self.shell {
                        shell.focus().map_err(|error| error.to_string())?;
                    }
                }
                trace_webview_step(format_args!("runtime.focus.end"));
            }
            WebAction::Surface {
                id,
                corners,
                viewport,
                visible,
            } => {
                if self.pending_hide.contains(&id) && self.active.as_ref() != Some(&id) {
                    return Ok(());
                }
                if let Some(view) = self.pages.get(&id) {
                    view.present(
                        corners,
                        viewport,
                        visible && self.active.as_ref() == Some(&id),
                    )
                    .map_err(|error| error.to_string())?;
                    if visible
                        && self.desired_active.as_ref() == Some(&id)
                        && self.active.as_ref() != Some(&id)
                    {
                        self.run_action(entity, WebAction::Focus { id })?;
                    }
                }
            }
            WebAction::Mouse {
                id,
                kind,
                position,
                modifiers,
                wheel_delta,
            } => {
                if self.active.as_ref() == Some(&id) {
                    if let Some(view) = self.pages.get(&id) {
                        view.send_mouse(kind, position, modifiers, wheel_delta)
                            .map_err(|error| error.to_string())?;
                    }
                }
            }
            WebAction::Blur => {
                self.desired_active = None;
                if let Some(view) = self.active.as_ref().and_then(|id| self.pages.get(id)) {
                    view.focus_parent().map_err(|error| error.to_string())?;
                }
            }
            WebAction::Overview => {
                self.desired_active = None;
                self.leave_focus();
            }
            WebAction::Navigate { id, url } => {
                if let Some(view) = self.pages.get(&id) {
                    view.load_url(&url).map_err(|error| error.to_string())?;
                } else if let Some(pending) = self.pending_pages.get_mut(&id) {
                    pending.url = url;
                }
            }
            WebAction::Back => {
                if let Some(view) = self.active.as_ref().and_then(|id| self.pages.get(id)) {
                    unsafe { view.webview().GoBack() }.map_err(|error| error.to_string())?;
                }
            }
            WebAction::Forward => {
                if let Some(view) = self.active.as_ref().and_then(|id| self.pages.get(id)) {
                    unsafe { view.webview().GoForward() }.map_err(|error| error.to_string())?;
                }
            }
            WebAction::Close { id } => {
                if self.desired_active.as_ref() == Some(&id) {
                    self.desired_active = None;
                }
                self.pending_pages.remove(&id);
                if self.warming.as_ref() == Some(&id) {
                    self.finish_warm(false);
                }
                self.warm_queue.retain(|queued| queued != &id);
                self.loaded_pages.remove(&id);
                if self.active.as_ref() == Some(&id) {
                    if let Some(view) = self.pages.get(&id) {
                        let _ = view.focus_parent();
                    }
                    self.active = None;
                }
                self.capture_after.remove(&id);
                self.capture_deadlines.remove(&id);
                self.capturing.remove(&id);
                self.page_generations.remove(&id);
                self.pending_hide.remove(&id);
                self.pages.remove(&id);
                self.popups
                    .borrow_mut()
                    .views
                    .retain(|popup| popup.owner != id);
                self.sync_popups(true);
            }
            WebAction::Capture { id } => self.capture(&id)?,
            WebAction::Warm { id } => {
                if (self.pages.contains_key(&id) || self.pending_pages.contains_key(&id))
                    && self.active.as_ref() != Some(&id)
                    && self.warming.as_ref() != Some(&id)
                    && !self.warm_queue.contains(&id)
                {
                    self.warm_queue.push_back(id);
                }
            }
            WebAction::Launcher => self.show_launcher(),
            WebAction::ClosePanel => {
                if let Some(shell) = &self.shell {
                    shell
                        .evaluate_script("window.orbitClosePanel?.()")
                        .map_err(|error| error.to_string())?;
                }
            }
            WebAction::ShellState(state) => {
                self.expanded
                    .set(state["expanded"].as_bool().unwrap_or(false));
                self.shell_state = state;
                self.update_shell();
            }
            WebAction::Inspect { id } => {
                self.run_action(
                    entity,
                    WebAction::Evaluate {
                        id,
                        script: INSPECT_PAGE.to_owned(),
                    },
                )?;
            }
            WebAction::Evaluate { id, script } => {
                let sender = self.sender.clone();
                let inspection_id = id.clone();
                let callback = move |result: String| {
                    let result = serde_json::from_str(&result).unwrap_or(Value::String(result));
                    let _ = sender.send(RuntimeEvent::Public(WebEvent::Inspection {
                        id: inspection_id.clone(),
                        result,
                    }));
                };
                if let Some(view) = self.pages.get(&id) {
                    view.evaluate_script_with_callback(&script, callback)
                        .map_err(|error| error.to_string())?;
                    return Ok(());
                }
                let popup_pool = self.popups.borrow();
                let view = if id == "popup" {
                    popup_pool
                        .views
                        .iter()
                        .rev()
                        .find(|popup| Some(&popup.owner) == self.active.as_ref())
                        .map(|popup| &popup.view)
                } else if id == "shell" {
                    self.shell.as_ref()
                } else {
                    None
                };
                if let Some(view) = view {
                    view.evaluate_script_with_callback(&script, callback)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
        Ok(())
    }

    fn clip_compositor(&self) {
        if let Some(compositor) = &self.compositor {
            let scale = self.physical_size.1 / self.size.1.max(1.0);
            let top = if self.shell_overlay {
                64.0
            } else {
                self.shell_height
            };
            let _ = compositor.clip(
                self.physical_size.0 as f32,
                self.physical_size.1 as f32,
                (top * scale) as f32,
            );
        }
    }
}

fn create_popup(
    owner: String,
    parent: bevy::window::RawHandleWrapper,
    popups: Rc<RefCell<PopupPool>>,
    sender: Sender<RuntimeEvent>,
    opener: wry::NewWindowOpener,
) -> NewWindowResponse {
    let (id, size, visible) = {
        let mut pool = popups.borrow_mut();
        pool.next_id += 1;
        (
            pool.next_id,
            pool.size,
            pool.active_owner.as_ref() == Some(&owner),
        )
    };
    let title_sender = sender.clone();
    let child_parent = parent.clone();
    let child_owner = owner.clone();
    let child_popups = popups.clone();
    let child_sender = sender.clone();
    let builder = WebViewBuilder::new()
        .with_environment(opener.environment)
        .with_bounds(bounds(size.0, size.1 - 64.0, 64.0))
        .with_visible(visible)
        .with_focused(visible)
        .with_background_color((9, 11, 20, 255))
        .with_clipboard(true)
        .with_devtools(true)
        .with_initialization_script(OBSERVE_PAGE_ERRORS)
        .with_document_title_changed_handler(move |title| {
            let _ = title_sender.send(RuntimeEvent::PopupTitle { id, title });
        })
        .with_new_window_req_handler(move |_, nested| {
            create_popup(
                child_owner.clone(),
                child_parent.clone(),
                child_popups.clone(),
                child_sender.clone(),
                nested.opener,
            )
        });
    let handle = unsafe { parent.get_handle() };
    let view = match builder.build_as_child(&handle) {
        Ok(view) => view,
        Err(error) => {
            eprintln!("Impossibile aprire la finestra del browser: {error}");
            return NewWindowResponse::Deny;
        }
    };
    if let Err(error) = attach_popup_controls(&view, id, sender.clone()) {
        eprintln!("Impossibile collegare la finestra del browser: {error}");
        return NewWindowResponse::Deny;
    }
    let webview = view.webview();
    popups.borrow_mut().views.push(NativePopup {
        id,
        owner,
        title: "Finestra del browser".to_owned(),
        view,
    });
    let _ = sender.send(RuntimeEvent::PopupOpened);
    NewWindowResponse::Create { webview }
}

fn attach_popup_controls(
    view: &WebView,
    id: u64,
    sender: Sender<RuntimeEvent>,
) -> windows::core::Result<()> {
    let closed_sender = sender.clone();
    let closed = WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
        let _ = closed_sender.send(RuntimeEvent::PopupClosed { id });
        Ok(())
    }));
    let mut close_token = 0;
    unsafe {
        view.webview()
            .add_WindowCloseRequested(&closed, &mut close_token)?;
    }
    let keys = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, arguments| {
        let Some(arguments) = arguments else {
            return Ok(());
        };
        unsafe {
            let mut kind = COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN;
            arguments.KeyEventKind(&mut kind)?;
            if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
            {
                return Ok(());
            }
            let mut key = 0;
            arguments.VirtualKey(&mut key)?;
            if key == VK_ESCAPE.0 as u32 {
                arguments.SetHandled(true)?;
                let _ = sender.send(RuntimeEvent::PopupClosed { id });
            } else if is_launcher_shortcut(key) {
                arguments.SetHandled(true)?;
                let _ = sender.send(RuntimeEvent::Launcher);
            }
        }
        Ok(())
    }));
    let mut key_token = 0;
    unsafe {
        view.controller()
            .add_AcceleratorKeyPressed(&keys, &mut key_token)
    }
}

fn raise_view(view: &WebView) {
    let controller = view.controller();
    let mut handle = HWND::default();
    unsafe {
        if controller.ParentWindow(&mut handle).is_ok() {
            let _ = SetWindowPos(
                handle,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

fn read_stream(stream: &IStream) -> windows::core::Result<Vec<u8>> {
    let mut statistics = STATSTG::default();
    unsafe {
        stream.Stat(&mut statistics, STATFLAG_NONAME)?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut bytes = vec![0; statistics.cbSize as usize];
        let mut read = 0;
        stream
            .Read(
                bytes.as_mut_ptr().cast(),
                bytes.len() as u32,
                Some(&mut read),
            )
            .ok()?;
        bytes.truncate(read as usize);
        Ok(bytes)
    }
}

fn is_launcher_shortcut(key: u32) -> bool {
    key == VK_T.0 as u32
        && unsafe {
            GetKeyState(VK_CONTROL.0 as i32) < 0
                && GetKeyState(VK_MENU.0 as i32) >= 0
                && GetKeyState(VK_SHIFT.0 as i32) >= 0
        }
}

fn attach_shortcuts(
    controller: ICoreWebView2Controller,
    sender: Sender<RuntimeEvent>,
    expanded: Rc<Cell<bool>>,
) -> windows::core::Result<()> {
    let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, arguments| {
        let Some(arguments) = arguments else {
            return Ok(());
        };
        unsafe {
            let mut kind = COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN;
            arguments.KeyEventKind(&mut kind)?;
            if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
            {
                return Ok(());
            }
            let mut key = 0;
            arguments.VirtualKey(&mut key)?;
            if key == VK_ESCAPE.0 as u32 && expanded.get() {
                arguments.SetHandled(true)?;
                let _ = sender.send(RuntimeEvent::Public(WebEvent::Shell(
                    json!({ "action": "collapse" }),
                )));
            } else if is_launcher_shortcut(key) {
                arguments.SetHandled(true)?;
                let _ = sender.send(RuntimeEvent::Launcher);
            } else if key == 0x0D && GetKeyState(VK_CONTROL.0 as i32) < 0 {
                arguments.SetHandled(true)?;
                let _ = sender.send(RuntimeEvent::Public(WebEvent::Shell(
                    json!({ "action":if expanded.get() { "collapse" } else { "expand" } }),
                )));
            }
        }
        Ok(())
    }));
    let mut token = 0;
    unsafe { controller.add_AcceleratorKeyPressed(&handler, &mut token) }
}

fn attach_navigation(
    webview: ICoreWebView2,
    id: String,
    sender: Sender<RuntimeEvent>,
) -> windows::core::Result<()> {
    let handler = SourceChangedEventHandler::create(Box::new(move |webview, _| {
        let Some(webview) = webview else {
            return Ok(());
        };
        let mut source = windows::core::PWSTR::null();
        unsafe {
            webview.Source(&mut source)?;
        }
        let _ = sender.send(RuntimeEvent::Public(WebEvent::Navigation {
            id: id.clone(),
            url: take_pwstr(source),
        }));
        Ok(())
    }));
    let mut token = 0;
    unsafe { webview.add_SourceChanged(&handler, &mut token) }
}

fn update_webviews(
    mut runtime: NonSendMut<WebRuntime>,
    mut commands: ResMut<WebCommands>,
    mut events: ResMut<WebEvents>,
    mut shell_interaction: ResMut<ShellInteraction>,
    windows: Query<(Entity, &Window), With<PrimaryWindow>>,
) {
    let Ok((entity, window)) = windows.single() else {
        return;
    };
    let native_ready = WINIT_WINDOWS.with(|windows| windows.borrow().get_window(entity).is_some());
    if !native_ready {
        return;
    }
    let size = (window.width() as f64, window.height() as f64);
    let physical_size = (
        window.physical_width() as f64,
        window.physical_height() as f64,
    );
    let resized = runtime.size != size || runtime.physical_size != physical_size;
    runtime.physical_size = physical_size;
    if resized {
        if let Err(message) = runtime.resize(size) {
            events.events.push(WebEvent::Error {
                id: "shell".to_owned(),
                message,
            });
        }
    }
    runtime.clip_compositor();
    let mut surfaces = Vec::new();
    for action in std::mem::take(&mut commands.actions) {
        if matches!(action, WebAction::Surface { .. }) {
            surfaces.push(action);
            continue;
        }
        if let Err(message) = runtime.run_action(entity, action) {
            events.events.push(WebEvent::Error {
                id: "runtime".to_owned(),
                message,
            });
        }
    }
    for action in &surfaces {
        if let WebAction::Surface {
            id,
            corners,
            viewport,
            visible,
        } = action
        {
            runtime.remember_surface(id, *corners, *viewport, *visible);
        }
    }
    runtime.poll_pages(entity, &mut events.events);
    for action in surfaces {
        if let Err(message) = runtime.run_action(entity, action) {
            events.events.push(WebEvent::Error {
                id: "surface".to_owned(),
                message,
            });
        }
    }
    while let Ok(event) = runtime.receiver.try_recv() {
        match event {
            RuntimeEvent::Captured {
                id,
                generation,
                request,
                result,
            } => {
                if runtime.page_generations.get(&id) != Some(&generation)
                    || runtime.capturing.get(&id) != Some(&request)
                {
                    continue;
                }
                runtime.capturing.remove(&id);
                runtime.capture_deadlines.remove(&id);
                let warmed = runtime.warming.as_ref() == Some(&id);
                if warmed {
                    runtime.finish_warm(false);
                }
                if runtime.pending_hide.contains(&id) {
                    runtime.hide(&id);
                }
                match result {
                    Ok(bytes) => events.events.push(WebEvent::Snapshot { id, bytes }),
                    Err(message) if warmed => {
                        eprintln!("Anteprima iniziale non disponibile per {id}: {message}")
                    }
                    Err(message) => events.events.push(WebEvent::Error { id, message }),
                }
            }
            RuntimeEvent::Launcher => {
                runtime.show_launcher();
            }
            RuntimeEvent::PopupOpened => runtime.sync_popups(true),
            RuntimeEvent::PopupClosed { id } => runtime.close_popup(Some(id)),
            RuntimeEvent::PopupTitle { id, title } => {
                if let Some(popup) = runtime
                    .popups
                    .borrow_mut()
                    .views
                    .iter_mut()
                    .find(|popup| popup.id == id)
                {
                    popup.title = title;
                }
                runtime.sync_popups(false);
            }
            RuntimeEvent::Public(WebEvent::Shell(message)) if message["action"] == "closePopup" => {
                runtime.close_popup(None)
            }
            RuntimeEvent::Public(WebEvent::Shell(message))
                if message["action"] == "shellResize" =>
            {
                runtime.shell_overlay = message["overlay"].as_bool().unwrap_or(false);
                runtime.shell_palette = if runtime.shell_overlay {
                    palette_geometry(&message["palette"])
                } else {
                    None
                };
                shell_interaction.overlay = runtime.shell_overlay;
                shell_interaction.palette =
                    runtime.shell_palette.map(|[x, y, width, height, _]| {
                        bevy::math::Rect::from_corners(
                            Vec2::new(x as f32, y as f32),
                            Vec2::new((x + width) as f32, (y + height) as f32),
                        )
                    });
                runtime.shell_height = message["height"]
                    .as_f64()
                    .unwrap_or(64.0)
                    .clamp(64.0, window.height() as f64);
                if let Err(message) = runtime.resize(size) {
                    events.events.push(WebEvent::Error {
                        id: "shell".to_owned(),
                        message,
                    });
                }
            }
            RuntimeEvent::Public(WebEvent::Shell(message)) if message["action"] == "shellReady" => {
                runtime.update_shell();
                events.events.push(WebEvent::Shell(message));
            }
            RuntimeEvent::Public(WebEvent::PageLoaded { id, url }) => {
                runtime.loaded_pages.insert(id.clone());
                if runtime.active.as_ref() == Some(&id) {
                    runtime
                        .capture_after
                        .insert(id.clone(), Instant::now() + Duration::from_millis(700));
                } else if runtime.warming.as_ref() == Some(&id) {
                    runtime.warm_deadline = None;
                    runtime
                        .capture_after
                        .insert(id.clone(), Instant::now() + Duration::from_secs(1));
                }
                if id == "shell" {
                    runtime.update_shell();
                }
                events.events.push(WebEvent::PageLoaded { id, url });
            }
            RuntimeEvent::Public(WebEvent::Inspection { id, mut result }) => {
                if let Some(view) = runtime.pages.get(&id) {
                    if let Some(result) = result.as_object_mut() {
                        result.insert("nativeSurface".to_owned(), view.surface_state());
                    }
                }
                events.events.push(WebEvent::Inspection { id, result });
            }
            RuntimeEvent::Public(event) => events.events.push(event),
        }
    }
    let popup_requests = std::mem::take(&mut *runtime.popup_requests.borrow_mut());
    for PopupRequest {
        owner,
        parent,
        opener,
        arguments,
        deferral,
    } in popup_requests
    {
        if runtime.pages.contains_key(&owner) {
            let response = create_popup(
                owner,
                parent,
                runtime.popups.clone(),
                runtime.sender.clone(),
                opener,
            );
            if let NewWindowResponse::Create { webview } = response {
                unsafe {
                    let _ = arguments.SetNewWindow(&webview);
                }
            }
        }
        unsafe {
            let _ = deferral.Complete();
        }
    }
    let timed_out: Vec<String> = runtime
        .capture_deadlines
        .iter()
        .filter(|(_, deadline)| **deadline <= Instant::now())
        .map(|(id, _)| id.clone())
        .collect();
    for id in timed_out {
        runtime.capture_deadlines.remove(&id);
        runtime.capturing.remove(&id);
        if runtime.warming.as_ref() == Some(&id) {
            runtime.finish_warm(false);
        }
        if runtime.pending_hide.contains(&id) {
            runtime.hide(&id);
        }
        eprintln!("Anteprima non disponibile per {id}: acquisizione scaduta");
    }
    if runtime
        .warm_deadline
        .is_some_and(|deadline| deadline <= Instant::now())
    {
        if let Some(id) = runtime.warming.clone() {
            eprintln!("Acquisizione del contenuto corrente per {id}: caricamento ancora in corso");
            runtime.capture_after.insert(id, Instant::now());
        }
        runtime.warm_deadline = None;
    }
    runtime.start_warm();
    let captures: Vec<String> = runtime
        .capture_after
        .iter()
        .filter(|(_, deadline)| **deadline <= Instant::now())
        .map(|(id, _)| id.clone())
        .collect();
    for id in captures {
        runtime.capture_after.remove(&id);
        if runtime.active.as_ref() == Some(&id) || runtime.warming.as_ref() == Some(&id) {
            if let Err(message) = runtime.capture(&id) {
                if runtime.warming.as_ref() == Some(&id) {
                    runtime.finish_warm(false);
                    eprintln!("Anteprima iniziale non disponibile per {id}: {message}");
                } else {
                    events.events.push(WebEvent::Error { id, message });
                }
            }
        }
    }
}
