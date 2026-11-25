//! Embeds the application icon into the Windows executable (no-op elsewhere).

fn main() {
    println!("cargo:rerun-if-changed=assets/app-icons/windows/caelum-rs.ico");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("assets/app-icons/windows/caelum-rs.ico");
            if let Err(e) = res.compile() {
                eprintln!("build.rs: cannot embed Windows icon: {e}");
                std::process::exit(1);
            }
        }
    }
}
