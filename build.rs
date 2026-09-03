//! Embeds the application icon into the Windows executable so Explorer,
//! the taskbar, and MSI shortcuts display it instead of a generic white icon.

fn main() {
    // CARGO_CFG_WINDOWS mirrors #[cfg(windows)]; keeps non-Windows builds a no-op.
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let icon = "icons/clippy-converter_icon.ico";
        println!("cargo:rerun-if-changed={icon}");
        // Panicking on failure is standard for build scripts: the build must not
        // silently ship an exe without its icon.
        if let Err(e) = winresource::WindowsResource::new().set_icon(icon).compile() {
            panic!("failed to embed Windows icon resource: {e}");
        }
    }
}
