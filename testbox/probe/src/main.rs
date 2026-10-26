//! The box's probe, built by the box's builder and run in its client image.
//!
//! - `probe` says it runs, and what it is, so a check can tell that the binary it hears is the one
//!   just built.
//! - `probe window` opens a window as large as the display and clears it to [`COLOUR`] with wgpu,
//!   and says which adapter it got: RADV with the host's GPU passed through, llvmpipe or lavapipe
//!   without (`cargo testbox up --no-gpu`). The `box` phase reads the colour back from the display
//!   and the adapter from the log.
//!
//! It presents without waiting for vertical blank: Xvnc has none, and FIFO presentation there
//! stalls (120 frames of `vkcube` took a minute; mailbox and immediate take a second or less).

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// What the probe prints first; the `box` phase waits for it.
const RUNS: &str = "probe: runs";
/// What it prints once it has its adapter, before the adapter's name, backend and type.
const ADAPTER: &str = "probe: adapter";
/// What it prints once its first frame is presented.
const PRESENTED: &str = "probe: presented";
/// The window's colour, `#c0392b`, written as is (the surface's format isn't sRGB).
const COLOUR: [u8; 3] = [0xc0, 0x39, 0x2b];
/// How often it draws again: a display that comes late still gets a frame soon.
const REDRAW: Duration = Duration::from_millis(100);

fn main() -> std::process::ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => {
            println!("{RUNS}, version {}", env!("CARGO_PKG_VERSION"));
            std::process::ExitCode::SUCCESS
        }
        Some("window") => match window() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("probe: {error}");
                std::process::ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("probe: {other}? usage: probe [window]");
            std::process::ExitCode::FAILURE
        }
    }
}

fn window() -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|error| format!("an event loop: {error}"))?;
    let mut probe = Probe::default();
    event_loop
        .run_app(&mut probe)
        .map_err(|error| format!("the event loop: {error}"))?;
    probe.failed.map_or(Ok(()), Err)
}

#[derive(Default)]
struct Probe {
    drawn: Option<Drawn>,
    failed: Option<String>,
}

/// The window and what it's drawn with.
struct Drawn {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    presented: bool,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.drawn.is_some() {
            return;
        }
        match pollster::block_on(open(event_loop)) {
            Ok(drawn) => self.drawn = Some(drawn),
            Err(error) => {
                self.failed = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(drawn) = &mut self.drawn else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                drawn.config.width = size.width;
                drawn.config.height = size.height;
                drawn.surface.configure(&drawn.device, &drawn.config);
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = drawn.draw() {
                    self.failed = Some(error);
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(drawn) = &self.drawn {
            drawn.window.request_redraw();
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + REDRAW));
        }
    }
}

/// A window the size of the display, on the adapter wgpu picks for it.
async fn open(event_loop: &ActiveEventLoop) -> Result<Drawn, String> {
    // Xvnc names no monitor primary: the first will do, it's the only one.
    let size = event_loop
        .primary_monitor()
        .or_else(|| event_loop.available_monitors().next())
        .map(|monitor| monitor.size())
        .ok_or("no monitor")?;
    let window = Arc::new(
        event_loop
            .create_window(
                Window::default_attributes()
                    .with_title("probe")
                    .with_decorations(false)
                    .with_inner_size(size),
            )
            .map_err(|error| format!("a window: {error}"))?,
    );
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let surface = instance
        .create_surface(window.clone())
        .map_err(|error| format!("a surface: {error}"))?;
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            ..Default::default()
        })
        .await
        .map_err(|error| format!("an adapter: {error}"))?;
    let info = adapter.get_info();
    println!(
        "{ADAPTER} {} ({:?}, {:?}, {})",
        info.name, info.backend, info.device_type, info.driver_info
    );
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .map_err(|error| format!("a device: {error}"))?;
    let capabilities = surface.get_capabilities(&adapter);
    // Eight bits a channel, as the display's depth 24, and no sRGB encoding, so the colour's
    // bytes arrive as they are.
    let format = [
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgba8Unorm,
    ]
    .into_iter()
    .find(|format| capabilities.formats.contains(format))
    .ok_or("no 8-bit surface format that isn't sRGB")?;
    let present_mode = [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
        .into_iter()
        .find(|mode| capabilities.present_modes.contains(mode))
        .unwrap_or(wgpu::PresentMode::Fifo);
    let size = window.inner_size();
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode,
        desired_maximum_frame_latency: 2,
        alpha_mode: capabilities
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto),
        view_formats: Vec::new(),
        color_space: wgpu::SurfaceColorSpace::Auto,
    };
    surface.configure(&device, &config);
    println!("probe: presenting {format:?}, {present_mode:?}");
    Ok(Drawn {
        window,
        surface,
        device,
        queue,
        config,
        presented: false,
    })
}

impl Drawn {
    /// One frame of [`COLOUR`].
    fn draw(&mut self) -> Result<(), String> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            // Resized or lost under it: configured again, and drawn on the next tick.
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            other => return Err(format!("a frame: {other:?}")),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let [red, green, blue] = COLOUR.map(|channel| f64::from(channel) / 255.0);
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: red,
                        g: green,
                        b: blue,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);
        if !self.presented {
            self.presented = true;
            println!("{PRESENTED}");
        }
        Ok(())
    }
}
