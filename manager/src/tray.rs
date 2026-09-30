//! The menu bar (macOS): 🧱, then what the daemon does (`🧱 4m +2`,
//! `🧱 paused`), as the status item's title, with no image. The words come
//! from [`actlog::tray_view`], tested on Linux; this file only shows them.
//!
//! - left click: opens the page on the running build (else the latest);
//! - right click: the menu (the status, the last result, Publish vX… while
//!   bana asks to publish a release, Fix #N with Claude… while the last build
//!   failed, Open bana, Cancel build, Pause automatic builds, Quit bana).
//!
//! Publish vX… opens the page on the release's card, where the notes are
//! read first: nothing is published from the menu.
//!
//! Fix #N with Claude… has the daemon make the fix (as the page's button
//! does), then opens Claude Code's claude-cli:// link, whose handler opens a
//! terminal in the fix's worktree with the prompt typed. If the fix cannot be
//! made, the page opens on the build instead: its button says why.
//!
//! It shows one project: the one whose build runs, else the first.
//!
//! It runs in the daemon's process, on the main thread (AppKit wants it),
//! while the daemon runs on tokio's threads. A task forwards the projects'
//! changes to the event loop; a 60 s tick keeps the elapsed time current.

use crate::actlog::{tray_view, BRICK};
use crate::daemon::Daemon;
use crate::registry::Registry;
use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const TICK: Duration = Duration::from_secs(60);
/// Where Publish vX… goes in the menu: after the last result.
const RELEASE_AT: usize = 2;
/// Where Fix #N with Claude… goes: after the last result, and Publish vX….
const FIX_AT: usize = 2;

pub enum Wake {
    /// The daemon runs: its projects, the page's URL, with the token, and
    /// the runtime the menu's longer actions run on.
    Ready(Arc<Registry>, String, Handle),
    Changed,
    Stopped,
    Menu(MenuEvent),
    LeftClick,
}

/// What the daemon's side tells the menu bar.
#[derive(Clone)]
pub struct Tray(EventLoopProxy<Wake>);

impl Tray {
    /// The daemon runs: the menu bar follows its projects from now on.
    /// Called on the runtime.
    pub fn ready(&self, r: &Arc<Registry>, url: String) {
        let _ = self
            .0
            .send_event(Wake::Ready(r.clone(), url, Handle::current()));
        let (mut rx, proxy) = (r.subscribe(), self.0.clone());
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
    /// The menu itself (it shares its items with the status item's), where
    /// the release and fix items come and go.
    menu: Menu,
    status: MenuItem,
    last: MenuItem,
    release: MenuItem,
    /// The release item is in the menu now.
    release_shown: Cell<bool>,
    fix: MenuItem,
    /// The fix item is in the menu now.
    fix_shown: Cell<bool>,
    open: MenuItem,
    cancel: MenuItem,
    pause: CheckMenuItem,
    quit: MenuItem,
}

/// What the menu bar knows of the daemon.
struct Shown {
    registry: Arc<Registry>,
    /// The project shown, and its prefix.
    daemon: Option<Daemon>,
    prefix: String,
    url: String,
    rt: Handle,
    running: Option<u64>,
    last: Option<u64>,
    open: Option<u64>,
    /// The build Fix with Claude… is for.
    fix: Option<u64>,
    /// The release Publish vX… opens.
    release: Option<String>,
}

fn menu() -> (Menu, Items) {
    let status = MenuItem::new("bana: starting", false, None);
    let last = MenuItem::new("Last: none yet", false, None);
    let open = MenuItem::new("Open bana", false, None);
    let cancel = MenuItem::new("Cancel build", false, None);
    let pause = CheckMenuItem::new("Pause automatic builds", false, false, None);
    let quit = MenuItem::new("Quit bana (local CI stops until next login)", true, None);
    let menu = Menu::with_items(&[
        &status,
        &last,
        &PredefinedMenuItem::separator(),
        &open,
        &cancel,
        &pause,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .expect("menu");
    let it = Items {
        menu: menu.clone(),
        status,
        last,
        release: MenuItem::new("Publish…", true, None),
        release_shown: Cell::new(false),
        fix: MenuItem::new("Fix with Claude…", true, None),
        fix_shown: Cell::new(false),
        open,
        cancel,
        pause,
        quit,
    };
    (menu, it)
}

/// The shown project's summary last published, into the title, the tooltip
/// and the menu. (Not `Daemon::summary`: that publishes, and would wake us
/// again.)
fn show(tray: &TrayIcon, it: &Items, s: &mut Shown) {
    let all = s.registry.daemons();
    let shown = all
        .iter()
        .find(|(_, d)| d.running().is_some())
        .or(all.first())
        .cloned();
    let Some((prefix, d)) = shown else {
        s.daemon = None;
        tray.set_title(Some(BRICK));
        let _ = tray.set_tooltip(Some("bana: no projects yet"));
        it.status
            .set_text("bana: no projects yet (bana add in a checkout)");
        for i in [&it.last, &it.cancel] {
            i.set_enabled(false);
        }
        it.pause.set_enabled(false);
        return;
    };
    let summary = d.published();
    (s.daemon, s.prefix) = (Some(d), prefix);
    let v = tray_view(&summary);
    s.running = summary.running.as_ref().map(|b| b.id);
    s.last = summary.last.as_ref().map(|b| b.id);
    s.open = v.open_build;
    s.fix = v.fix_build;
    s.release = v.release_tag.clone();
    tray.set_title(Some(&v.title));
    let _ = tray.set_tooltip(Some(&v.tooltip));
    it.status.set_text(&v.status_line);
    it.last
        .set_text(v.last_line.as_deref().unwrap_or("Last: none yet"));
    it.last.set_enabled(v.last_line.is_some());
    match &v.release_line {
        Some(line) => {
            it.release.set_text(line);
            it.release.set_enabled(v.release_enabled);
            if !it.release_shown.get() && it.menu.insert(&it.release, RELEASE_AT).is_ok() {
                it.release_shown.set(true);
            }
        }
        None => {
            if it.release_shown.get() && it.menu.remove(&it.release).is_ok() {
                it.release_shown.set(false);
            }
        }
    }
    match &v.fix_line {
        Some(line) => {
            it.fix.set_text(line);
            let at = FIX_AT + usize::from(it.release_shown.get());
            if !it.fix_shown.get() && it.menu.insert(&it.fix, at).is_ok() {
                it.fix_shown.set(true);
            }
        }
        None => {
            if it.fix_shown.get() && it.menu.remove(&it.fix).is_ok() {
                it.fix_shown.set(false);
            }
        }
    }
    it.open.set_enabled(true);
    it.cancel.set_enabled(v.cancel_enabled);
    it.pause.set_enabled(true);
    it.pause.set_checked(v.paused);
}

/// Opens the page (on project `p`'s build `id`) in the default browser; or,
/// with no build, any URL (Claude Code's link).
fn open(url: &str, p: &str, id: Option<u64>) {
    let url = match id {
        Some(id) => format!("{url}&p={p}&build={id}"),
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

/// Publish `tag`…: the page, on project `p`'s release's card (a tag is
/// `[A-Za-z0-9._-]`).
fn open_release(url: &str, p: &str, tag: &str) {
    open(&format!("{url}&p={p}&release={tag}"), p, None);
}

/// Fix #`id` with Claude…: the daemon makes the fix (git may take a while, so
/// on the runtime, not this thread), then its link goes to Claude Code's
/// handler. When it cannot, the page opens on the build.
fn fix(s: &Shown, id: u64) {
    let Some(d) = s.daemon.clone() else { return };
    let (url, p) = (s.url.clone(), s.prefix.clone());
    s.rt.spawn(async move {
        match d.fix(id).await {
            Ok(made) => open(&made.link, &p, None),
            Err(e) => {
                eprintln!("bana-manager: fix #{id}: {e}");
                open(&url, &p, Some(id));
            }
        }
    });
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
                    for (_, d) in s.registry.daemons() {
                        d.summary();
                    }
                }
                false
            }
            Event::UserEvent(Wake::Stopped) => {
                tray = None;
                *flow = ControlFlow::Exit;
                return;
            }
            Event::UserEvent(Wake::Ready(registry, url, rt)) => {
                shown = Some(Shown {
                    registry,
                    daemon: None,
                    prefix: String::new(),
                    url,
                    rt,
                    running: None,
                    last: None,
                    open: None,
                    fix: None,
                    release: None,
                });
                true
            }
            Event::UserEvent(Wake::Changed) => true,
            Event::UserEvent(Wake::LeftClick) => {
                if let Some(s) = &shown {
                    open(&s.url, &s.prefix, s.open);
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
                    for i in [
                        &it.last,
                        &it.release,
                        &it.fix,
                        &it.open,
                        &it.cancel,
                        &it.quit,
                    ] {
                        i.set_enabled(false);
                    }
                    it.pause.set_enabled(false);
                    // Nothing more to show or do until it has stopped.
                    shown = None;
                } else if let Some(s) = &shown {
                    if e.id == it.open.id() {
                        open(&s.url, &s.prefix, s.open);
                    } else if e.id == it.last.id() {
                        open(&s.url, &s.prefix, s.last);
                    } else if e.id == it.release.id() {
                        if let Some(tag) = &s.release {
                            open_release(&s.url, &s.prefix, tag);
                        }
                    } else if e.id == it.fix.id() {
                        if let Some(id) = s.fix {
                            fix(s, id);
                        }
                    } else if e.id == it.cancel.id() {
                        if let (Some(id), Some(d)) = (s.running, &s.daemon) {
                            if let Err(e) = d.cancel(id, "cancelled from the menu bar") {
                                eprintln!("bana-manager: {e}");
                            }
                        }
                    } else if e.id == it.pause.id() {
                        // The check mark already shows the new choice.
                        if let Some(d) = &s.daemon {
                            d.set_paused(it.pause.is_checked());
                        }
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
