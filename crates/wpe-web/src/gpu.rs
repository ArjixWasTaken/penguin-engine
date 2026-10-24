//! Frames that stay on the GPU: after each paint, Servo's framebuffer is
//! copied into one of a few textures made in its OpenGL context and
//! exported once as dma-bufs, which the renderer imports into wgpu. Like
//! video frames (`wpe-runtime`'s `video::gl`), only the slot index moves.

use std::ffi::{c_int, c_void};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex, MutexGuard};

use glow::HasContext;
use khronos_egl as egl;

/// One being written, one ready, the rest covering copies the renderer has
/// queued but the GPU hasn't run yet.
const BUFFERS: usize = 4;

/// `DRM_FORMAT_ABGR8888`: R, G, B, A bytes in memory order.
const FOURCC_ABGR8888: c_int = i32::from_le_bytes(*b"AB24");

/// One exported texture.
pub struct Dmabuf {
    pub fd: OwnedFd,
    pub modifier: u64,
    pub stride: u32,
    pub offset: u32,
}

/// Which shared textures hold what.
#[derive(Default)]
pub struct Slots {
    /// The newest finished frame, not yet copied.
    ready: Option<usize>,
    /// Frames the renderer's queued copies still read.
    busy: Vec<usize>,
}

impl Slots {
    pub fn lock(slots: &Mutex<Slots>) -> MutexGuard<'_, Slots> {
        lock(slots)
    }

    /// The newest frame for the renderer to copy, marked busy until
    /// [`Self::release`].
    pub fn take_ready(&mut self) -> Option<usize> {
        let slot = self.ready.take()?;
        self.busy.push(slot);
        Some(slot)
    }

    /// The renderer's copy of `slot` has run.
    pub fn release(&mut self, slot: usize) {
        self.busy.retain(|&b| b != slot);
    }
}

pub fn lock(slots: &Mutex<Slots>) -> MutexGuard<'_, Slots> {
    slots.lock().unwrap_or_else(|e| e.into_inner())
}

/// A page's shared textures as the renderer imports them: `width`×`height`
/// RGBA8, rows top-down.
pub struct GpuFrames {
    pub width: u32,
    pub height: u32,
    pub buffers: Vec<Dmabuf>,
    pub slots: Arc<Mutex<Slots>>,
}

/// The textures on Servo's side.
pub struct Ring {
    targets: Vec<(glow::Texture, glow::Framebuffer)>,
    size: (u32, u32),
    slots: Arc<Mutex<Slots>>,
}

type ExportQuery =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_int, *mut c_int, *mut u64) -> u32;
type Export = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_int, *mut i32, *mut i32) -> u32;

impl Ring {
    /// Make the textures in the current (Servo's) context and export them.
    pub fn new(gl: &glow::Context, size: (u32, u32)) -> Result<(Self, GpuFrames), String> {
        let api = &egl::API;
        let display = api
            .get_current_display()
            .ok_or("the page's context isn't EGL")?;
        let context = api
            .get_current_context()
            .ok_or("the page's context isn't EGL")?;
        let slots = Arc::new(Mutex::new(Slots::default()));
        let mut ring = Self {
            targets: Vec::new(),
            size,
            slots: slots.clone(),
        };
        let mut buffers = Vec::new();
        for _ in 0..BUFFERS {
            let (texture, fbo) = unsafe {
                let texture = gl.create_texture()?;
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.tex_storage_2d(
                    glow::TEXTURE_2D,
                    1,
                    glow::RGBA8,
                    size.0 as i32,
                    size.1 as i32,
                );
                gl.bind_texture(glow::TEXTURE_2D, None);
                let fbo = gl.create_framebuffer()?;
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(
                    glow::DRAW_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(texture),
                    0,
                );
                (texture, fbo)
            };
            ring.targets.push((texture, fbo));
            let status = unsafe { gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER) };
            if status != glow::FRAMEBUFFER_COMPLETE {
                ring.delete(gl);
                return Err(format!("framebuffer incomplete ({status:#x})"));
            }
            match export(display, context, texture) {
                Ok(buffer) => buffers.push(buffer),
                Err(e) => {
                    ring.delete(gl);
                    return Err(e);
                }
            }
        }
        unsafe {
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.finish();
        }
        let frames = GpuFrames {
            width: size.0,
            height: size.1,
            buffers,
            slots,
        };
        Ok((ring, frames))
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Copy the painted framebuffer `source` into a free texture, flipped to
    /// top-down rows, and mark it ready. False when every texture is in use.
    pub fn copy(&self, gl: &glow::Context, source: u32) -> bool {
        let free = {
            let slots = lock(&self.slots);
            (0..self.targets.len()).find(|i| slots.ready != Some(*i) && !slots.busy.contains(i))
        };
        let Some(slot) = free else {
            return false;
        };
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        unsafe {
            gl.bind_framebuffer(
                glow::READ_FRAMEBUFFER,
                std::num::NonZeroU32::new(source).map(glow::NativeFramebuffer),
            );
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.targets[slot].1));
            gl.blit_framebuffer(
                0,
                0,
                w,
                h,
                0,
                h,
                w,
                0,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            // The renderer reads the texture on another API; finish writing first.
            gl.finish();
        }
        lock(&self.slots).ready = Some(slot);
        true
    }

    pub fn delete(&mut self, gl: &glow::Context) {
        for (texture, fbo) in self.targets.drain(..) {
            unsafe {
                gl.delete_framebuffer(fbo);
                gl.delete_texture(texture);
            }
        }
    }
}

/// Export `texture` as a single-plane RGBA dma-buf.
fn export(
    display: egl::Display,
    context: egl::Context,
    texture: glow::Texture,
) -> Result<Dmabuf, String> {
    let api = &egl::API;
    let proc = |name| {
        api.get_proc_address(name)
            .ok_or(format!("{name} is unavailable"))
    };
    let query: ExportQuery = unsafe { std::mem::transmute(proc("eglExportDMABUFImageQueryMESA")?) };
    let export: Export = unsafe { std::mem::transmute(proc("eglExportDMABUFImageMESA")?) };
    let image = api
        .create_image(
            display,
            context,
            egl::GL_TEXTURE_2D as egl::Enum,
            unsafe { egl::ClientBuffer::from_ptr(texture.0.get() as usize as *mut c_void) },
            &[egl::ATTRIB_NONE],
        )
        .map_err(|e| format!("wrapping a texture: {e}"))?;
    let (mut fourcc, mut planes, mut modifier) = (0, 0, 0u64);
    let (mut fd, mut stride, mut offset) = (-1, 0, 0);
    let ok = unsafe {
        query(
            display.as_ptr(),
            image.as_ptr(),
            &mut fourcc,
            &mut planes,
            &mut modifier,
        ) != 0
            && planes == 1
            && export(
                display.as_ptr(),
                image.as_ptr(),
                &mut fd,
                &mut stride,
                &mut offset,
            ) != 0
    };
    let _ = api.destroy_image(display, image);
    if !ok || fd < 0 {
        return Err(format!("exporting a texture failed ({planes} planes)"));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if fourcc != FOURCC_ABGR8888 {
        return Err(format!(
            "texture exported as {:?}, not AB24",
            fourcc.to_le_bytes().map(char::from)
        ));
    }
    Ok(Dmabuf {
        fd,
        modifier,
        stride: stride as u32,
        offset: offset as u32,
    })
}
