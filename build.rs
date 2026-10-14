#[cfg(windows)]
fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/logo.ico");
    if let Err(e) = res.compile() {
        eprintln!("cargo:warning=Failed to compile Windows icon resource: {}", e);
    }
}

#[cfg(not(windows))]
fn main() {}
