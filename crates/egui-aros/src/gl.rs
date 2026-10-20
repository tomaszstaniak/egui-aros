//! The OpenGL renderer (feature `glow`): glutin's GLA context on the window, egui_glow's painter.

use std::num::NonZeroU32;
use std::sync::Arc;

use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextAttributesBuilder, NotCurrentGlContext as _, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay as _, GlDisplay as _};
use glutin::surface::{GlSurface as _, Surface, SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::HasWindowHandle as _;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

pub struct Gl {
    // Field order is drop order: the painter's GL objects go while the context is current.
    painter: egui_glow::Painter,
    surface: Surface<WindowSurface>,
    context: PossiblyCurrentContext,
    /// GL_VENDOR, GL_RENDERER, GL_VERSION.
    pub info: String,
}

/// Opens the window through glutin-winit and makes a GL context current on it. On failure the
/// window is still returned when it was opened, so the program can draw with the CPU instead.
pub fn open(event_loop: &ActiveEventLoop, attributes: WindowAttributes) -> (Option<Window>, Result<Gl, String>) {
    let built = glutin_winit::DisplayBuilder::new()
        .with_window_attributes(Some(attributes.clone()))
        .build(event_loop, ConfigTemplateBuilder::new(), |mut configs| configs.next().expect("GLA offers one config"));
    let (window, config) = match built {
        Ok((Some(window), config)) => (window, config),
        Ok((None, _)) => return (None, Err("no window".into())),
        Err(e) => return (event_loop.create_window(attributes).ok(), Err(format!("GL display: {e}"))),
    };
    let result = (|| {
        let display = config.display();
        let handle = window.window_handle().map_err(|e| e.to_string())?.as_raw();
        let context = unsafe { display.create_context(&config, &ContextAttributesBuilder::new().build(Some(handle))) }
            .map_err(|e| format!("GL context: {e}"))?;
        let size = window.inner_size();
        let surface = unsafe {
            display.create_window_surface(
                &config,
                &SurfaceAttributesBuilder::<WindowSurface>::new().build(
                    handle,
                    NonZeroU32::new(size.width.max(1)).expect("non-zero"),
                    NonZeroU32::new(size.height.max(1)).expect("non-zero"),
                ),
            )
        }
        .map_err(|e| format!("GL surface: {e}"))?;
        let context = context.make_current(&surface).map_err(|e| format!("GL make current: {e}"))?;
        let gl = Arc::new(unsafe { glow::Context::from_loader_function_cstr(|name| display.get_proc_address(name)) });
        let info = {
            use glow::HasContext as _;
            let get = |name| unsafe { gl.get_parameter_string(name) };
            format!("{} / {} / {}", get(glow::VENDOR), get(glow::RENDERER), get(glow::VERSION))
        };
        let painter = egui_glow::Painter::new(gl, "", None, false).map_err(|e| format!("GL painter: {e}"))?;
        Ok(Gl { painter, surface, context, info })
    })();
    (Some(window), result)
}

impl Gl {
    pub fn paint(
        &mut self,
        size: [u32; 2],
        clear: [u8; 3],
        ppp: f32,
        primitives: &[egui::ClippedPrimitive],
        textures: &mut egui::TexturesDelta,
    ) {
        let c = |v: u8| v as f32 / 255.0;
        self.painter.clear(size, [c(clear[0]), c(clear[1]), c(clear[2]), 1.0]);
        self.painter.paint_and_update_textures(size, ppp, primitives, textures);
    }

    /// The frame just painted, as RGBA rows from the top (before it is shown).
    pub fn read_pixels(&self, size: [u32; 2]) -> Vec<u8> {
        let image = self.painter.read_screen_rgba(size);
        image.pixels.iter().flat_map(|p| p.to_array()).collect()
    }

    pub fn present(&self) {
        if let Err(e) = self.surface.swap_buffers(&self.context) {
            log::warn!("GL swap: {e}");
        }
    }

    pub fn resize(&self, size: [u32; 2]) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(size[0]), NonZeroU32::new(size[1])) {
            self.surface.resize(&self.context, w, h);
        }
    }
}

impl Drop for Gl {
    fn drop(&mut self) {
        self.painter.destroy();
    }
}
