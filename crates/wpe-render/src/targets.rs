//! Render targets: `_rt_default` plus the ping-pong buffers, effect FBOs and
//! framebuffer copies a scene declares.

use crate::Gpu;

/// Index of a render target in the renderer's target list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetId(pub usize);

impl TargetId {
    /// `_rt_default`, the scene's main colour buffer.
    pub const DEFAULT: Self = Self(0);
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TargetSize {
    Fixed(u32, u32),
    /// The output size divided by `divisor`.
    Screen {
        divisor: f32,
    },
}

#[derive(Debug, Clone)]
pub struct TargetDesc {
    pub name: String,
    pub size: TargetSize,
    pub format: wgpu::TextureFormat,
}

pub(crate) struct TargetTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: (u32, u32),
}

pub(crate) struct Targets {
    descs: Vec<TargetDesc>,
    textures: Vec<Option<TargetTexture>>,
    /// Bumped whenever any texture is (re)allocated, invalidating bind groups.
    pub generation: u64,
    screen: (u32, u32),
    pub sampler: wgpu::Sampler,
}

/// Format of `_rt_default` and ping-pong buffers. Wallpaper Engine composites
/// in non-linear 8-bit space, so no sRGB conversion happens anywhere.
pub const DEFAULT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl Targets {
    pub fn new(gpu: &Gpu) -> Self {
        Self {
            descs: vec![TargetDesc {
                name: "_rt_default".to_owned(),
                size: TargetSize::Screen { divisor: 1.0 },
                format: DEFAULT_FORMAT,
            }],
            textures: vec![None],
            generation: 0,
            screen: (0, 0),
            sampler: gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("render target"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    /// Replace every target except `_rt_default`; ids start at 1 in `descs` order.
    pub fn set(&mut self, descs: Vec<TargetDesc>) {
        self.descs.truncate(1);
        self.textures.truncate(1);
        self.textures.extend(descs.iter().map(|_| None));
        self.descs.extend(descs);
        self.generation += 1;
    }

    fn resolve(&self, size: TargetSize) -> (u32, u32) {
        match size {
            TargetSize::Fixed(w, h) => (w.max(1), h.max(1)),
            TargetSize::Screen { divisor } => {
                let d = divisor.max(1.0);
                (
                    ((self.screen.0 as f32 / d) as u32).max(1),
                    ((self.screen.1 as f32 / d) as u32).max(1),
                )
            }
        }
    }

    /// Allocate missing targets and resize screen-relative ones to `screen`.
    pub fn prepare(&mut self, gpu: &Gpu, screen: (u32, u32)) {
        self.screen = screen;
        for i in 0..self.descs.len() {
            let size = self.resolve(self.descs[i].size);
            if self.textures[i].as_ref().is_some_and(|t| t.size == size) {
                continue;
            }
            let desc = &self.descs[i];
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&desc.name),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: desc.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            self.textures[i] = Some(TargetTexture {
                texture,
                view,
                size,
            });
            self.generation += 1;
        }
    }

    pub fn get(&self, id: TargetId) -> &TargetTexture {
        self.textures[id.0]
            .as_ref()
            .expect("targets are prepared before use")
    }

    pub fn len(&self) -> usize {
        self.descs.len()
    }

    pub fn format(&self, id: TargetId) -> wgpu::TextureFormat {
        self.descs[id.0].format
    }
}

/// Depth buffers of the targets 3D draws render into, one per target and
/// sized like it, kept across frames. Generic over the buffer so the
/// lifetime rules can be tested without a GPU.
pub(crate) struct DepthBuffers<T = wgpu::TextureView> {
    buffers: Vec<Option<((u32, u32), T)>>,
    /// Targets whose depth was cleared this frame.
    cleared: Vec<bool>,
}

impl<T> Default for DepthBuffers<T> {
    fn default() -> Self {
        Self {
            buffers: Vec::new(),
            cleared: Vec::new(),
        }
    }
}

impl<T> DepthBuffers<T> {
    /// Start a frame over `targets` targets; buffers of removed targets are freed.
    pub fn begin_frame(&mut self, targets: usize) {
        self.buffers.truncate(targets);
        self.buffers.resize_with(targets, || None);
        self.cleared.clear();
        self.cleared.resize(targets, false);
    }

    /// The depth buffer of `target` at `size`, made by `make` when missing
    /// or sized differently, and whether this is its first use this frame
    /// (a pass must clear it to the far plane then, and keep it after).
    ///
    /// Returning a `wgpu::LoadOp<f32>` from here instead got its clear value
    /// corrupted in release builds (rustc 1.96 packed it into a register
    /// together with stale stack bits), so callers build the load op.
    pub fn get(
        &mut self,
        target: TargetId,
        size: (u32, u32),
        make: impl FnOnce((u32, u32)) -> T,
    ) -> (&T, bool) {
        if self.buffers.len() <= target.0 {
            self.begin_frame(target.0 + 1);
        }
        let slot = &mut self.buffers[target.0];
        if slot.as_ref().is_none_or(|(s, _)| *s != size) {
            *slot = Some((size, make(size)));
        }
        let first = !std::mem::replace(&mut self.cleared[target.0], true);
        let (_, buffer) = slot.as_ref().expect("allocated above");
        (buffer, first)
    }
}

/// A depth buffer for a target of `size`.
pub(crate) fn depth_view(gpu: &Gpu, size: (u32, u32)) -> wgpu::TextureView {
    gpu.device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::material::DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_buffers_clear_once_and_follow_target_size() {
        let mut made = 0;
        let mut depth = DepthBuffers::<u32>::default();
        let mut make = |_| {
            made += 1;
            made
        };
        depth.begin_frame(2);
        let t = TargetId(1);
        let (&b, first) = depth.get(t, (4, 4), &mut make);
        assert_eq!((b, first), (1, true));
        let (&b, first) = depth.get(t, (4, 4), &mut make);
        assert_eq!((b, first), (1, false));

        // Kept across frames, cleared again in each.
        depth.begin_frame(2);
        let (&b, first) = depth.get(t, (4, 4), &mut make);
        assert_eq!((b, first), (1, true));

        // A resized target gets a new buffer.
        depth.begin_frame(2);
        let (&b, _) = depth.get(t, (8, 4), &mut make);
        assert_eq!(b, 2);

        // Dropped with its target; a target in its place starts fresh.
        depth.begin_frame(1);
        depth.begin_frame(2);
        let (&b, first) = depth.get(t, (8, 4), &mut make);
        assert_eq!((b, first), (3, true));
    }
}
