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

    /// The host is going away.
    fn on_exit(&mut self);
}

/// What puts the overlay on screen.
pub trait OverlayHost {
    /// Run until the UI asks to stop or the surface is destroyed.
    fn run(self: Box<Self>, ui: Box<dyn OverlayUi>) -> Result<()>;
}

/// Requested geometry, in logical pixels.
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
        return Box::new(crate::ui::host_layer::LayerShellHost { geometry, monitor });
    }
    if surface == "layer" {
        tracing::warn!("layer surface asked for but not compiled in; using the toplevel host");
    }
    Box::new(EframeHost {
        geometry,
        runner,
        monitor,
    })
}

/// Today's default host: an `xdg_toplevel` driven by eframe over winit.
///
/// A toplevel is the surface type that means "I am an application window", so
/// the compositor tiles it and focuses it, and `try_hyprland_float` undoes
/// that from outside afterwards. **That compensation is started here**, by the
/// host that causes the problem — a layer surface never tiles and never takes
/// focus, so its host will start nothing. That is the whole argument for M6.1,
/// expressed as a difference between two impls rather than a flag.
pub struct EframeHost {
    pub geometry: Geometry,
    pub runner: std::sync::Arc<dyn crate::process::CommandRunner>,
    /// Which monitor the overlay should be pinned to, by name or position.
    pub monitor: String,
}

impl OverlayHost for EframeHost {
    fn run(self: Box<Self>, ui: Box<dyn OverlayUi>) -> Result<()> {
        crate::ui::host_toplevel::try_hyprland_float(
            self.runner.clone(),
            self.monitor.clone(),
            &self.geometry,
        );

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
