// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // WebKitGTK's GPU (dmabuf) buffers break the transparent window on some
    // Wayland setups (NVIDIA): a protocol error at launch, or only the changed
    // regions get painted. Shared-memory buffers fix both. Set it to 0 to opt out.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DMABUF_RENDERER_FORCE_SHM").is_none() {
        std::env::set_var("WEBKIT_DMABUF_RENDERER_FORCE_SHM", "1");
    }
    craft_status_app_lib::run()
}
