//! Reflection over compiled SPIR-V via naga.

use std::collections::BTreeMap;

use naga::{AddressSpace, Binding, ImageDimension, ScalarKind, TypeInner};

use crate::glslang::{GLOBALS_BLOCK, Stage};
use crate::rewrite::{SAMPLER_SUFFIX, TEXTURE_SUFFIX};

/// Scalar component type of a uniform or vertex input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    Float,
    Int,
    Uint,
    Bool,
}

/// Shape of a uniform block member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniformType {
    pub scalar: Scalar,
    /// Rows of a vector/matrix column (1 for scalars).
    pub rows: u8,
    /// Matrix columns (1 for scalars and vectors).
    pub columns: u8,
    /// Array length (1 for non-arrays).
    pub count: u32,
    /// Byte distance between consecutive array elements.
    pub array_stride: u32,
}

impl UniformType {
    /// Byte distance between matrix columns (std140 pads them to 16 bytes).
    pub fn column_stride(&self) -> u32 {
        if self.columns > 1 {
            16
        } else {
            4 * self.rows as u32
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniformMember {
    pub offset: u32,
    pub size: u32,
    pub ty: UniformType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureDim {
    D2,
    D3,
    Cube,
    D2Array,
}

/// A combined sampler from the source, split into two bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureBinding {
    pub texture: u32,
    pub sampler: u32,
    pub dim: TextureDim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexInput {
    pub name: String,
    pub location: u32,
    pub scalar: Scalar,
    pub components: u8,
}

/// Resources a compiled program uses. Everything lives in bind group 0.
#[derive(Debug, Clone, Default)]
pub struct Reflection {
    /// Size of the loose-uniform block at binding 0 (0 if unused).
    pub uniform_size: u32,
    pub uniforms: BTreeMap<String, UniformMember>,
    /// Combined sampler name (e.g. `g_Texture0`) → bindings.
    pub textures: BTreeMap<String, TextureBinding>,
    pub vertex_inputs: Vec<VertexInput>,
    /// Stages that read the uniform block or each texture.
    pub uniform_stages: Vec<Stage>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReflectError {
    #[error("naga could not read the SPIR-V: {0}")]
    Parse(#[from] naga::front::spv::Error),
    #[error("naga rejected the module: {0}")]
    Validate(String),
    #[error("uniform {0} has different layouts in different stages")]
    LayoutMismatch(String),
}

pub fn parse_spirv(words: &[u32]) -> Result<naga::Module, ReflectError> {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let opts = naga::front::spv::Options {
        adjust_coordinate_space: false,
        ..Default::default()
    };
    Ok(naga::front::spv::parse_u8_slice(&bytes, &opts)?)
}

pub fn validate(module: &naga::Module) -> Result<naga::valid::ModuleInfo, ReflectError> {
    naga::valid::Validator::new(Default::default(), naga::valid::Capabilities::all())
        .validate(module)
        .map_err(|e| ReflectError::Validate(e.emit_to_string("")))
}

fn scalar(kind: ScalarKind) -> Scalar {
    match kind {
        ScalarKind::Sint => Scalar::Int,
        ScalarKind::Uint => Scalar::Uint,
        ScalarKind::Bool => Scalar::Bool,
        _ => Scalar::Float,
    }
}

fn uniform_type(module: &naga::Module, ty: naga::Handle<naga::Type>) -> Option<UniformType> {
    let base = |scalar, rows, columns| UniformType {
        scalar,
        rows,
        columns,
        count: 1,
        array_stride: 0,
    };
    match module.types[ty].inner {
        TypeInner::Scalar(s) => Some(base(scalar(s.kind), 1, 1)),
        TypeInner::Vector { size, scalar: s } => Some(base(scalar(s.kind), size as u8, 1)),
        TypeInner::Matrix {
            columns,
            rows,
            scalar: s,
        } => Some(base(scalar(s.kind), rows as u8, columns as u8)),
        TypeInner::Array {
            base: elem,
            size,
            stride,
        } => {
            let mut t = uniform_type(module, elem)?;
            t.count = match size {
                naga::ArraySize::Constant(n) => n.get(),
                _ => 1,
            };
            t.array_stride = stride;
            Some(t)
        }
        _ => None,
    }
}

impl Reflection {
    /// Merge the resources of one stage's module.
    pub fn add_stage(&mut self, stage: Stage, module: &naga::Module) -> Result<(), ReflectError> {
        let mut layouter = naga::proc::Layouter::default();
        layouter
            .update(module.to_ctx())
            .expect("layout of a validated module");

        let mut textures = BTreeMap::new();
        let mut samplers = BTreeMap::new();
        for (_, var) in module.global_variables.iter() {
            let Some(binding) = &var.binding else {
                continue;
            };
            let name = var.name.clone().unwrap_or_default();
            match (&module.types[var.ty].inner, var.space) {
                (TypeInner::Struct { members, span }, AddressSpace::Uniform) => {
                    let is_globals = module.types[var.ty].name.as_deref() == Some(GLOBALS_BLOCK)
                        || binding.binding == 0;
                    if !is_globals {
                        continue;
                    }
                    self.uniform_size = self.uniform_size.max(*span);
                    if !self.uniform_stages.contains(&stage) {
                        self.uniform_stages.push(stage);
                    }
                    for m in members {
                        let Some(ty) = uniform_type(module, m.ty) else {
                            continue;
                        };
                        let member = UniformMember {
                            offset: m.offset,
                            size: layouter[m.ty].size,
                            ty,
                        };
                        let key = m.name.clone().unwrap_or_default();
                        match self.uniforms.get(&key) {
                            Some(existing) if *existing != member => {
                                return Err(ReflectError::LayoutMismatch(key));
                            }
                            _ => {
                                self.uniforms.insert(key, member);
                            }
                        }
                    }
                }
                (TypeInner::Image { dim, arrayed, .. }, _) => {
                    let dim = match (dim, arrayed) {
                        (ImageDimension::D3, _) => TextureDim::D3,
                        (ImageDimension::Cube, _) => TextureDim::Cube,
                        (_, true) => TextureDim::D2Array,
                        _ => TextureDim::D2,
                    };
                    let base = name
                        .strip_suffix(TEXTURE_SUFFIX)
                        .unwrap_or(&name)
                        .to_owned();
                    textures.insert(base, (binding.binding, dim));
                }
                (TypeInner::Sampler { .. }, _) => {
                    let base = name
                        .strip_suffix(SAMPLER_SUFFIX)
                        .unwrap_or(&name)
                        .to_owned();
                    samplers.insert(base, binding.binding);
                }
                _ => {}
            }
        }
        for (name, (texture, dim)) in textures {
            let sampler = samplers.get(&name).copied().unwrap_or(texture + 1);
            self.textures.insert(
                name,
                TextureBinding {
                    texture,
                    sampler,
                    dim,
                },
            );
        }

        if stage == Stage::Vertex
            && let Some(ep) = module.entry_points.first()
        {
            for arg in &ep.function.arguments {
                let Some(Binding::Location { location, .. }) = arg.binding else {
                    continue;
                };
                let (scalar_kind, components) = match module.types[arg.ty].inner {
                    TypeInner::Scalar(s) => (s.kind, 1),
                    TypeInner::Vector { size, scalar: s } => (s.kind, size as u8),
                    _ => continue,
                };
                self.vertex_inputs.push(VertexInput {
                    name: arg.name.clone().unwrap_or_default(),
                    location,
                    scalar: scalar(scalar_kind),
                    components,
                });
            }
        }
        Ok(())
    }
}
