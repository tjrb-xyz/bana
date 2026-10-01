//! The menu bar (macOS): 🧱, then what the daemon does (`🧱 4m +2`,
//! `🧱 paused`, `🧱 app 4m +2` with more than one project), as the status
//! item's title, with no image. The words come from [`actlog::tray_view`],
//! tested on Linux; this file only shows them.
//!
//! - left click: opens the page on the running build (else the first
//!   project's latest);
//! - right click: the menu: the machine's line, Open bana, one submenu per
//!   project, Quit bana. A project's submenu (`app: building main 1a2b3c4`)
//!   has its last result, Publish vX… while bana asks to publish a release,
//!   Fix #N with Claude… while its last build failed, Open, Cancel #N while
//!   a build runs, and Pause automatic builds. A project that cannot start
//!   says why instead, and how to mend it.
//!
//! Publish vX… opens the page on the release's card, where the notes are
//! read first: nothing is published from the menu.
//!
//! Fix #N with Claude… has the daemon make the fix (as the page's button
//! does), then opens Claude Code's claude-cli:// link, whose handler opens a
//! terminal in the fix's worktree with the prompt typed. If the fix cannot be
//! made, the page opens on the build instead: its button says why.
//!
//! A project's items have ids `<prefix>:<action>` ([`actlog::menu_action`]).
//! Submenus come and go with the projects (bana add, bana remove).
//!
//! It runs in the daemon's process, on the main thread (AppKit wants it),
//! while the daemon runs on tokio's threads. A task forwards the projects'
//! changes to the event loop; a 60 s tick keeps the elapsed time current.

use crate::actlog::{cut, menu_action, menu_id, tray_view, ProjectView, Summary, BRICK};
use crate::registry::Registry;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const TICK: Duration = Duration::from_secs(60);
/// Where the projects' submenus start in the menu: after the machine's
/// line, Open bana and a separator.
const PROJECTS_AT: usize = 3;

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

/// The menu's own items; the projects' submenus go between them.
struct Items {
    menu: Menu,
    status: MenuItem,
    open: MenuItem,
    quit: MenuItem,
}

/// One project's submenu and its items, each in it while it applies.
struct ShownProject {
    sub: Submenu,
    last: MenuItem,
    release: MenuItem,
    fix: MenuItem,
    open: MenuItem,
    cancel: MenuItem,
    pause: CheckMenuItem,
    /// Why it cannot start, and how to mend it.
    error: MenuItem,
    hint: MenuItem,
    /// The actions of the items in the submenu now, in order.
    shape: Vec<&'static str>,
    view: Option<ProjectView>,
}

impl ShownProject {
    fn new(p: &str) -> Self {
        let item = |a: &str, text: &str, on: bool| MenuItem::with_id(menu_id(p, a), text, on, None);
        Self {
            sub: Submenu::new(p, true),
            last: item("last", "Last: none yet", true),
            release: item("release", "Publish…", true),
            fix: item("fix", "Fix with Claude…", true),
            open: item("open", "Open", true),
            cancel: item("cancel", "Cancel build", true),
            pause: CheckMenuItem::with_id(
                menu_id(p, "pause"),
                "Pause automatic builds",
                true,
                false,
                None,
            ),
            error: MenuItem::new("", false, None),
            hint: MenuItem::new(
                format!("bana add in its checkout, or bana remove {p}"),
                false,
                None,
            ),
            shape: Vec::new(),
            view: None,
        }
    }

    fn item(&self, a: &str) -> &dyn IsMenuItem {
        match a {
            "last" => &self.last,
            "release" => &self.release,
            "fix" => &self.fix,
            "cancel" => &self.cancel,
            "pause" => &self.pause,
            "error" => &self.error,
            "hint" => &self.hint,
            _ => &self.open,
        }
    }

    /// Its view into the submenu: the texts, then the items that apply now.
    fn show(&mut self, v: ProjectView) {
        self.sub.set_text(&v.status_line);
        let mut want: Vec<&'static str> = Vec::new();
        if let Some(e) = &v.error {
            self.error.set_text(cut(e, 90));
            want.extend(["error", "hint", "open"]);
        } else {
            if let Some(l) = &v.last_line {
                self.last.set_text(l);
                want.push("last");
            }
            if let Some(l) = &v.release_line {
                self.release.set_text(l);
                self.release.set_enabled(v.release_enabled);
                want.push("release");
            }
            if let Some(l) = &v.fix_line {
                self.fix.set_text(l);
                want.push("fix");
            }
            want.push("open");
            if let Some(id) = v.cancel_build {
                self.cancel.set_text(format!("Cancel #{id}"));
                want.push("cancel");
            }
            self.pause.set_checked(v.paused);
            want.push("pause");
        }
        if want != self.shape {
            while self.sub.remove_at(0).is_some() {}
            for a in &want {
                let _ = self.sub.append(self.item(a));
            }
            self.shape = want;
        }
        self.view = Some(v);
    }
}

/// What the menu bar knows of the daemon.
struct Shown {
    registry: Arc<Registry>,
    url: String,
    rt: Handle,
    /// What a left click opens: a project, and its build.
    open: Option<(String, Option<u64>)>,
    projects: BTreeMap<String, ShownProject>,
}

fn menu() -> (Menu, Items) {
    let status = MenuItem::new("bana: starting", false, None);
    let open = MenuItem::new("Open bana", false, None);
    let quit = MenuItem::new("Quit bana (local CI stops until next login)", true, None);
    let menu = Menu::with_items(&[
        &status,
        &open,
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .expect("menu");
    let it = Items {
        menu: menu.clone(),
        status,
        open,
        quit,
    };
    (menu, it)
}

/// The projects' summaries last published, into the title, the tooltip and
/// the menu. (Not `Daemon::summary`: that publishes, and would wake us
/// again.)
fn show(tray: &TrayIcon, it: &Items, s: &mut Shown) {
    let all: Vec<Summary> = s
        .registry
        .daemons()
        .iter()
        .map(|(_, d)| d.published())
        .collect();
    let v = tray_view(&all, &s.registry.errors());
    tray.set_title(Some(&v.title));
    let _ = tray.set_tooltip(Some(&v.tooltip));
    it.status.set_text(&v.status_line);
    it.open.set_enabled(true);
    s.open = v.open;
    // A project added or removed: the submenus again, in order.
    if !s.projects.keys().eq(v.projects.iter().map(|p| &p.prefix)) {
        for p in s.projects.values() {
            let _ = it.menu.remove(&p.sub);
        }
        let mut old = std::mem::take(&mut s.projects);
        for (i, pv) in v.projects.iter().enumerate() {
            let p = old
                .remove(&pv.prefix)
                .unwrap_or_else(|| ShownProject::new(&pv.prefix));
            let _ = it.menu.insert(&p.sub, PROJECTS_AT + i);
            s.projects.insert(pv.prefix.clone(), p);
        }
    }
    for pv in v.projects {
        if let Some(p) = s.projects.get_mut(&pv.prefix) {
            p.show(pv);
        }
    }
}

/// The page on project `p` (and its build `id`).
fn page(url: &str, p: &str, id: Option<u64>) -> String {
    match id {
        Some(id) => format!("{url}&p={p}&build={id}"),
        None => format!("{url}&p={p}"),
    }
}

/// Opens a URL in the default browser (or Claude Code's link in its handler).
fn open(url: &str) {
    match std::process::Command::new("open").arg(url).spawn() {
        // Reaped off the main thread.
        Ok(mut c) => {
            std::thread::spawn(move || c.wait());
        }
        Err(e) => eprintln!("bana-manager: open: {e}"),
    }
}

/// Fix #`id` with Claude…: project `p`'s daemon makes the fix (git may take a
/// while, so on the runtime, not this thread), then its link goes to Claude
/// Code's handler. When it cannot, the page opens on the build.
fn fix(s: &Shown, p: &str, id: u64) {
    let Some(d) = s.registry.daemon(p) else {
        return;
    };
    let back = page(&s.url, p, Some(id));
    s.rt.spawn(async move {
        match d.fix(id).await {
            Ok(made) => open(&made.link),
            Err(e) => {
                eprintln!("bana-manager: fix #{id}: {e}");
                open(&back);
            }
        }
    });
}

/// Project `p`'s menu item `a` was chosen.
fn act(s: &Shown, p: &str, a: &str) {
    let Some(sp) = s.projects.get(p) else { return };
    let Some(v) = &sp.view else { return };
    match a {
        "open" => open(&page(&s.url, p, v.open_build)),
        "last" => open(&page(&s.url, p, v.last_build)),
        // A tag is `[A-Za-z0-9._-]`.
        "release" => {
            if let Some(tag) = &v.release_tag {
                open(&format!("{}&release={tag}", page(&s.url, p, None)));
            }
        }
        "fix" => {
            if let Some(id) = v.fix_build {
                fix(s, p, id);
            }
        }
        "cancel" => {
            if let (Some(id), Some(d)) = (v.cancel_build, s.registry.daemon(p)) {
                if let Err(e) = d.cancel(id, "cancelled from the menu bar") {
                    eprintln!("bana-manager: {e}");
                }
            }
        }
        // The check mark already shows the new choice.
        "pause" => {
            if let Some(d) = s.registry.daemon(p) {
                d.set_paused(sp.pause.is_checked());
            }
        }
        _ => {}
    }
}

/// What a left click and Open bana open.
fn open_shown(s: &Shown) {
    match &s.open {
        Some((p, id)) => open(&page(&s.url, p, *id)),
        None => open(&s.url),
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
            // The elapsed time moves on: fresh summaries (Changed follows).
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
                    url,
                    rt,
                    open: None,
                    projects: BTreeMap::new(),
                });
                true
            }
            Event::UserEvent(Wake::Changed) => true,
            Event::UserEvent(Wake::LeftClick) => {
                if let Some(s) = &shown {
                    open_shown(s);
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
                    it.open.set_enabled(false);
                    it.quit.set_enabled(false);
                    if let Some(s) = &shown {
                        for p in s.projects.values() {
                            p.sub.set_enabled(false);
                        }
                    }
                    // Nothing more to show or do until it has stopped.
                    shown = None;
                } else if let Some(s) = &shown {
                    if e.id == it.open.id() {
                        open_shown(s);
                    } else if let Some((p, a)) = menu_action(e.id.as_ref()) {
                        act(s, p, a);
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
