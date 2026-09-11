#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    lawn_orbit::run()
}

// Browser builds use the library's explicit JavaScript entry point.
#[cfg(target_arch = "wasm32")]
fn main() {}
