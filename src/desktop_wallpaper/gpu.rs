//! Desktop-only GPU presentation. Decoding and the lock renderer are unchanged.
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use calloop::ping::Ping;
use iced::futures::executor::block_on;
use iced::wgpu::{self, util::DeviceExt};
use iced_runtime::window::raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle, WindowHandle,
};
use wayland_client::{protocol::wl_surface::WlSurface, Connection, Proxy};

use crate::wallpaper::gpu_conversion::{Converter, P010Frame};

/// wgpu 27 may also panic while retiring an acquired image after device loss.
/// Take ownership before dropping so failed teardown cannot be retried, and
/// retire targets before the device and the native Wayland surface.
pub(super) fn retire<T>(resource: T) {
    if catch_unwind(AssertUnwindSafe(|| drop(resource))).is_err() {
        eprintln!("genkan wallpaper: GPU teardown failed; continuing with CPU presentation");
    }
}

// The connection is retained by wgpu. The owner drops Target before destroying
// the layer's wl_surface (a cloned proxy alone does not keep that object alive).
struct Window {
    connection: Connection,
    surface: WlSurface,
}

impl HasDisplayHandle for Window {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let pointer = NonNull::new(self.connection.backend().display_ptr().cast())
            .ok_or(HandleError::Unavailable)?;
        // SAFETY: the owned connection keeps the display alive for this borrow.
        Ok(unsafe {
            DisplayHandle::borrow_raw(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                pointer,
            )))
        })
    }
}

impl HasWindowHandle for Window {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let pointer =
            NonNull::new(self.surface.id().as_ptr().cast()).ok_or(HandleError::Unavailable)?;
        // SAFETY: Target is destroyed before the owning layer surface.
        Ok(unsafe {
            WindowHandle::borrow_raw(RawWindowHandle::Wayland(WaylandWindowHandle::new(pointer)))
        })
    }
}

pub(super) struct Renderer {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    failed: Arc<AtomicBool>,
    wake: Ping,
    texture: Option<wgpu::Texture>,
    conversion: Converter,
    pub rgba_required: bool,
    pub uploaded: bool,
}

pub(super) struct Target {
    surface: wgpu::Surface<'static>,
    config: Option<wgpu::SurfaceConfiguration>,
    pipeline: Option<wgpu::RenderPipeline>,
    coordinates: Option<wgpu::Buffer>,
    source_size: (u32, u32),
}

impl Renderer {
    pub fn new(
        connection: &Connection,
        surface: &WlSurface,
        wake: Ping,
    ) -> Result<(Self, Target), String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let target = Self::create_target(&instance, connection, surface)?;
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&target.surface),
            force_fallback_adapter: false,
        }))
        .map_err(|error| error.to_string())?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu
            && std::env::var("GENKAN_WALLPAPER_RENDERER").as_deref() != Ok("vulkan")
        {
            return Err("software Vulkan adapter; retaining CPU presentation".into());
        }
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("wallpaper"),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|error| error.to_string())?;
        let failed = Arc::new(AtomicBool::new(false));
        let lost = failed.clone();
        let lost_wake = wake.clone();
        device.set_device_lost_callback(move |_, _| {
            lost.store(true, Ordering::Relaxed);
            lost_wake.ping();
        });
        let errors = failed.clone();
        let error_wake = wake.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            eprintln!("genkan wallpaper: GPU error: {error}");
            errors.store(true, Ordering::Relaxed);
            error_wake.ping();
        }));
        eprintln!(
            "genkan wallpaper: GPU presentation on {} ({:?})",
            info.name, info.device_type
        );
        Ok((
            Self {
                instance,
                adapter,
                device,
                queue,
                failed,
                wake,
                texture: None,
                conversion: Converter::default(),
                rgba_required: false,
                uploaded: false,
            },
            target,
        ))
    }

    fn create_target(
        instance: &wgpu::Instance,
        connection: &Connection,
        surface: &WlSurface,
    ) -> Result<Target, String> {
        let surface = instance
            .create_surface(Window {
                connection: connection.clone(),
                surface: surface.clone(),
            })
            .map_err(|error| error.to_string())?;
        Ok(Target {
            surface,
            config: None,
            pipeline: None,
            coordinates: None,
            source_size: (0, 0),
        })
    }

    pub fn target(&self, connection: &Connection, surface: &WlSurface) -> Result<Target, String> {
        Self::create_target(&self.instance, connection, surface)
    }

    pub fn failed(&self) -> bool {
        // Native wgpu delivers loss callbacks only while polling/submitting.
        // The existing one-second resume timer also drives this when idle;
        // Poll never waits for GPU work or adds a new periodic wakeup.
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.failed.load(Ordering::Relaxed)
    }

    fn upload(
        &mut self,
        frame: Option<(u32, u32, &[u8])>,
        p010: Option<&P010Frame>,
    ) -> Result<(), String> {
        if self.failed.load(Ordering::Relaxed) {
            return Err("GPU device failed".into());
        }
        if self.uploaded {
            return Ok(());
        }
        let fallback = [
            super::FALLBACK_RGB[0],
            super::FALLBACK_RGB[1],
            super::FALLBACK_RGB[2],
            255,
        ];
        let (width, height, pixels) = frame.unwrap_or((1, 1, &fallback));
        let (width, height) = p010.map_or((width, height), |source| {
            (source.info.width(), source.info.height())
        });
        let limit = self.device.limits().max_texture_dimension_2d;
        if width > limit || height > limit {
            return Err("wallpaper exceeds GPU texture limits".into());
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        if self
            .texture
            .as_ref()
            .is_none_or(|texture| texture.size() != size)
        {
            self.texture = Some(self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shared wallpaper frame"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Uint,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }));
        }
        let cpu_pixels;
        let pixels = if let Some(source) = p010 {
            if self.conversion.upload(
                source,
                &self.device,
                &self.queue,
                &self.failed,
                self.texture.as_ref().unwrap(),
            )? {
                self.uploaded = true;
                return Ok(());
            }
            if self.failed.load(Ordering::Relaxed) {
                return Err("GPU device failed".into());
            }
            self.rgba_required = true;
            cpu_pixels = source.rgba().map_err(|error| error.to_string())?;
            cpu_pixels.as_ref()
        } else {
            pixels
        };
        self.queue.write_texture(
            self.texture.as_ref().unwrap().as_image_copy(),
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            size,
        );
        self.uploaded = true;
        Ok(())
    }

    pub fn draw(
        &mut self,
        target: &mut Target,
        size: (u32, u32),
        frame: Option<(u32, u32, &[u8])>,
        p010: Option<&P010Frame>,
    ) -> Result<wgpu::SurfaceTexture, String> {
        // wgpu reports some device-loss errors (including acquired-texture
        // discard) as panics rather than through on_uncaptured_error. Do not
        // reuse this device after unwinding; the desktop can keep using SHM.
        match catch_unwind(AssertUnwindSafe(|| {
            self.draw_inner(target, size, frame, p010)
        })) {
            Ok(result) => result,
            Err(_) => {
                self.failed.store(true, Ordering::Relaxed);
                self.wake.ping();
                Err("GPU rendering failed".into())
            }
        }
    }

    pub fn present(&self, output: wgpu::SurfaceTexture) -> Result<(), String> {
        if catch_unwind(AssertUnwindSafe(|| output.present())).is_err() {
            self.failed.store(true, Ordering::Relaxed);
            self.wake.ping();
            return Err("GPU presentation failed".into());
        }
        Ok(())
    }

    fn draw_inner(
        &mut self,
        target: &mut Target,
        size: (u32, u32),
        frame: Option<(u32, u32, &[u8])>,
        p010: Option<&P010Frame>,
    ) -> Result<wgpu::SurfaceTexture, String> {
        if self.failed.load(Ordering::Relaxed) {
            return Err("GPU device failed".into());
        }
        let limit = self.device.limits().max_texture_dimension_2d;
        if size.0 > limit || size.1 > limit {
            return Err("output exceeds GPU texture limits".into());
        }
        let resize = target
            .config
            .as_ref()
            .is_none_or(|config| (config.width, config.height) != size);
        if resize {
            let caps = target.surface.get_capabilities(&self.adapter);
            let format = [
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureFormat::Rgba8Unorm,
            ]
            .into_iter()
            .find(|format| caps.formats.contains(format))
            .ok_or("GPU surface has no unorm format preserving wallpaper colors")?;
            if !caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
                return Err("GPU surface has no opaque presentation mode".into());
            }
            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: size.0,
                height: size.1,
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: wgpu::CompositeAlphaMode::Opaque,
                view_formats: vec![],
            };
            target.surface.configure(&self.device, &config);
            let shader = self
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("wallpaper cover"),
                    source: wgpu::ShaderSource::Wgsl(include_str!("cover.wgsl").into()),
                });
            target.pipeline = Some(self.device.create_render_pipeline(
                &wgpu::RenderPipelineDescriptor {
                    label: Some("wallpaper cover"),
                    layout: None,
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vertex"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fragment"),
                        compilation_options: Default::default(),
                        targets: &[Some(format.into())],
                    }),
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    multiview: None,
                    cache: None,
                },
            ));
            target.config = Some(config);
        }
        // Acquisition can block for up to wgpu's timeout. Fall back on the
        // first failure instead of letting one output repeatedly stall all
        // others. Normal playback is gated by Wayland frame callbacks.
        let output = target
            .surface
            .get_current_texture()
            .map_err(|error| error.to_string())?;
        // Do not stage uploads until a swapchain image is available. Otherwise
        // hidden outputs can accumulate unsubmitted writes on every retry.
        self.upload(frame, p010)?;
        let texture = self.texture.as_ref().expect("upload precedes draw");
        let source_size = (texture.width(), texture.height());
        if resize || target.source_size != source_size {
            let coordinates = cover_coordinates(source_size, size);
            let bytes: Vec<u8> = coordinates
                .iter()
                .flat_map(|value| value.to_ne_bytes())
                .collect();
            target.coordinates = Some(self.device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("exact cover coordinates"),
                    contents: &bytes,
                    usage: wgpu::BufferUsages::STORAGE,
                },
            ));
            target.source_size = source_size;
        }
        let pipeline = target.pipeline.as_ref().unwrap();
        let source = texture.create_view(&Default::default());
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wallpaper frame and crop"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: target.coordinates.as_ref().unwrap().as_entire_binding(),
                },
            ],
        });
        let view = output.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wallpaper"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        Ok(output)
    }
}

// Use the CPU renderer's exact integer crop and nearest-neighbor sampling.
// A small lookup table avoids both per-fragment division and GPU rounding drift.
fn cover_coordinates((fw, fh): (u32, u32), (w, h): (u32, u32)) -> Vec<u32> {
    let (fw, fh, w, h) = (u128::from(fw), u128::from(fh), u128::from(w), u128::from(h));
    let (sw, sh) = if fw * h > w * fh {
        (fw * h / fh, h)
    } else {
        (w, fh * w / fw)
    };
    let (cx, cy) = ((sw - w) / 2, (sh - h) / 2);
    std::iter::once(w as u32)
        .chain((0..w).map(|x| (((x + cx) * fw / sw).min(fw - 1)) as u32))
        .chain((0..h).map(|y| (((y + cy) * fh / sh).min(fh - 1)) as u32))
        .collect()
}
