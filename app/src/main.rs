#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;

use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tao::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    platform::windows::WindowBuilderExtWindows,
    window::{ResizeDirection, WindowBuilder},
};
use tray_icon::{
    MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use wry::{WebContext, WebViewBuilder};

const HTML: &str = include_str!("../ui/index.html");
const CAPSULE: LogicalSize<f64> = LogicalSize::new(276.0, 54.0);
const MIN: LogicalSize<f64> = LogicalSize::new(340.0, 540.0);

enum UserEvent {
    Js(String),
    Ipc(String),
    Toggle,
    Menu(String),
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct State {
    x: Option<i32>,
    y: Option<i32>,
    w: u32,
    h: u32,
    on_top: bool,
    collapsed: bool,
}

impl Default for State {
    fn default() -> Self {
        State { x: None, y: None, w: 0, h: 0, on_top: true, collapsed: false }
    }
}

fn data_dir() -> PathBuf {
    let d = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("Noema");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn load_state() -> State {
    std::fs::read_to_string(data_dir().join("state.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(s: &State) {
    if let Ok(j) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(data_dir().join("state.json"), j);
    }
}

/// 琥珀色菱形圖示（視窗與系統匣共用）
fn icon_rgba(size: u32) -> Vec<u8> {
    let mut px = vec![0u8; (size * size * 4) as usize];
    let c = (size as f32 - 1.0) / 2.0;
    for y in 0..size {
        for x in 0..size {
            let d = (x as f32 - c).abs() + (y as f32 - c).abs();
            let i = ((y * size + x) * 4) as usize;
            let (r, g, b, a) = if d < c * 0.42 {
                (0xed, 0xc4, 0x77, 255)
            } else if d < c * 0.72 {
                (0x19, 0x1b, 0x1b, 255)
            } else if d < c * 0.98 {
                (0xed, 0xc4, 0x77, 255)
            } else {
                (0, 0, 0, 0)
            };
            px[i..i + 4].copy_from_slice(&[r, g, b, a]);
        }
    }
    px
}

fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let mut state = load_state();

    let mut builder = WindowBuilder::new()
        .with_title("Noema · 認知核心")
        .with_decorations(false)
        .with_transparent(true)
        .with_undecorated_shadow(false)
        .with_skip_taskbar(true)
        .with_always_on_top(state.on_top)
        .with_window_icon(tao::window::Icon::from_rgba(icon_rgba(32), 32, 32).ok())
        .with_visible(false);
    builder = if state.w >= 200 && state.h >= 200 {
        builder.with_inner_size(PhysicalSize::new(state.w, state.h))
    } else {
        builder.with_inner_size(LogicalSize::new(420.0, 660.0))
    };
    let window = builder.build(&event_loop).expect("window");

    // 還原位置：只有落在某個螢幕範圍內才採用
    if let (Some(x), Some(y)) = (state.x, state.y) {
        let on_screen = window.available_monitors().any(|m| {
            let (p, s) = (m.position(), m.size());
            x + 60 > p.x && x < p.x + s.width as i32 - 60 && y >= p.y - 10 && y < p.y + s.height as i32 - 40
        });
        if on_screen {
            window.set_outer_position(PhysicalPosition::new(x, y));
        }
    }
    if let Ok(p) = window.outer_position() {
        state.x = Some(p.x);
        state.y = Some(p.y);
    }
    if state.collapsed {
        window.set_inner_size(CAPSULE);
    } else {
        window.set_min_inner_size(Some(MIN));
        let s = window.inner_size();
        state.w = s.width;
        state.h = s.height;
    }

    let proxy = event_loop.create_proxy();
    let mut web_context = WebContext::new(Some(data_dir().join("webview")));
    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_transparent(true)
        .with_html(HTML)
        .with_ipc_handler(move |req| {
            let _ = ipc_proxy.send_event(UserEvent::Ipc(req.body().clone()));
        })
        .build(&window)
        .expect("webview");
    window.set_visible(true);

    // 背景掃描執行緒
    let (tx, rx) = mpsc::channel::<core::Cmd>();
    {
        let p = proxy.clone();
        let tx2 = tx.clone();
        std::thread::spawn(move || {
            core::run(rx, tx2, Box::new(move |json| {
                let _ = p.send_event(UserEvent::Js(json));
            }))
        });
    }

    // 系統匣
    let menu = Menu::new();
    let _ = menu.append_items(&[
        &MenuItem::with_id("toggle", "顯示 / 隱藏　Ctrl+Alt+N", true, None),
        &MenuItem::with_id("collapse", "收合 / 展開", true, None),
        &MenuItem::with_id("pin", "切換置頂", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id("quit", "結束 Noema", true, None),
    ]);
    let _tray = tray_icon::Icon::from_rgba(icon_rgba(32), 32, 32).ok().and_then(|icon| {
        TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip("Noema · 認知核心")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .ok()
    });
    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = p.send_event(UserEvent::Menu(e.id.0.clone()));
        }));
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                let _ = p.send_event(UserEvent::Toggle);
            }
        }));
    }

    // 全域快捷鍵 Ctrl+Alt+N
    let hotkey_manager = GlobalHotKeyManager::new().ok();
    let hotkey_ok = hotkey_manager
        .as_ref()
        .map(|m| m.register(HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyN)).is_ok())
        .unwrap_or(false);
    {
        let p = proxy.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if e.state == HotKeyState::Pressed {
                let _ = p.send_event(UserEvent::Toggle);
            }
        }));
    }

    let mut dirty: Option<Instant> = None;

    event_loop.run(move |event, _, control_flow| {
        let _keep = (&hotkey_manager, &_tray);
        let push_state = |state: &State| {
            let msg = serde_json::json!({
                "type": "state", "onTop": state.on_top, "collapsed": state.collapsed,
                "hotkey": if hotkey_ok { "Ctrl+Alt+N" } else { "" },
            });
            let _ = webview.evaluate_script(&format!("window.__noema&&window.__noema({msg})"));
        };
        let set_collapsed = |state: &mut State, on: bool| {
            if state.collapsed == on {
                return;
            }
            state.collapsed = on;
            if on {
                window.set_min_inner_size(None::<LogicalSize<f64>>);
                window.set_inner_size(CAPSULE);
            } else {
                window.set_inner_size(PhysicalSize::new(state.w.max(200), state.h.max(200)));
                window.set_min_inner_size(Some(MIN));
            }
        };
        let toggle_visible = || {
            if window.is_visible() {
                window.set_visible(false);
            } else {
                window.set_visible(true);
                window.set_focus();
            }
        };

        match event {
            Event::UserEvent(UserEvent::Js(json)) => {
                let _ = webview.evaluate_script(&format!("window.__noema&&window.__noema({json})"));
            }
            Event::UserEvent(UserEvent::Toggle) => toggle_visible(),
            Event::UserEvent(UserEvent::Menu(id)) => match id.as_str() {
                "toggle" => toggle_visible(),
                "collapse" => {
                    let on = !state.collapsed;
                    set_collapsed(&mut state, on);
                    push_state(&state);
                    save_state(&state);
                }
                "pin" => {
                    state.on_top = !state.on_top;
                    window.set_always_on_top(state.on_top);
                    push_state(&state);
                    save_state(&state);
                }
                "quit" => {
                    save_state(&state);
                    *control_flow = ControlFlow::Exit;
                }
                _ => {}
            },
            Event::UserEvent(UserEvent::Ipc(body)) => {
                let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                match v["cmd"].as_str().unwrap_or("") {
                    "ready" => {
                        push_state(&state);
                        let _ = tx.send(core::Cmd::Refresh);
                    }
                    "drag" => {
                        let _ = window.drag_window();
                    }
                    "resize" if !state.collapsed => {
                        let dir = match v["dir"].as_str().unwrap_or("") {
                            "n" => ResizeDirection::North,
                            "s" => ResizeDirection::South,
                            "e" => ResizeDirection::East,
                            "w" => ResizeDirection::West,
                            "ne" => ResizeDirection::NorthEast,
                            "nw" => ResizeDirection::NorthWest,
                            "sw" => ResizeDirection::SouthWest,
                            _ => ResizeDirection::SouthEast,
                        };
                        let _ = window.drag_resize_window(dir);
                    }
                    "collapse" => {
                        set_collapsed(&mut state, v["on"].as_bool().unwrap_or(false));
                        push_state(&state);
                        save_state(&state);
                    }
                    "pin" => {
                        state.on_top = v["on"].as_bool().unwrap_or(true);
                        window.set_always_on_top(state.on_top);
                        push_state(&state);
                        save_state(&state);
                    }
                    "hide" => window.set_visible(false),
                    "project" => {
                        if let Some(id) = v["id"].as_str() {
                            let _ = tx.send(core::Cmd::Switch(id.to_string()));
                        }
                    }
                    "quit" => {
                        save_state(&state);
                        *control_flow = ControlFlow::Exit;
                    }
                    _ => {}
                }
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    save_state(&state);
                    *control_flow = ControlFlow::Exit;
                }
                WindowEvent::Moved(p) => {
                    state.x = Some(p.x);
                    state.y = Some(p.y);
                    dirty = Some(Instant::now());
                }
                WindowEvent::Resized(s) => {
                    if !state.collapsed && s.width >= 200 && s.height >= 200 {
                        state.w = s.width;
                        state.h = s.height;
                        dirty = Some(Instant::now());
                    }
                }
                _ => {}
            },
            _ => {}
        }

        if *control_flow != ControlFlow::Exit {
            match dirty {
                Some(t) if t.elapsed() >= Duration::from_millis(700) => {
                    save_state(&state);
                    dirty = None;
                    *control_flow = ControlFlow::Wait;
                }
                Some(t) => *control_flow = ControlFlow::WaitUntil(t + Duration::from_millis(720)),
                None => *control_flow = ControlFlow::Wait,
            }
        }
    });
}
