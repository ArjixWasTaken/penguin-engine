use wpe_assets::tex::{Mip, TexFlags, TexFormat, Texture};

use crate::Gpu;

/// A `.tex` uploaded to the GPU.
pub struct GpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
    /// Size of the uploaded first mip.
    pub width: u32,
    pub height: u32,
    /// Size of the picture inside the (possibly padded) texture.
    pub map_width: u32,
    pub map_height: u32,
}

impl GpuTexture {
    /// UV scale mapping a card onto the picture inside a padded texture.
    pub fn uv_scale(&self) -> [f32; 2] {
        if self.width * self.height > self.map_width * self.map_height {
            [
                (self.map_width as f32 / self.width as f32).min(1.0),
                (self.map_height as f32 / self.height as f32).min(1.0),
            ]
        } else {
            [1.0, 1.0]
        }
    }
}

fn wgpu_format(format: TexFormat) -> wgpu::TextureFormat {
    use wgpu::TextureFormat as F;
    match format {
        TexFormat::Rgba8 | TexFormat::Rgb565 => F::Rgba8Unorm,
        TexFormat::Bc1 => F::Bc1RgbaUnorm,
        TexFormat::Bc2 => F::Bc2RgbaUnorm,
        TexFormat::Bc3 => F::Bc3RgbaUnorm,
        TexFormat::Bc6h => F::Bc6hRgbUfloat,
        TexFormat::Bc7 => F::Bc7RgbaUnorm,
        TexFormat::Rg8 => F::Rg8Unorm,
        TexFormat::R8 => F::R8Unorm,
        TexFormat::Rgba16f => F::Rgba16Float,
        TexFormat::Rg16f => F::Rg16Float,
        TexFormat::R16f => F::R16Float,
        TexFormat::Rgba1010102 => F::Rgb10a2Unorm,
    }
}

fn rgb565_to_rgba8(data: &[u8]) -> Vec<u8> {
    data.chunks_exact(2)
        .flat_map(|c| {
            let v = u16::from_le_bytes([c[0], c[1]]);
            let expand =
                |bits: u16, max: u16| ((bits as u32 * 255 + max as u32 / 2) / max as u32) as u8;
            [
                expand(v >> 11, 31),
                expand((v >> 5) & 63, 63),
                expand(v & 31, 31),
                255,
            ]
        })
        .collect()
}

/// The longest prefix of `mips` that forms a valid wgpu mip chain.
fn valid_mip_chain(format: TexFormat, mips: &[Mip]) -> usize {
    let Some(first) = mips.first() else { return 0 };
    let bd = format.block_dim();
    if format.is_compressed() && (first.width % bd != 0 || first.height % bd != 0) {
        return 0;
    }
    mips.iter()
        .enumerate()
        .take_while(|(level, mip)| {
            let expect = |size: u32| (size >> level).max(1);
            mip.width == expect(first.width)
                && mip.height == expect(first.height)
                && mip.depth == first.depth
                && mip.data.len() >= format.region_bytes(mip.width, mip.height) * mip.depth as usize
        })
        .count()
}

/// Upload the first image slot of `tex`, with as much of its mip chain as is valid.
pub fn upload_texture(gpu: &Gpu, tex: &Texture, label: &str) -> GpuTexture {
    let mips = tex.images.first().map_or(&[][..], |i| &i.mips[..]);
    let mut format = tex.format;
    if format.is_compressed() && !gpu.supports_bc() {
        format = TexFormat::Rgba8;
    }
    let levels = if format == tex.format {
        valid_mip_chain(format, mips)
    } else {
        0
    };

    let fallback;
    let (mips, format) = if levels == 0 {
        tracing::warn!(
            "texture {label}: no uploadable mips ({:?}), using white",
            tex.format
        );
        fallback = [Mip {
            width: 1,
            height: 1,
            depth: 1,
            data: vec![255; 4],
        }];
        (&fallback[..], TexFormat::Rgba8)
    } else {
        (&mips[..levels], format)
    };

    let first = &mips[0];
    let volume = first.depth > 1;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: first.width,
            height: first.height,
            depth_or_array_layers: first.depth,
        },
        mip_level_count: mips.len() as u32,
        sample_count: 1,
        dimension: if volume {
            wgpu::TextureDimension::D3
        } else {
            wgpu::TextureDimension::D2
        },
        format: wgpu_format(format),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let bd = format.block_dim();
    for (level, mip) in mips.iter().enumerate() {
        let converted;
        let data = if format == TexFormat::Rgb565 {
            converted = rgb565_to_rgba8(&mip.data);
            &converted[..]
        } else {
            &mip.data[..]
        };
        let row_bytes = if format == TexFormat::Rgb565 {
            mip.width as usize * 4
        } else {
            format.region_bytes(mip.width, bd)
        };
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes as u32),
                rows_per_image: Some(mip.height.div_ceil(bd)),
            },
            wgpu::Extent3d {
                width: mip.width,
                height: mip.height,
                depth_or_array_layers: mip.depth,
            },
        );
    }

    let filter = if tex.flags.contains(TexFlags::NO_INTERPOLATION) {
        wgpu::FilterMode::Nearest
    } else {
        wgpu::FilterMode::Linear
    };
    let address = if tex.flags.contains(TexFlags::CLAMP_UVS) {
        wgpu::AddressMode::ClampToEdge
    } else {
        wgpu::AddressMode::Repeat
    };
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: address,
        address_mode_v: address,
        address_mode_w: address,
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    GpuTexture {
        texture,
        view,
        sampler,
        width: first.width,
        height: first.height,
        map_width: if levels == 0 { 1 } else { tex.map_width },
        map_height: if levels == 0 { 1 } else { tex.map_height },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mip(w: u32, h: u32, format: TexFormat) -> Mip {
        Mip {
            width: w,
            height: h,
            depth: 1,
            data: vec![0; format.region_bytes(w, h)],
        }
    }

    #[test]
    fn mip_chain_validation() {
        let f = TexFormat::Bc3;
        assert_eq!(
            valid_mip_chain(f, &[mip(8, 4, f), mip(4, 2, f), mip(2, 1, f), mip(1, 1, f)]),
            4
        );
        assert_eq!(valid_mip_chain(f, &[mip(8, 4, f), mip(3, 2, f)]), 1);
        assert_eq!(valid_mip_chain(f, &[mip(6, 4, f)]), 0);
        let r = TexFormat::Rgba8;
        assert_eq!(valid_mip_chain(r, &[mip(6, 3, r), mip(3, 1, r)]), 2);
    }

    #[test]
    fn rgb565_expands() {
        assert_eq!(rgb565_to_rgba8(&0xF800u16.to_le_bytes()), [255, 0, 0, 255]);
        assert_eq!(rgb565_to_rgba8(&0x07E0u16.to_le_bytes()), [0, 255, 0, 255]);
    }
}
