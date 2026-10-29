//! X11 window tweaks egui cannot express. MyWhoosh runs fullscreen under Proton. KWin keeps
//! even "keep above" windows below the focused fullscreen window and only stacks
//! on-screen-display windows over it; GNOME's mutter does not know that type but keeps "above"
//! windows over fullscreen ones.
use anyhow::{Context, Result, anyhow};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, PropMode, Window};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Whether the app runs on X11 (see `prefer_x11` in main): an X server (XWayland on Wayland
/// desktops) answers. Without one the app stays on Wayland instead of failing to start.
pub fn active() -> bool {
    static ACTIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ACTIVE.get_or_init(|| std::env::var_os("DISPLAY").is_some() && x11rb::connect(None).is_ok())
}

/// Mark this process's still unmapped top-level window `title` as an on-screen display that
/// stays above others. Both are properties, which window managers read when the window is
/// mapped; requests sent while it is hidden are ignored. Window managers without the KDE type
/// fall back to a normal window kept above.
pub fn on_screen_display(title: &str) -> Result<()> {
    let (conn, screen) = x11rb::connect(None).context("connect to X11")?;
    let root = conn.setup().roots[screen].root;
    let window = find(&conn, root, title)?.ok_or_else(|| anyhow!("no X11 window {title:?}"))?;
    let atom =
        |name: &str| -> Result<u32> { Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom) };
    let types = [
        atom("_KDE_NET_WM_WINDOW_TYPE_ON_SCREEN_DISPLAY")?,
        atom("_NET_WM_WINDOW_TYPE_NORMAL")?,
    ];
    conn.change_property32(
        PropMode::REPLACE,
        window,
        atom("_NET_WM_WINDOW_TYPE")?,
        AtomEnum::ATOM,
        &types,
    )?
    .check()?;
    conn.change_property32(
        PropMode::REPLACE,
        window,
        atom("_NET_WM_STATE")?,
        AtomEnum::ATOM,
        &[atom("_NET_WM_STATE_ABOVE")?],
    )?
    .check()?;
    Ok(())
}

/// The pointer on the screen, in pixels. Window-relative positions shift while a window is
/// being moved, so dragging needs this one.
pub fn pointer() -> Option<(i16, i16)> {
    static CONNECTION: std::sync::OnceLock<Option<(RustConnection, Window)>> =
        std::sync::OnceLock::new();
    let (conn, root) = CONNECTION
        .get_or_init(|| {
            let (conn, screen) = x11rb::connect(None).ok()?;
            let root = conn.setup().roots[screen].root;
            Some((conn, root))
        })
        .as_ref()?;
    let reply = conn.query_pointer(*root).ok()?.reply().ok()?;
    Some((reply.root_x, reply.root_y))
}

fn find(conn: &RustConnection, root: Window, title: &str) -> Result<Option<Window>> {
    let pid_atom = conn.intern_atom(false, b"_NET_WM_PID")?.reply()?.atom;
    let pid = std::process::id();
    for w in conn.query_tree(root)?.reply()?.children {
        let owner = conn
            .get_property(false, w, pid_atom, AtomEnum::CARDINAL, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut v| v.next());
        if owner != Some(pid) {
            continue;
        }
        let name = conn
            .get_property(false, w, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 256)?
            .reply()?
            .value;
        if name == title.as_bytes() {
            return Ok(Some(w));
        }
    }
    Ok(None)
}
