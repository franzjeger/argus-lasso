//! Compositor-side window opacity via Wayland wp_alpha_modifier_v1 protocol.
//!
//! At startup we create a secondary backend from eframe's already-open wl_display,
//! enumerate globals, and bind wp_alpha_modifier_v1 if the compositor advertises it
//! (KWin 6.x does).  Whenever the user changes the opacity slider we call
//! set_multiplier() — the change takes effect on the next wl_surface.commit() which
//! eframe issues automatically on the next render frame.

use std::ffi::c_void;

use wayland_backend::client::Backend;
use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry, wl_surface::WlSurface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::wp::alpha_modifier::v1::client::{
    wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1, wp_alpha_modifier_v1::WpAlphaModifierV1,
};

// ── Minimal Dispatch state ─────────────────────────────────────────────────────

struct OpacityState;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for OpacityState {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(OpacityState: ignore WpAlphaModifierV1);
delegate_noop!(OpacityState: ignore WpAlphaModifierSurfaceV1);
delegate_noop!(OpacityState: ignore WlSurface);

// ── WaylandOpacity ─────────────────────────────────────────────────────────────

pub struct WaylandOpacity {
    conn: Connection,
    surface_alpha: WpAlphaModifierSurfaceV1,
}

impl WaylandOpacity {
    /// Try to initialise compositor opacity control.
    ///
    /// `display_ptr` — raw `wl_display *` from eframe's display handle.
    /// `surface_ptr` — raw `wl_surface *` from eframe's window handle.
    ///
    /// Returns `None` if the compositor does not support `wp_alpha_modifier_v1`
    /// or if any step fails.
    pub fn new(display_ptr: *mut c_void, surface_ptr: *mut c_void) -> Option<Self> {
        if display_ptr.is_null() || surface_ptr.is_null() {
            log::warn!("wayland_opacity: null display or surface pointer");
            return None;
        }

        // SAFETY: display_ptr is a valid wl_display* from eframe. We create a
        // foreign backend that borrows the display without taking ownership.
        let backend = unsafe {
            Backend::from_foreign_display(display_ptr as *mut wayland_sys::client::wl_display)
        };
        let conn = Connection::from_backend(backend);

        // Enumerate compositor globals in a private event queue so we don't
        // interfere with eframe's queue.
        let (globals, mut queue) = match registry_queue_init::<OpacityState>(&conn) {
            Ok(x) => x,
            Err(e) => {
                log::warn!("wayland_opacity: registry_queue_init failed: {e}");
                return None;
            }
        };
        let qh = queue.handle();

        // Log every advertised global so the user can see what KWin offers.
        let contents = globals.contents();
        contents.with_list(|list| {
            for g in list {
                log::debug!("wayland global: {} v{}", g.interface, g.version);
            }
        });

        // Bind wp_alpha_modifier_v1.
        let alpha_modifier: WpAlphaModifierV1 = match globals.bind(&qh, 1..=1, ()) {
            Ok(am) => {
                log::info!("wayland_opacity: bound wp_alpha_modifier_v1");
                am
            }
            Err(e) => {
                log::warn!("wayland_opacity: wp_alpha_modifier_v1 not available — {e}");
                return None;
            }
        };

        // Flush bind request.
        if let Err(e) = queue.roundtrip(&mut OpacityState) {
            log::warn!("wayland_opacity: roundtrip after bind failed: {e}");
            return None;
        }

        // Wrap eframe's wl_surface* as a WlSurface proxy in our connection.
        // SAFETY: surface_ptr is a valid wl_proxy* created by eframe for the
        // application window surface.  We do not take ownership — we only borrow
        // it to send the get_surface request.
        let surface_id = unsafe {
            wayland_backend::client::ObjectId::from_ptr(
                WlSurface::interface(),
                surface_ptr as *mut wayland_sys::client::wl_proxy,
            )
        };
        let surface_id = match surface_id {
            Ok(id) => id,
            Err(e) => {
                log::warn!("wayland_opacity: ObjectId::from_ptr failed: {e:?}");
                return None;
            }
        };
        let surface = match WlSurface::from_id(&conn, surface_id) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("wayland_opacity: WlSurface::from_id failed: {e:?}");
                return None;
            }
        };

        // Request a WpAlphaModifierSurfaceV1 for this surface.
        let surface_alpha = alpha_modifier.get_surface(&surface, &qh, ());

        // Flush the get_surface request.
        if let Err(e) = queue.roundtrip(&mut OpacityState) {
            log::warn!("wayland_opacity: roundtrip after get_surface failed: {e}");
            return None;
        }

        log::info!("wayland_opacity: wp_alpha_modifier surface control ready");
        Some(Self {
            conn,
            surface_alpha,
        })
    }

    /// Set window opacity in [0.0, 1.0].
    ///
    /// set_multiplier is double-buffered; the change applies on the next
    /// wl_surface.commit which eframe issues automatically on the next render frame.
    pub fn set(&self, opacity: f32) {
        let val = (opacity.clamp(0.0, 1.0) as f64 * u32::MAX as f64) as u32;
        self.surface_alpha.set_multiplier(val);
        let _ = self.conn.flush();
        log::debug!("wayland_opacity: set_multiplier({val:#010x}) opacity={opacity:.3}");
    }
}

impl Drop for WaylandOpacity {
    fn drop(&mut self) {
        self.surface_alpha.destroy();
        // Without this, the destroy request can sit in the client-side
        // buffer unsent if nothing flushes before the connection itself
        // gets torn down — set() already flushes after every request for
        // the same reason.
        let _ = self.conn.flush();
    }
}
