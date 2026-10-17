fn main() {
    // Leaves the interpreter's symbols undefined at link time on macOS and Windows, as an extension module must.
    pyo3_build_config::add_extension_module_link_args();
}
