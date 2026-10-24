//! Where Servo paints: an offscreen OpenGL surface on the GPU (surfman's
//! hardware adapter, which needs no window), read back after each frame.
//! [`servo::SoftwareRenderingContext`] is the same on Mesa's software
//! renderer, used when there is no GPU context.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use dpi::PhysicalSize;
use euclid::default::Size2D as UntypedSize2D;
use glow::HasContext;
use servo::{DeviceIntRect, RefreshDriver, RenderingContext, RgbaImage, SoftwareRenderingContext};
use surfman::chains::{PreserveBuffer, SwapChain};
use surfman::{
    Connection, Context, ContextAttributeFlags, ContextAttributes, Device, Error, GLApi, Surface,
    SurfaceAccess, SurfaceInfo, SurfaceTexture, SurfaceType,
};

/// When pages start their frames (animations, `requestAnimationFrame`):
/// whenever [`Pacer::tick`] is called, which the renderer does once per
/// frame it draws, so pages follow its frame rate and stop while it's
/// paused or hidden.
#[derive(Default)]
pub struct Pacer {
    waiting: Mutex<Vec<Box<dyn Fn() + Send>>>,
}

impl Pacer {
    pub fn tick(&self) {
        let waiting = std::mem::take(&mut *self.waiting.lock().unwrap());
        for start in waiting {
            start();
        }
    }
}

struct Driver(Arc<Pacer>);

impl RefreshDriver for Driver {
    fn observe_next_frame(&self, start: Box<dyn Fn() + Send + 'static>) {
        self.0.waiting.lock().unwrap().push(start);
    }
}

/// A page's rendering context: the GPU one, or the software fallback.
pub struct PageContext {
    inner: Inner,
    driver: Rc<Driver>,
}

enum Inner {
    Gpu(GpuContext),
    Software(SoftwareRenderingContext),
}

impl PageContext {
    /// A context `size` pixels big, on the GPU unless `WPE_WEB=cpu` or
    /// that fails, whose frames start when `pacer` ticks.
    pub fn new(size: PhysicalSize<u32>, pacer: Arc<Pacer>) -> Result<Self, Error> {
        let driver = Rc::new(Driver(pacer));
        if std::env::var("WPE_WEB").as_deref() != Ok("cpu") {
            match GpuContext::new(size) {
                Ok(context) => {
                    return Ok(Self {
                        inner: Inner::Gpu(context),
                        driver,
                    });
                }
                Err(e) => {
                    tracing::warn!("no GPU context for web pages, rendering in software: {e:?}")
                }
            }
        }
        Ok(Self {
            inner: Inner::Software(SoftwareRenderingContext::new(size)?),
            driver,
        })
    }

    fn inner(&self) -> &dyn RenderingContext {
        match &self.inner {
            Inner::Gpu(c) => c,
            Inner::Software(c) => c,
        }
    }

    /// The framebuffer Servo paints into, when it is on the GPU.
    pub fn gpu_framebuffer(&self) -> Option<u32> {
        match &self.inner {
            Inner::Gpu(c) => Some(c.framebuffer()),
            Inner::Software(_) => None,
        }
    }

    /// The last painted frame as top-down RGBA8 rows.
    pub fn read_frame(&self) -> Option<RgbaImage> {
        let size = self.size();
        let rect =
            DeviceIntRect::from_size(euclid::Size2D::new(size.width as i32, size.height as i32));
        self.inner().read_to_image(rect)
    }
}

impl RenderingContext for PageContext {
    fn prepare_for_rendering(&self) {
        self.inner().prepare_for_rendering()
    }
    fn read_to_image(&self, rect: DeviceIntRect) -> Option<RgbaImage> {
        self.inner().read_to_image(rect)
    }
    fn size(&self) -> PhysicalSize<u32> {
        self.inner().size()
    }
    fn resize(&self, size: PhysicalSize<u32>) {
        self.inner().resize(size)
    }
    fn present(&self) {
        self.inner().present()
    }
    fn make_current(&self) -> Result<(), Error> {
        self.inner().make_current()
    }
    fn gleam_gl_api(&self) -> Rc<dyn gleam::gl::Gl> {
        self.inner().gleam_gl_api()
    }
    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.inner().glow_gl_api()
    }
    fn create_texture(
        &self,
        surface: Surface,
    ) -> Option<(SurfaceTexture, u32, UntypedSize2D<i32>)> {
        self.inner().create_texture(surface)
    }
    fn destroy_texture(&self, texture: SurfaceTexture) -> Option<Surface> {
        self.inner().destroy_texture(texture)
    }
    fn connection(&self) -> Option<Connection> {
        self.inner().connection()
    }
    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        Some(self.driver.clone())
    }
}

/// [`SoftwareRenderingContext`] on surfman's hardware adapter.
pub struct GpuContext {
    gleam: Rc<dyn gleam::gl::Gl>,
    glow: Arc<glow::Context>,
    device: RefCell<Device>,
    context: RefCell<Context>,
    swap_chain: SwapChain<Device>,
    size: Cell<PhysicalSize<u32>>,
}

impl GpuContext {
    pub fn new(size: PhysicalSize<u32>) -> Result<Self, Error> {
        if size.width == 0 || size.height == 0 {
            return Err(Error::Failed);
        }
        let connection = Connection::new()?;
        let adapter = connection.create_hardware_adapter()?;
        let device = connection.create_device(&adapter)?;
        let flags = ContextAttributeFlags::ALPHA
            | ContextAttributeFlags::DEPTH
            | ContextAttributeFlags::STENCIL;
        let gl_api = connection.gl_api();
        let version = match gl_api {
            GLApi::GLES => surfman::GLVersion { major: 3, minor: 0 },
            GLApi::GL => surfman::GLVersion { major: 3, minor: 2 },
        };
        let descriptor = device.create_context_descriptor(&ContextAttributes { flags, version })?;
        let mut context = device.create_context(&descriptor, None)?;
        let gleam = unsafe {
            match gl_api {
                GLApi::GL => gleam::gl::GlFns::load_with(|f| device.get_proc_address(&context, f)),
                GLApi::GLES => {
                    gleam::gl::GlesFns::load_with(|f| device.get_proc_address(&context, f))
                }
            }
        };
        let glow = unsafe {
            glow::Context::from_loader_function(|f| device.get_proc_address(&context, f))
        };
        let surface_size = euclid::default::Size2D::new(size.width as i32, size.height as i32);
        let surface = device.create_surface(
            &context,
            SurfaceAccess::GPUOnly,
            SurfaceType::Generic { size: surface_size },
        )?;
        if let Err((e, mut surface)) = device.bind_surface_to_context(&mut context, surface) {
            let _ = device.destroy_surface(&mut context, &mut surface);
            let _ = device.destroy_context(&mut context);
            return Err(e);
        }
        device.make_context_current(&context)?;
        let swap_chain = SwapChain::create_attached(&device, &mut context, SurfaceAccess::GPUOnly)?;
        tracing::debug!(
            "web pages render on {} ({})",
            unsafe { glow.get_parameter_string(glow::RENDERER) },
            unsafe { glow.get_parameter_string(glow::VERSION) },
        );
        Ok(Self {
            gleam,
            glow: Arc::new(glow),
            device: RefCell::new(device),
            context: RefCell::new(context),
            swap_chain,
            size: Cell::new(size),
        })
    }

    pub(crate) fn framebuffer(&self) -> u32 {
        let device = self.device.borrow();
        let context = self.context.borrow();
        device
            .context_surface_info(&context)
            .ok()
            .flatten()
            .and_then(|info| info.framebuffer_object)
            .map_or(0, |fbo| fbo.0.get())
    }
}

impl Drop for GpuContext {
    fn drop(&mut self) {
        let device = &mut self.device.borrow_mut();
        let context = &mut self.context.borrow_mut();
        let _ = self.swap_chain.destroy(device, context);
        let _ = device.destroy_context(context);
    }
}

impl RenderingContext for GpuContext {
    fn prepare_for_rendering(&self) {
        self.gleam
            .bind_framebuffer(gleam::gl::FRAMEBUFFER, self.framebuffer());
    }

    fn read_to_image(&self, rect: DeviceIntRect) -> Option<RgbaImage> {
        let gl = &self.gleam;
        gl.bind_framebuffer(gleam::gl::FRAMEBUFFER, self.framebuffer());
        gl.bind_vertex_array(0);
        let (w, h) = (rect.width() as usize, rect.height() as usize);
        let pixels = gl.read_pixels(
            rect.min.x,
            rect.min.y,
            rect.width(),
            rect.height(),
            gleam::gl::RGBA,
            gleam::gl::UNSIGNED_BYTE,
        );
        // GL rows run bottom-up.
        let stride = w * 4;
        let mut flipped = Vec::with_capacity(pixels.len());
        for row in pixels.chunks_exact(stride).rev() {
            flipped.extend_from_slice(row);
        }
        RgbaImage::from_raw(w as u32, h as u32, flipped)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.size.get()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        if self.size.get() == size || size.width == 0 || size.height == 0 {
            return;
        }
        self.size.set(size);
        let device = &mut self.device.borrow_mut();
        let context = &mut self.context.borrow_mut();
        let size = euclid::default::Size2D::new(size.width as i32, size.height as i32);
        let _ = self.swap_chain.resize(device, context, size);
    }

    fn present(&self) {
        let device = &mut self.device.borrow_mut();
        let context = &mut self.context.borrow_mut();
        let _ = self
            .swap_chain
            .swap_buffers(device, context, PreserveBuffer::No);
    }

    fn make_current(&self) -> Result<(), Error> {
        self.device
            .borrow()
            .make_context_current(&self.context.borrow())
    }

    fn gleam_gl_api(&self) -> Rc<dyn gleam::gl::Gl> {
        self.gleam.clone()
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.glow.clone()
    }

    fn create_texture(
        &self,
        surface: Surface,
    ) -> Option<(SurfaceTexture, u32, UntypedSize2D<i32>)> {
        let device = &self.device.borrow();
        let context = &mut self.context.borrow_mut();
        let SurfaceInfo { size, .. } = device.surface_info(&surface);
        let texture = device.create_surface_texture(context, surface).ok()?;
        let id = device
            .surface_texture_object(&texture)
            .map_or(0, |t| t.0.get());
        Some((texture, id, size))
    }

    fn destroy_texture(&self, texture: SurfaceTexture) -> Option<Surface> {
        let device = &self.device.borrow();
        let context = &mut self.context.borrow_mut();
        device
            .destroy_surface_texture(context, texture)
            .map_err(|(e, _)| e)
            .ok()
    }

    fn connection(&self) -> Option<Connection> {
        Some(self.device.borrow().connection())
    }
}
