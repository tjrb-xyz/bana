//! The menu bar (macOS): 🧱, then what the daemon does (`🧱 4m +2`,
//! `🧱 paused`), as the status item's title, with no image. The words come
//! from [`actlog::tray_view`], tested on Linux; this file only shows them.
//!
//! - left click: opens the page on the running build (else the latest);
//! - right click: the menu (the status, the last result, Open bana, Cancel
//!   build, Pause new builds, Quit bana).
//!
//! It runs in the daemon's process, on the main thread (AppKit wants it),
//! while the daemon runs on tokio's threads. A task forwards the daemon's
//! summary to the event loop; a 60 s tick keeps the elapsed time current.

use crate::actlog::{tray_view, Summary, BRICK};
use crate::daemon::Daemon;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tokio::sync::Notify;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const TICK: Duration = Duration::from_secs(60);

pub enum Wake {
    /// The daemon runs; the page's URL, with the token.
    Ready(Daemon, String),
    Changed,
    Stopped,
    Menu(MenuEvent),
    LeftClick,
}

/// What the daemon's side tells the menu bar.
#[derive(Clone)]
pub struct Tray(EventLoopProxy<Wake>);

impl Tray {
    /// The daemon runs: the menu bar follows its summary from now on.
    /// Called on the runtime.
    pub fn ready(&self, d: &Daemon, url: String) {
        let _ = self.0.send_event(Wake::Ready(d.clone(), url));
        let (mut rx, proxy) = (d.subscribe(), self.0.clone());
        tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                if proxy.send_event(Wake::Changed).is_err() {
                    return;
                }
                // At most two a second: the summary changes with each step.
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
    }

    /// The daemon has stopped: the menu bar goes, and the process exits 0.
    pub fn stopped(&self) {
        let _ = self.0.send_event(Wake::Stopped);
    }
}

struct Items {
    status: MenuItem,
    last: MenuItem,
    open: MenuItem,
    cancel: MenuItem,
    pause: CheckMenuItem,
    quit: MenuItem,
}

/// What the menu bar knows of the daemon.
struct Shown {
    daemon: Daemon,
    summary: tokio::sync::watch::Receiver<Summary>,
    url: String,
    running: Option<u64>,
    last: Option<u64>,
    open: Option<u64>,
}

fn menu() -> (Menu, Items) {
    let it = Items {
        status: MenuItem::new("bana: starting", false, None),
        last: MenuItem::new("Last: none yet", false, None),
        open: MenuItem::new("Open bana", false, None),
        cancel: MenuItem::new("Cancel build", false, None),
        pause: CheckMenuItem::new("Pause new builds", false, false, None),
        quit: MenuItem::new("Quit bana (local CI stops until next login)", true, None),
    };
    let menu = Menu::with_items(&[
        &it.status,
        &it.last,
        &PredefinedMenuItem::separator(),
        &it.open,
        &it.cancel,
        &it.pause,
        &PredefinedMenuItem::separator(),
        &it.quit,
    ])
    .expect("menu");
    (menu, it)
}

/// The summary last published, into the title, the tooltip and the menu.
/// (Not `Daemon::summary`: that publishes, and would wake us again.)
fn show(tray: &TrayIcon, it: &Items, s: &mut Shown) {
    let summary = s.summary.borrow().clone();
    let v = tray_view(&summary);
    s.running = summary.running.as_ref().map(|b| b.id);
    s.last = summary.last.as_ref().map(|b| b.id);
    s.open = v.open_build;
    tray.set_title(Some(&v.title));
    let _ = tray.set_tooltip(Some(&v.tooltip));
    it.status.set_text(&v.status_line);
    it.last
        .set_text(v.last_line.as_deref().unwrap_or("Last: none yet"));
    it.last.set_enabled(v.last_line.is_some());
    it.open.set_enabled(true);
    it.cancel.set_enabled(v.cancel_enabled);
    it.pause.set_enabled(true);
    it.pause.set_checked(v.paused);
}

/// Opens the page (on build `id`) in the default browser.
fn open(url: &str, id: Option<u64>) {
    let url = match id {
        Some(id) => format!("{url}&build={id}"),
        None => url.to_string(),
    };
    match std::process::Command::new("open").arg(&url).spawn() {
        // Reaped off the main thread.
        Ok(mut c) => {
            std::thread::spawn(move || c.wait());
        }
        Err(e) => eprintln!("bana-manager: open: {e}"),
    }
}

/// Runs the menu bar on this, the main, thread until the daemon has stopped,
/// then exits 0. `start` starts the daemon on the runtime; Quit wakes `quit`,
/// which the daemon's side waits on, and it calls [`Tray::stopped`] after.
pub fn run(quit: Arc<Notify>, start: impl FnOnce(Tray)) -> ! {
    let mut event_loop = EventLoopBuilder::<Wake>::with_user_event().build();
    // A menu bar item only: no Dock icon, no app menu.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let proxy = event_loop.create_proxy();
    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(Wake::Menu(e));
        }));
    }
    {
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                let _ = p.send_event(Wake::LeftClick);
            }
        }));
    }
    start(Tray(proxy));

    let mut tray: Option<TrayIcon> = None;
    let mut items: Option<Items> = None;
    let mut shown: Option<Shown> = None;
    let mut tick = Instant::now() + TICK;
    event_loop.run(move |event, _, flow| {
        let changed = match event {
            // The status item is made once the loop runs (macOS requires it).
            Event::NewEvents(StartCause::Init) => {
                let (m, it) = menu();
                tray = Some(
                    TrayIconBuilder::new()
                        .with_menu(Box::new(m))
                        .with_title(format!("{BRICK} …"))
                        .with_tooltip("bana: starting")
                        .with_menu_on_left_click(false)
                        .build()
                        .expect("tray icon"),
                );
                items = Some(it);
                false
            }
            // The elapsed time moves on: a fresh summary (Changed follows).
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                tick = Instant::now() + TICK;
                if let Some(s) = &shown {
                    s.daemon.summary();
                }
                false
            }
            Event::UserEvent(Wake::Stopped) => {
                tray = None;
                *flow = ControlFlow::Exit;
                return;
            }
            Event::UserEvent(Wake::Ready(d, url)) => {
                shown = Some(Shown {
                    summary: d.subscribe(),
                    daemon: d,
                    url,
                    running: None,
                    last: None,
                    open: None,
                });
                true
            }
            Event::UserEvent(Wake::Changed) => true,
            Event::UserEvent(Wake::LeftClick) => {
                if let Some(s) = &shown {
                    open(&s.url, s.open);
                }
                false
            }
            Event::UserEvent(Wake::Menu(e)) => {
                let Some(it) = &items else { return };
                if e.id == it.quit.id() {
                    quit.notify_one();
                    it.status.set_text("bana: stopping");
                    if let Some(t) = &tray {
                        t.set_title(Some(format!("{BRICK} stopping")));
                    }
                    for i in [&it.last, &it.open, &it.cancel, &it.quit] {
                        i.set_enabled(false);
                    }
                    it.pause.set_enabled(false);
                    // Nothing more to show or do until it has stopped.
                    shown = None;
                } else if let Some(s) = &shown {
                    if e.id == it.open.id() {
                        open(&s.url, s.open);
                    } else if e.id == it.last.id() {
                        open(&s.url, s.last);
                    } else if e.id == it.cancel.id() {
                        if let Some(id) = s.running {
                            if let Err(e) = s.daemon.cancel(id, "cancelled from the menu bar") {
                                eprintln!("bana-manager: {e}");
                            }
                        }
                    } else if e.id == it.pause.id() {
                        // The check mark already shows the new choice.
                        s.daemon.set_paused(it.pause.is_checked());
                    }
                }
                false
            }
            _ => false,
        };
        if let (true, Some(t), Some(it), Some(s)) = (changed, &tray, &items, &mut shown) {
            show(t, it, s);
        }
        *flow = ControlFlow::WaitUntil(tick);
    });
}
