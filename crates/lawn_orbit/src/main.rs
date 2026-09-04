mod app;
mod input_adapter;
mod profile_store;

use anyhow::Result;
use winit::event_loop::{ControlFlow, EventLoop};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lawn_orbit=info,lawn_render=info,wgpu=warn".into()),
        )
        .init();
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = app::LawnOrbitApp::new()?;
    event_loop.run_app(&mut app)?;
    Ok(())
}
