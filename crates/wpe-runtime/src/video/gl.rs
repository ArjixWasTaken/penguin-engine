//! GPU output: mpv renders with OpenGL in a headless EGL context into
//! textures exported as dma-bufs, which wgpu imports once. With hardware
//! decoding, frames never leave the GPU; each new frame is a texture copy
//! into the layer's texture.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::fd::{FromRawFd, OwnedFd};
use std::ptr;
use std::sync::{Arc, Mutex};

use glow::HasContext;
use khronos_egl as egl;
use libmpv2::Mpv;
use libmpv2_sys as sys;
use wpe_render::{Gpu, GpuTexture, wgpu};

use super::{RenderContext, VideoError, Wake, params};

/// Shared textures per video: one being written, one ready, and the rest
/// covering copies the renderer has queued but the GPU hasn't run yet.
const BUFFERS: usize = 4;

/// `DRM_FORMAT_ABGR8888`: R, G, B, A bytes in memory order.
const FOURCC_ABGR8888: c_int = i32::from_le_bytes(*b"AB24");

const EGL_PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;

/// Which shared textures hold what.
#[derive(Default)]
pub struct Slots {
    /// The newest finished frame, not yet copied.
    ready: Option<usize>,
    /// Frames the renderer's queued copies still read.
    busy: Vec<usize>,
}

impl Slots {
    pub fn shared() -> Arc<Mutex<Slots>> {
        Arc::default()
    }
}

fn lock(slots: &Mutex<Slots>) -> std::sync::MutexGuard<'_, Slots> {
    slots.lock().unwrap_or_else(|e| e.into_inner())
}

/// One exported texture.
pub struct Dmabuf {
    fd: OwnedFd,
    modifier: u64,
    stride: u32,
    offset: u32,
}

/// The target must be a plain RGBA8 texture for frames to be copied into it.
pub fn check_target(gpu: &Gpu, target: &GpuTexture) -> Result<wgpu::TextureFormat, VideoError> {
    if !gpu
        .device
        .features()
        .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
    {
        return Err(VideoError::Share(
            "the GPU device can't import dma-bufs".to_owned(),
        ));
    }
    let format = target.texture.format();
    match format {
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => Ok(format),
        other => Err(VideoError::Share(format!("layer texture is {other:?}"))),
    }
}

/// The DRM render node of the renderer's GPU, where mpv opens VA-API so
/// decoded frames are on the same device.
pub fn render_node(gpu: &Gpu) -> Option<std::path::PathBuf> {
    let info = gpu.adapter.get_info();
    let id = |path: std::path::PathBuf| {
        let text = std::fs::read_to_string(path).ok()?;
        u32::from_str_radix(text.trim().trim_start_matches("0x"), 16).ok()
    };
    std::fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("renderD"))
        .find(|e| {
            id(e.path().join("device/vendor")) == Some(info.vendor)
                && id(e.path().join("device/device")) == Some(info.device)
        })
        .map(|e| std::path::Path::new("/dev/dri").join(e.file_name()))
}

type ExportQuery =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_int, *mut c_int, *mut u64) -> u32;
type Export = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_int, *mut i32, *mut i32) -> u32;

fn egl_err(what: &str) -> impl Fn(egl::Error) -> VideoError + '_ {
    move |e| VideoError::Gl(format!("{what}: {e}"))
}

unsafe extern "C" fn get_proc_address(_: *mut c_void, name: *const c_char) -> *mut c_void {
    let name = unsafe { CStr::from_ptr(name) };
    name.to_str()
        .ok()
        .and_then(|n| egl::API.get_proc_address(n))
        .map_or(ptr::null_mut(), |f| f as *mut c_void)
}

/// A headless OpenGL context current on the playback thread.
struct Context {
    display: egl::Display,
    context: egl::Context,
    gl: glow::Context,
}

impl Context {
    fn new() -> Result<Self, VideoError> {
        let api = &egl::API;
        let display = unsafe {
            api.get_platform_display(
                EGL_PLATFORM_SURFACELESS_MESA,
                egl::DEFAULT_DISPLAY,
                &[egl::ATTRIB_NONE],
            )
        }
        .map_err(egl_err("opening the EGL display"))?;
        api.initialize(display)
            .map_err(egl_err("initialising EGL"))?;
        api.bind_api(egl::OPENGL_API)
            .map_err(egl_err("selecting OpenGL"))?;
        // Nothing is drawn to a surface, so no config (EGL_KHR_no_config_context).
        let config = unsafe { egl::Config::from_ptr(ptr::null_mut()) };
        let context = api
            .create_context(
                display,
                config,
                None,
                &[
                    egl::CONTEXT_MAJOR_VERSION,
                    3,
                    egl::CONTEXT_MINOR_VERSION,
                    3,
                    egl::CONTEXT_OPENGL_PROFILE_MASK,
                    egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
                    egl::NONE,
                ],
            )
            .map_err(egl_err("creating the context"))?;
        api.make_current(display, None, None, Some(context))
            .map_err(egl_err("making the context current"))?;
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                api.get_proc_address(name)
                    .map_or(ptr::null(), |f| f as *const c_void)
            })
        };
        Ok(Self {
            display,
            context,
            gl,
        })
    }

    /// Export `texture` as a single-plane RGBA dma-buf.
    fn export(&self, texture: glow::Texture) -> Result<Dmabuf, VideoError> {
        let api = &egl::API;
        let proc = |name| {
            api.get_proc_address(name)
                .ok_or_else(|| VideoError::Gl(format!("{name} is unavailable")))
        };
        let query: ExportQuery =
            unsafe { std::mem::transmute(proc("eglExportDMABUFImageQueryMESA")?) };
        let export: Export = unsafe { std::mem::transmute(proc("eglExportDMABUFImageMESA")?) };

        let image = api
            .create_image(
                self.display,
                self.context,
                egl::GL_TEXTURE_2D as egl::Enum,
                unsafe { egl::ClientBuffer::from_ptr(texture.0.get() as usize as *mut c_void) },
                &[egl::ATTRIB_NONE],
            )
            .map_err(egl_err("wrapping the texture"))?;
        let (mut fourcc, mut planes, mut modifier) = (0, 0, 0u64);
        let (mut fd, mut stride, mut offset) = (-1, 0, 0);
        let ok = unsafe {
            query(
                self.display.as_ptr(),
                image.as_ptr(),
                &mut fourcc,
                &mut planes,
                &mut modifier,
            ) != 0
                && planes == 1
                && export(
                    self.display.as_ptr(),
                    image.as_ptr(),
                    &mut fd,
                    &mut stride,
                    &mut offset,
                ) != 0
        };
        let _ = api.destroy_image(self.display, image);
        if !ok || fd < 0 {
            return Err(VideoError::Gl(format!(
                "exporting the texture failed ({planes} planes)"
            )));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if fourcc != FOURCC_ABGR8888 {
            return Err(VideoError::Gl(format!(
                "texture exported as {:?}, not AB24",
                fourcc.to_le_bytes().map(char::from)
            )));
        }
        Ok(Dmabuf {
            fd,
            modifier,
            stride: stride as u32,
            offset: offset as u32,
        })
    }
}

pub struct Output {
    /// Dropped first, while the context is still current.
    render: Option<RenderContext>,
    targets: Vec<(glow::Texture, glow::Framebuffer)>,
    size: [i32; 2],
    slots: Arc<Mutex<Slots>>,
    /// Handed to mpv for VA-API; must outlive the render context.
    _drm: Option<(std::fs::File, Box<sys::mpv_opengl_drm_params_v2>)>,
    context: Context,
}

impl Output {
    pub fn new(
        mpv: &Mpv,
        wake: &Arc<Wake>,
        width: u32,
        height: u32,
        slots: Arc<Mutex<Slots>>,
        render_node: Option<std::path::PathBuf>,
    ) -> Result<(Self, Vec<Dmabuf>), VideoError> {
        use std::os::fd::AsRawFd;

        let drm = render_node.and_then(|path| {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .inspect_err(|e| tracing::warn!("opening {}: {e}", path.display()))
                .ok()?;
            let params = Box::new(sys::mpv_opengl_drm_params_v2 {
                fd: -1,
                crtc_id: 0,
                connector_id: 0,
                atomic_request_ptr: ptr::null_mut(),
                render_fd: file.as_raw_fd(),
            });
            Some((file, params))
        });
        let mut output = Self {
            render: None,
            targets: Vec::new(),
            size: [width as i32, height as i32],
            slots,
            _drm: drm,
            context: Context::new()?,
        };
        let mut buffers = Vec::new();
        for _ in 0..BUFFERS {
            let gl = &output.context.gl;
            let (texture, fbo) = unsafe {
                let texture = gl.create_texture().map_err(VideoError::Gl)?;
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.tex_storage_2d(
                    glow::TEXTURE_2D,
                    1,
                    glow::RGBA8,
                    width as i32,
                    height as i32,
                );
                let fbo = gl.create_framebuffer().map_err(VideoError::Gl)?;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(texture),
                    0,
                );
                (texture, fbo)
            };
            output.targets.push((texture, fbo));
            let status = unsafe { gl.check_framebuffer_status(glow::FRAMEBUFFER) };
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(VideoError::Gl(format!(
                    "framebuffer incomplete ({status:#x})"
                )));
            }
            buffers.push(output.context.export(texture)?);
        }
        unsafe {
            output.context.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            output.context.gl.finish();
        }

        let mut init = sys::mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx: ptr::null_mut(),
        };
        let drm = output
            ._drm
            .as_mut()
            .map_or(ptr::null_mut(), |(_, p)| &mut **p as *mut _ as *mut c_void);
        let mut create = params([
            (
                sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                sys::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            ),
            (
                sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                &mut init as *mut _ as *mut c_void,
            ),
        ]);
        if !drm.is_null() {
            create.insert(
                2,
                sys::mpv_render_param {
                    type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_DRM_DISPLAY_V2,
                    data: drm,
                },
            );
        }
        output.render = Some(RenderContext::new(mpv, wake, "OpenGL", create)?);
        Ok((output, buffers))
    }
}

impl super::Output for Output {
    fn render(&mut self) -> bool {
        let Some(render) = &self.render else {
            return false;
        };
        if !render.frame_due() {
            return false;
        }
        let free = {
            let slots = lock(&self.slots);
            (0..self.targets.len()).find(|i| slots.ready != Some(*i) && !slots.busy.contains(i))
        };
        // Every texture is in use: drop this frame.
        let Some(slot) = free else {
            return false;
        };
        let mut fbo = sys::mpv_opengl_fbo {
            fbo: self.targets[slot].1.0.get() as c_int,
            w: self.size[0],
            h: self.size[1],
            internal_format: glow::RGBA8 as c_int,
        };
        let mut flip: c_int = 0;
        let rendered = render.render(params([
            (
                sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
                &mut fbo as *mut _ as *mut c_void,
            ),
            (
                sys::mpv_render_param_type_MPV_RENDER_PARAM_FLIP_Y,
                &mut flip as *mut c_int as *mut c_void,
            ),
        ]));
        if !rendered {
            return false;
        }
        // The renderer reads the texture on another API; finish writing first.
        unsafe { self.context.gl.finish() };
        lock(&self.slots).ready = Some(slot);
        true
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.render.take();
        let gl = &self.context.gl;
        for (texture, fbo) in self.targets.drain(..) {
            unsafe {
                gl.delete_framebuffer(fbo);
                gl.delete_texture(texture);
            }
        }
        let api = &egl::API;
        let _ = api.make_current(self.context.display, None, None, None);
        let _ = api.destroy_context(self.context.display, self.context.context);
        // The display is shared by every video in the process: not terminated.
    }
}

/// Import a single-plane dma-buf as a texture frames are copied from.
#[allow(clippy::too_many_arguments)]
pub fn import(
    gpu: &Gpu,
    fd: OwnedFd,
    modifier: u64,
    stride: u32,
    offset: u32,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
    label: &str,
) -> Result<wgpu::Texture, VideoError> {
    use wgpu::hal::api::Vulkan;

    let raw = {
        let device = unsafe { gpu.device.as_hal::<Vulkan>() }
            .ok_or_else(|| VideoError::Share("the renderer isn't on Vulkan".to_owned()))?;
        let desc = wgpu::hal::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUses::COPY_SRC,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        unsafe { device.texture_from_dmabuf_fd(fd, &desc, modifier, stride as u64, offset as u64) }
            .map_err(|e| VideoError::Share(format!("importing a dma-buf: {e}")))?
    };
    Ok(unsafe {
        gpu.device.create_texture_from_hal::<Vulkan>(
            raw,
            &wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
            wgpu::TextureUses::COPY_SRC,
        )
    })
}

/// The renderer's side: the shared textures as wgpu sees them.
pub struct Frames {
    textures: Vec<wgpu::Texture>,
    slots: Arc<Mutex<Slots>>,
}

impl Frames {
    pub fn import(
        gpu: &Gpu,
        buffers: Vec<Dmabuf>,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        slots: Arc<Mutex<Slots>>,
    ) -> Result<Self, VideoError> {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let textures = buffers
            .into_iter()
            .map(|b| {
                import(
                    gpu,
                    b.fd,
                    b.modifier,
                    b.stride,
                    b.offset,
                    size,
                    format,
                    "video frame",
                )
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { textures, slots })
    }

    /// Copy the newest finished frame into `target`.
    pub fn copy_latest(&self, gpu: &Gpu, target: &GpuTexture) {
        let slot = {
            let mut slots = lock(&self.slots);
            let Some(slot) = slots.ready.take() else {
                return;
            };
            slots.busy.push(slot);
            slot
        };
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("video frame copy"),
            });
        encoder.copy_texture_to_texture(
            self.textures[slot].as_image_copy(),
            target.texture.as_image_copy(),
            self.textures[slot].size(),
        );
        gpu.queue.submit([encoder.finish()]);
        let slots = self.slots.clone();
        gpu.queue.on_submitted_work_done(move || {
            lock(&slots).busy.retain(|&b| b != slot);
        });
    }
}
