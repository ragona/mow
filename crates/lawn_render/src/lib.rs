//! `wgpu` renderer for Lawn Orbit.

mod bloom;
mod gpu_profiler;
mod grass_roots;
mod interaction;
mod mesh;
mod particles;
mod renderer;
mod surface;
mod vehicle_presentation;

pub use renderer::{
    FrameAcquireError, FrameStats, RenderCapabilities, RenderFrame, RenderTier, Renderer,
};

#[cfg(test)]
mod tests {
    #[test]
    fn all_shipping_wgsl_modules_parse_and_validate() {
        for (name, source) in [
            ("terrain", include_str!("shaders/terrain.wgsl")),
            ("shadow", include_str!("shaders/shadow.wgsl")),
            ("grass", include_str!("shaders/grass.wgsl")),
            ("interaction", include_str!("shaders/interaction.wgsl")),
            ("particles", include_str!("shaders/particles.wgsl")),
            ("composite", include_str!("shaders/composite.wgsl")),
            ("bloom", include_str!("shaders/bloom.wgsl")),
        ] {
            let module = naga::front::wgsl::parse_str(source)
                .unwrap_or_else(|error| panic!("{name}.wgsl did not parse: {error}"));
            let mut validator = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            );
            validator
                .validate(&module)
                .unwrap_or_else(|error| panic!("{name}.wgsl did not validate: {error}"));
        }
    }
}
