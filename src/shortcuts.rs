use bevy::{ecs::system::NonSendMarker, prelude::*, window::PrimaryWindow, winit::WINIT_WINDOWS};
use std::{
    cell::RefCell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        Input::{
            KeyboardAndMouse::{
                GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT,
                VK_LWIN, VK_MENU, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_TAB,
            },
            RAWINPUTDEVICE, RIDEV_REMOVE, RegisterRawInputDevices,
        },
        WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GA_ROOT, GetAncestor,
            GetForegroundWindow, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, KillTimer,
            LLKHF_ALTDOWN, LLKHF_EXTENDED, LLKHF_UP, MSG, PM_NOREMOVE, PeekMessageW,
            PostThreadMessageW, SetTimer, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
            WH_KEYBOARD_LL, WINEVENT_OUTOFCONTEXT, WM_APP, WM_QUIT, WM_TIMER,
        },
    },
};
use wry::raw_window_handle::RawWindowHandle;

use crate::workspace::{OrbitWorkspace, WorkspaceAction, WorkspaceActions};

pub struct ShortcutsPlugin;

impl Plugin for ShortcutsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NativeShortcuts>().add_systems(
            Update,
            refresh_scope.after(crate::controller::handle_actions),
        );
    }
}

#[derive(Resource, Default)]
pub struct NativeShortcuts {
    hook: Option<KeyboardHook>,
    installed: bool,
    last_trace: Option<Instant>,
    keyboard_compatibility_failed: bool,
}

impl NativeShortcuts {
    pub fn ready(&self) -> bool {
        self.installed
            && self.thread_alive()
            && self.hook.as_ref().is_some_and(|hook| {
                hook.enabled.load(Ordering::Acquire) && orbit_is_foreground(hook.parent)
            })
    }

    pub fn callback_count(&self) -> u64 {
        CALLBACK_COUNT.load(Ordering::Acquire)
    }

    pub fn callback_stage(&self) -> u32 {
        CALLBACK_STAGE.load(Ordering::Acquire)
    }

    pub fn thread_alive(&self) -> bool {
        self.hook
            .as_ref()
            .and_then(|hook| hook.thread.as_ref())
            .is_some_and(|thread| !thread.is_finished())
    }
}

struct KeyboardHook {
    parent: isize,
    trace: bool,
    enabled: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    thread_id: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
    receiver: Mutex<Receiver<ShortcutEvent>>,
}

enum ShortcutEvent {
    Installed(Result<(), String>),
    Removed,
    ThreadStarted(u32),
    ThreadStopped {
        message_result: i32,
        unhooked: bool,
    },
    Cycle(i32),
    Trace {
        thread_id: u32,
        key: u32,
        scan_code: u32,
        flags: u32,
        message: usize,
        enabled: bool,
        foreground: isize,
        root: isize,
        parent: isize,
        result: ChordResult,
        before: ShortcutChord,
        after: ShortcutChord,
    },
}

struct HookContext {
    parent: isize,
    enabled: Arc<AtomicBool>,
    sender: Sender<ShortcutEvent>,
    chord: ShortcutChord,
    trace: bool,
    thread_id: u32,
    module: HINSTANCE,
    hook: Option<HHOOK>,
    release_timer: Option<usize>,
    active: bool,
}

thread_local! {
    static HOOK_CONTEXT: RefCell<Option<HookContext>> = const { RefCell::new(None) };
}

static CALLBACK_COUNT: AtomicU64 = AtomicU64::new(0);
static CALLBACK_STAGE: AtomicU32 = AtomicU32::new(0);
static CALLBACK_CODE: AtomicI32 = AtomicI32::new(0);
const SCOPE_CHANGED: u32 = WM_APP + 1;

#[derive(Clone, Copy, Debug, Default)]
struct ShortcutChord {
    left_shift: bool,
    right_shift: bool,
    left_control: bool,
    right_control: bool,
    left_alt: bool,
    right_alt: bool,
    left_windows: bool,
    right_windows: bool,
    tab_down: bool,
    captured_tab: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ChordResult {
    Pass,
    Consume,
    Cycle(i32),
}

impl KeyboardHook {
    fn start(parent: isize) -> Self {
        let trace = std::env::var_os("ORBIT_TRACE_SHORTCUTS").is_some_and(|value| value != "0");
        let enabled = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread_id = Arc::new(AtomicU32::new(0));
        let (sender, receiver) = mpsc::channel();
        let thread_enabled = enabled.clone();
        let thread_stopping = stopping.clone();
        let thread_identifier = thread_id.clone();
        let thread = thread::spawn(move || {
            run_hook(
                parent,
                thread_enabled,
                thread_stopping,
                thread_identifier,
                sender,
            );
        });
        Self {
            parent,
            trace,
            enabled,
            stopping,
            thread_id,
            thread: Some(thread),
            receiver: Mutex::new(receiver),
        }
    }

    fn set_enabled(&self, enabled: bool) {
        if self.enabled.swap(enabled, Ordering::AcqRel) == enabled {
            return;
        }
        let thread_id = self.thread_id.load(Ordering::Acquire);
        if thread_id != 0 {
            unsafe {
                let _ = PostThreadMessageW(
                    thread_id,
                    SCOPE_CHANGED,
                    WPARAM::default(),
                    LPARAM::default(),
                );
            }
        }
    }
}

impl Drop for KeyboardHook {
    fn drop(&mut self) {
        self.enabled.store(false, Ordering::Release);
        self.stopping.store(true, Ordering::Release);
        let thread_id = self.thread_id.load(Ordering::Acquire);
        if thread_id != 0 {
            unsafe {
                let _ =
                    PostThreadMessageW(thread_id, WM_QUIT, WPARAM::default(), LPARAM::default());
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl ShortcutChord {
    fn current() -> Self {
        let pressed = |key: VIRTUAL_KEY| unsafe { GetAsyncKeyState(key.0 as i32) < 0 };
        Self {
            left_shift: pressed(VK_LSHIFT),
            right_shift: pressed(VK_RSHIFT),
            left_control: pressed(VK_LCONTROL),
            right_control: pressed(VK_RCONTROL),
            left_alt: pressed(VK_LMENU),
            right_alt: pressed(VK_RMENU),
            left_windows: pressed(VK_LWIN),
            right_windows: pressed(VK_RWIN),
            tab_down: pressed(VK_TAB),
            ..default()
        }
    }

    fn accept(&mut self, event: &KBDLLHOOKSTRUCT, active: bool) -> ChordResult {
        let down = !event.flags.contains(LLKHF_UP);
        let key = normalized_modifier(event);
        let modifier = match key {
            VK_LSHIFT => Some(&mut self.left_shift),
            VK_RSHIFT => Some(&mut self.right_shift),
            VK_LCONTROL => Some(&mut self.left_control),
            VK_RCONTROL => Some(&mut self.right_control),
            VK_LMENU => Some(&mut self.left_alt),
            VK_RMENU => Some(&mut self.right_alt),
            VK_LWIN => Some(&mut self.left_windows),
            VK_RWIN => Some(&mut self.right_windows),
            _ => None,
        };
        if let Some(modifier) = modifier {
            *modifier = down;
            return ChordResult::Pass;
        }
        if key != VK_TAB {
            return ChordResult::Pass;
        }
        if !down {
            self.tab_down = false;
            return if std::mem::take(&mut self.captured_tab) {
                ChordResult::Consume
            } else {
                ChordResult::Pass
            };
        }
        if self.tab_down {
            return if self.captured_tab {
                ChordResult::Consume
            } else {
                ChordResult::Pass
            };
        }
        self.tab_down = true;
        let alt = self.left_alt || self.right_alt || event.flags.contains(LLKHF_ALTDOWN);
        if !active
            || !alt
            || self.left_control
            || self.right_control
            || self.left_windows
            || self.right_windows
        {
            return ChordResult::Pass;
        }
        self.captured_tab = true;
        ChordResult::Cycle(if self.left_shift || self.right_shift {
            -1
        } else {
            1
        })
    }
}

fn normalized_modifier(event: &KBDLLHOOKSTRUCT) -> VIRTUAL_KEY {
    let key = VIRTUAL_KEY(event.vkCode as u16);
    match key {
        VK_SHIFT => {
            if event.scanCode == 0x36 {
                VK_RSHIFT
            } else {
                VK_LSHIFT
            }
        }
        VK_CONTROL => {
            if event.flags.contains(LLKHF_EXTENDED) {
                VK_RCONTROL
            } else {
                VK_LCONTROL
            }
        }
        VK_MENU => {
            if event.flags.contains(LLKHF_EXTENDED) {
                VK_RMENU
            } else {
                VK_LMENU
            }
        }
        _ => key,
    }
}

fn run_hook(
    parent: isize,
    enabled: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    thread_id: Arc<AtomicU32>,
    sender: Sender<ShortcutEvent>,
) {
    let mut message = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
        thread_id.store(GetCurrentThreadId(), Ordering::Release);
    }
    let trace = std::env::var_os("ORBIT_TRACE_SHORTCUTS").is_some_and(|value| value != "0");
    if trace {
        let _ = sender.send(ShortcutEvent::ThreadStarted(
            thread_id.load(Ordering::Acquire),
        ));
    }
    if stopping.load(Ordering::Acquire) {
        return;
    }
    let module = match unsafe { GetModuleHandleW(None) } {
        Ok(module) => HINSTANCE(module.0),
        Err(error) => {
            let _ = sender.send(ShortcutEvent::Installed(Err(error.to_string())));
            return;
        }
    };
    HOOK_CONTEXT.with_borrow_mut(|context| {
        *context = Some(HookContext {
            parent,
            enabled,
            sender: sender.clone(),
            chord: ShortcutChord::current(),
            trace,
            thread_id: thread_id.load(Ordering::Acquire),
            module,
            hook: None,
            release_timer: None,
            active: false,
        });
    });
    let foreground_listener = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_changed),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if foreground_listener.0.is_null() {
        let error = windows::core::Error::from_win32();
        let _ = sender.send(ShortcutEvent::Installed(Err(error.to_string())));
        HOOK_CONTEXT.with_borrow_mut(|context| *context = None);
        return;
    }
    reconcile_scope(false);
    let mut message_result = 1;
    while !stopping.load(Ordering::Acquire) {
        message_result = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if message_result <= 0 {
            break;
        }
        match message.message {
            SCOPE_CHANGED => reconcile_scope(false),
            WM_TIMER if release_timer_matches(message.wParam.0) => reconcile_scope(true),
            _ => unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            },
        }
    }
    HOOK_CONTEXT.with_borrow_mut(|context| {
        if let Some(context) = context {
            remove_keyboard_hook(context);
        }
    });
    let unhooked = unsafe { UnhookWinEvent(foreground_listener) }.as_bool();
    let _ = sender.send(ShortcutEvent::ThreadStopped {
        message_result,
        unhooked,
    });
    HOOK_CONTEXT.with_borrow_mut(|context| *context = None);
}

unsafe extern "system" fn foreground_changed(
    _listener: HWINEVENTHOOK,
    _event: u32,
    _window: HWND,
    _object: i32,
    _child: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    reconcile_scope(false);
}

fn reconcile_scope(expire_release: bool) {
    HOOK_CONTEXT.with_borrow_mut(|context| {
        let Some(context) = context else {
            return;
        };
        let active = context.enabled.load(Ordering::Acquire) && orbit_is_foreground(context.parent);
        let entering = active && !context.active;
        context.active = active;
        if active {
            if entering {
                remove_keyboard_hook(context);
            }
            cancel_release_timer(context);
            if context.hook.is_none() {
                context.chord = ShortcutChord::current();
                match unsafe {
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(context.module), 0)
                } {
                    Ok(hook) => {
                        context.hook = Some(hook);
                        let _ = context.sender.send(ShortcutEvent::Installed(Ok(())));
                    }
                    Err(error) => {
                        let _ = context
                            .sender
                            .send(ShortcutEvent::Installed(Err(error.to_string())));
                    }
                }
            }
            return;
        }
        if context.hook.is_none() {
            return;
        }
        if context.chord.captured_tab && !expire_release {
            if context.release_timer.is_none() {
                let timer = unsafe { SetTimer(None, 0, 250, None) };
                if timer != 0 {
                    context.release_timer = Some(timer);
                    return;
                }
            } else {
                return;
            }
        }
        remove_keyboard_hook(context);
    });
}

fn cancel_release_timer(context: &mut HookContext) {
    if let Some(timer) = context.release_timer.take() {
        unsafe {
            let _ = KillTimer(None, timer);
        }
    }
}

fn remove_keyboard_hook(context: &mut HookContext) {
    cancel_release_timer(context);
    if let Some(hook) = context.hook.take() {
        unsafe {
            let _ = UnhookWindowsHookEx(hook);
        }
        context.chord = ShortcutChord::default();
        let _ = context.sender.send(ShortcutEvent::Removed);
    }
}

fn release_timer_matches(timer: usize) -> bool {
    HOOK_CONTEXT.with_borrow(|context| {
        context
            .as_ref()
            .is_some_and(|context| context.release_timer == Some(timer))
    })
}

unsafe extern "system" fn keyboard_hook(code: i32, message: WPARAM, pointer: LPARAM) -> LRESULT {
    CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
    CALLBACK_CODE.store(code, Ordering::Release);
    CALLBACK_STAGE.store(1, Ordering::Release);
    if code != HC_ACTION as i32 || pointer.0 == 0 {
        CALLBACK_STAGE.store(100, Ordering::Release);
        let result = unsafe { CallNextHookEx(None, code, message, pointer) };
        CALLBACK_STAGE.store(101, Ordering::Release);
        return result;
    }
    let event = unsafe { &*(pointer.0 as *const KBDLLHOOKSTRUCT) };
    let consumed = HOOK_CONTEXT.with_borrow_mut(|context| {
        let Some(context) = context else {
            CALLBACK_STAGE.store(102, Ordering::Release);
            return false;
        };
        let enabled = context.enabled.load(Ordering::Acquire);
        CALLBACK_STAGE.store(2, Ordering::Release);
        let foreground = unsafe { GetForegroundWindow() };
        CALLBACK_STAGE.store(3, Ordering::Release);
        let root = unsafe { GetAncestor(foreground, GA_ROOT) };
        CALLBACK_STAGE.store(4, Ordering::Release);
        let active = enabled && root == HWND(context.parent as *mut _);
        let before = context.chord;
        let result = context.chord.accept(event, active);
        if before.captured_tab && !context.chord.captured_tab && context.release_timer.is_some() {
            unsafe {
                let _ = PostThreadMessageW(
                    context.thread_id,
                    SCOPE_CHANGED,
                    WPARAM::default(),
                    LPARAM::default(),
                );
            }
        }
        CALLBACK_STAGE.store(5, Ordering::Release);
        if context.trace && is_shortcut_key(event) {
            let _ = context.sender.send(ShortcutEvent::Trace {
                thread_id: context.thread_id,
                key: event.vkCode,
                scan_code: event.scanCode,
                flags: event.flags.0,
                message: message.0,
                enabled,
                foreground: foreground.0 as isize,
                root: root.0 as isize,
                parent: context.parent,
                result,
                before,
                after: context.chord,
            });
        }
        CALLBACK_STAGE.store(6, Ordering::Release);
        match result {
            ChordResult::Pass => false,
            ChordResult::Consume => true,
            ChordResult::Cycle(direction) => {
                let _ = context.sender.send(ShortcutEvent::Cycle(direction));
                true
            }
        }
    });
    if consumed {
        CALLBACK_STAGE.store(9, Ordering::Release);
        LRESULT(1)
    } else {
        CALLBACK_STAGE.store(7, Ordering::Release);
        let result = unsafe { CallNextHookEx(None, code, message, pointer) };
        CALLBACK_STAGE.store(8, Ordering::Release);
        result
    }
}

fn is_shortcut_key(event: &KBDLLHOOKSTRUCT) -> bool {
    matches!(
        normalized_modifier(event),
        VK_TAB
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_LMENU
            | VK_RMENU
            | VK_LWIN
            | VK_RWIN
    )
}

fn orbit_is_foreground(parent: isize) -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    (unsafe { GetAncestor(foreground, GA_ROOT) }) == HWND(parent as *mut _)
}

fn disable_raw_keyboard_input() -> windows::core::Result<()> {
    let keyboard = RAWINPUTDEVICE {
        usUsagePage: 1,
        usUsage: 6,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: HWND::default(),
    };
    unsafe { RegisterRawInputDevices(&[keyboard], std::mem::size_of::<RAWINPUTDEVICE>() as u32) }
}

fn refresh_scope(
    _main_thread: NonSendMarker,
    mut shortcuts: ResMut<NativeShortcuts>,
    workspace: Res<OrbitWorkspace>,
    windows: Query<Entity, With<PrimaryWindow>>,
) {
    if shortcuts.hook.is_none() {
        if shortcuts.keyboard_compatibility_failed {
            return;
        }
        let Ok(entity) = windows.single() else {
            return;
        };
        let parent = WINIT_WINDOWS.with(|windows| {
            let windows = windows.borrow();
            let window = windows.get_window(entity)?;
            let handle = bevy::window::RawHandleWrapper::new(window).ok()?;
            let RawWindowHandle::Win32(handle) = handle.get_window_handle() else {
                return None;
            };
            Some(handle.hwnd.get())
        });
        let Some(parent) = parent else {
            return;
        };
        if let Err(error) = disable_raw_keyboard_input() {
            shortcuts.keyboard_compatibility_failed = true;
            eprintln!("Compatibilità Alt+Tab non disponibile: {error}");
            return;
        }
        shortcuts.hook = Some(KeyboardHook::start(parent));
    }
    if let Some(hook) = &shortcuts.hook {
        let enabled = workspace
            .current_project
            .as_ref()
            .is_some_and(|project| workspace.tabs.iter().any(|tab| &tab.project_id == project));
        hook.set_enabled(enabled);
    }
}

pub fn receive_shortcuts(
    mut shortcuts: ResMut<NativeShortcuts>,
    mut actions: ResMut<WorkspaceActions>,
) {
    let Some(hook) = &mut shortcuts.hook else {
        return;
    };
    let parent = hook.parent;
    let trace = hook.trace;
    let enabled = hook.enabled.clone();
    let Ok(receiver) = hook.receiver.get_mut() else {
        return;
    };
    let events: Vec<_> = receiver.try_iter().collect();
    for event in events {
        match event {
            ShortcutEvent::ThreadStarted(thread_id) => {
                eprintln!("SHORTCUTS thread_started id={thread_id}");
            }
            ShortcutEvent::ThreadStopped {
                message_result,
                unhooked,
            } => {
                shortcuts.installed = false;
                if trace {
                    eprintln!(
                        "SHORTCUTS thread_stopped message_result={message_result} unhooked={unhooked}"
                    );
                }
            }
            ShortcutEvent::Removed => {
                shortcuts.installed = false;
                if trace {
                    eprintln!("SHORTCUTS removed parent={parent}");
                }
            }
            ShortcutEvent::Installed(result) => match result {
                Ok(()) => {
                    shortcuts.installed = true;
                    if trace {
                        eprintln!(
                            "SHORTCUTS installed parent={parent} enabled={}",
                            enabled.load(Ordering::Acquire)
                        );
                    }
                }
                Err(error) => {
                    shortcuts.installed = false;
                    eprintln!("Alt+Tab non disponibile: {error}");
                }
            },
            ShortcutEvent::Cycle(direction) => {
                let enabled = enabled.load(Ordering::Acquire);
                let foreground = orbit_is_foreground(parent);
                if trace {
                    eprintln!(
                        "SHORTCUTS cycle direction={direction} accepted={} enabled={enabled} foreground={foreground}",
                        enabled && foreground
                    );
                }
                if enabled && foreground {
                    actions.0.push(WorkspaceAction::Cycle(direction));
                }
            }
            ShortcutEvent::Trace {
                thread_id,
                key,
                scan_code,
                flags,
                message,
                enabled,
                foreground,
                root,
                parent,
                result,
                before,
                after,
            } => eprintln!(
                "SHORTCUTS thread={thread_id} key={key} scan={scan_code} flags={flags:#x} message={message:#x} enabled={enabled} foreground={foreground} root={root} parent={parent} result={result:?} before={before:?} after={after:?}"
            ),
        }
    }
    if trace
        && shortcuts
            .last_trace
            .is_none_or(|previous| previous.elapsed() >= Duration::from_secs(1))
    {
        shortcuts.last_trace = Some(Instant::now());
        eprintln!(
            "SHORTCUTS health alive={} callbacks={} stage={} code={}",
            shortcuts.thread_alive(),
            shortcuts.callback_count(),
            shortcuts.callback_stage(),
            CALLBACK_CODE.load(Ordering::Acquire)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(key: VIRTUAL_KEY, down: bool) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            vkCode: key.0 as u32,
            flags: if down { default() } else { LLKHF_UP },
            ..default()
        }
    }

    #[test]
    fn alt_tab_cycles_once_until_tab_is_released() {
        let mut chord = ShortcutChord::default();
        assert_eq!(
            chord.accept(&event(VK_LMENU, true), true),
            ChordResult::Pass
        );
        assert_eq!(
            chord.accept(&event(VK_TAB, true), true),
            ChordResult::Cycle(1)
        );
        assert_eq!(
            chord.accept(&event(VK_TAB, true), true),
            ChordResult::Consume
        );
        assert_eq!(
            chord.accept(&event(VK_TAB, false), true),
            ChordResult::Consume
        );
        assert_eq!(
            chord.accept(&event(VK_TAB, true), true),
            ChordResult::Cycle(1)
        );
    }

    #[test]
    fn shift_reverses_the_direction_even_when_it_changes_while_alt_is_held() {
        let mut chord = ShortcutChord::default();
        chord.accept(&event(VK_RMENU, true), true);
        chord.accept(&event(VK_LSHIFT, true), true);
        assert_eq!(
            chord.accept(&event(VK_TAB, true), true),
            ChordResult::Cycle(-1)
        );
        chord.accept(&event(VK_TAB, false), true);
        chord.accept(&event(VK_LSHIFT, false), true);
        assert_eq!(
            chord.accept(&event(VK_TAB, true), true),
            ChordResult::Cycle(1)
        );
    }

    #[test]
    fn windows_shortcuts_and_alt_gr_are_passed_through() {
        for modifier in [VK_LCONTROL, VK_RCONTROL, VK_LWIN, VK_RWIN] {
            let mut chord = ShortcutChord::default();
            chord.accept(&event(VK_RMENU, true), true);
            chord.accept(&event(modifier, true), true);
            assert_eq!(chord.accept(&event(VK_TAB, true), true), ChordResult::Pass);
            assert_eq!(chord.accept(&event(VK_TAB, false), true), ChordResult::Pass);
        }
    }

    #[test]
    fn other_apps_receive_the_chord_and_captured_releases_remain_balanced() {
        let mut chord = ShortcutChord::default();
        chord.accept(&event(VK_LMENU, true), false);
        assert_eq!(chord.accept(&event(VK_TAB, true), false), ChordResult::Pass);
        assert_eq!(
            chord.accept(&event(VK_TAB, false), false),
            ChordResult::Pass
        );
        assert_eq!(
            chord.accept(&event(VK_TAB, true), true),
            ChordResult::Cycle(1)
        );
        assert_eq!(
            chord.accept(&event(VK_TAB, false), false),
            ChordResult::Consume
        );
        assert_eq!(chord.accept(&event(VK_TAB, true), false), ChordResult::Pass);
    }
}
