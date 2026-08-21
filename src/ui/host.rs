//! The seam between what the overlay draws and what puts it on screen.
//!
//! The overlay knows egui and nothing else — not winit, not Wayland, not
//! which surface type it lives on. A host knows one protocol and nothing
//! about transcription. Adding `zwlr_layer_shell_v1` (M6.1) is a new impl of
//! `OverlayHost`, not a `cfg` in the drawing code.
//!
//! This is the rule `Injector` already follows for text injection, applied to
//! the window instead of the keyboard: **a new platform is a new impl, never a
//! branch at the call site**. It is also why the seam is a `&mut egui::Ui`
//! rather than a `&egui::Context` — eframe opens the panel and hands one over,
//! and a raw host opens its own; both can satisfy that, while only one of them
//! can satisfy the other.

use anyhow::{anyhow, Result};
use eframe::egui;
use std::time::Duration;

/// Whether the overlay wants to keep running.
///
/// Returned rather than acted on: closing is `ViewportCommand::Close` under
/// eframe and destroying a layer surface under wlr-layer-shell, and the UI
/// should not have to know which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Exit,
}

/// A layout change the overlay is asking its host for.
///
/// The direction the seam did not have. Everything so far went host to UI —
/// paint this, tick that — because the surface was fixed at startup. Moving
/// the overlay between monitors is the one thing the UI can decide and only
/// the host can carry out, now that no window manager can be asked to do it.
///
/// Opacity is deliberately absent: that is paint, and paint never leaves the
/// UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutRequest {
    /// Logical size.
    pub size: Option<(f32, f32)>,
    /// Gap from the bottom edge of the monitor.
    pub bottom_margin: Option<f32>,
    /// `-1` or `+1`: the monitor left or right of the current one, in
    /// spatial order.
    pub monitor_step: Option<i32>,
}

/// What a host needs from the overlay.
pub trait OverlayUi: Send {
    /// Take in whatever arrived since the last frame, and say whether to go on.
    fn tick(&mut self) -> Flow;

    /// Paint one frame into a panel the host has already opened.
    fn paint(&mut self, ui: &mut egui::Ui);

    /// Premultiplied RGBA the host clears to before painting.
    fn clear_color(&self) -> [f32; 4];

    /// How long the host may idle before painting again. The overlay shows
    /// live speech, so this is a ceiling on latency, not a frame rate.
    fn repaint_after(&self) -> Duration {
        Duration::from_millis(50)
    }

    /// A layout change the UI wants, *taken* rather than read, so it fires
    /// once. Same shape as the session request the record button uses: the UI
    /// asks, and whoever owns the thing decides.
    fn take_layout_request(&mut self) -> Option<LayoutRequest> {
        None
    }

    /// The host is going away.
    fn on_exit(&mut self);
}

/// What puts the overlay on screen.
pub trait OverlayHost {
    /// Run until the UI asks to stop or the surface is destroyed.
    fn run(self: Box<Self>, ui: Box<dyn OverlayUi>) -> Result<()>;
}

/// Requested geometry, in logical pixels.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub width: f32,
    pub height: f32,
    /// Gap between the overlay and the bottom edge of its monitor.
    pub bottom_margin: f32,
}

/// The host this build uses.
///
/// **The single place the surface protocol is chosen.** Everything downstream
/// sees `dyn OverlayHost`, so no `cfg` reaches the drawing code, the event
/// handling, or `run_overlay` — the same containment `platform_injector()`
/// gives text injection.
pub fn default_host(
    geometry: Geometry,
    runner: std::sync::Arc<dyn crate::process::CommandRunner>,
    monitor: String,
    surface: &str,
) -> Box<dyn OverlayHost> {
    #[cfg(feature = "layer-shell")]
    if matches!(surface, "auto" | "layer") {
        tracing::info!(surface, "overlay on a wlr layer surface");
        return Box::new(crate::ui::host_layer::LayerShellHost {
            geometry,
            monitor: monitor.clone(),
            // Not every compositor implements wlr-layer-shell — GNOME does
            // not. Now that this is the default, a missing protocol has to
            // mean "use the other host", not "the overlay never appears".
            fallback: Some(Box::new(EframeHost {
                geometry,
                correction: Correction::for_this_platform(runner, monitor),
            })),
        });
    }
    if surface == "layer" {
        tracing::warn!("layer surface asked for but not compiled in; using the toplevel host");
    }
    Box::new(EframeHost {
        geometry,
        // Whether a toplevel needs correcting, and how, is a property of the
        // compositor — not of eframe. On macOS or Windows the window manager
        // does not tile this window and there is nothing to undo, so nothing
        // is attached and no `hyprctl` is ever spelled out on a machine that
        // has never heard of it.
        correction: Correction::for_this_platform(runner, monitor),
    })
}

/// What a toplevel needs done to it after the compositor has placed it.
///
/// A toplevel is the surface type that means "I am an application window", so
/// a tiling compositor tiles it and focuses it, and something has to undo that
/// from outside afterwards. **Which compositor, and therefore which fix, is
/// not eframe's business** — eframe runs on three platforms and only one of
/// them has this problem. A layer surface has none of it by construction,
/// which is the argument for M6.1.
pub enum Correction {
    /// Nothing to undo: the window manager does not tile this window.
    None,
    /// Hyprland: poll for the window and set float/pin/position.
    HyprlandFloat {
        runner: std::sync::Arc<dyn crate::process::CommandRunner>,
        /// Which monitor the overlay should be pinned to, by name or position.
        monitor: String,
    },
}

impl Correction {
    /// The one place a target OS is named. Everything else sees `Correction`.
    pub fn for_this_platform(
        runner: std::sync::Arc<dyn crate::process::CommandRunner>,
        monitor: String,
    ) -> Self {
        if cfg!(target_os = "linux") {
            // Still a guess about *which* Linux compositor, which is why the
            // correction itself checks for hyprctl before doing anything.
            Correction::HyprlandFloat { runner, monitor }
        } else {
            Correction::None
        }
    }

    fn start(&self, geometry: &Geometry) {
        match self {
            Correction::None => {}
            Correction::HyprlandFloat { runner, monitor } => {
                crate::ui::host_toplevel::try_hyprland_float(
                    runner.clone(),
                    monitor.clone(),
                    geometry,
                );
            }
        }
    }
}

/// Today's default host: an `xdg_toplevel` driven by eframe over winit.
pub struct EframeHost {
    pub geometry: Geometry,
    pub correction: Correction,
}

impl OverlayHost for EframeHost {
    fn run(self: Box<Self>, ui: Box<dyn OverlayUi>) -> Result<()> {
        self.correction.start(&self.geometry);

        let viewport = egui::ViewportBuilder::default()
            .with_title("oc-voice")
            .with_app_id("oc-voice")
            .with_inner_size([self.geometry.width, self.geometry.height])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(true);

        eframe::run_native(
            "oc-voice",
            eframe::NativeOptions {
                viewport,
                ..Default::default()
            },
            Box::new(|_cc| Ok(Box::new(Adapter { ui }))),
        )
        .map_err(|e| anyhow!("eframe error: {e}"))
    }
}

/// Translates the portable contract into `eframe::App`.
struct Adapter {
    ui: Box<dyn OverlayUi>,
}

impl eframe::App for Adapter {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.ui.clear_color()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.ui.tick() == Flow::Exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(self.ui.repaint_after());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui.paint(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.ui.on_exit();
    }
}
