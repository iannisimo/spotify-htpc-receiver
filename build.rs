fn main() {
    // Debug builds carry element ids so the UI tests can find elements; release builds do not.
    let debug_build = std::env::var("PROFILE").as_deref() == Ok("debug");
    let config = slint_build::CompilerConfiguration::new().with_debug_info(debug_build);
    slint_build::compile_with_config("ui/app.slint", config).expect("ui/app.slint compiles");
}
