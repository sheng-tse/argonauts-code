//! The sandbox launcher alone. The app's own binary carries the same launcher; this one lets the
//! tests of this crate run it without building the app.

fn main() {
    let code = argonauts_conductor::sandbox::launcher::launcher_main().unwrap_or_else(|| {
        eprintln!("argonauts-sandbox: start it the way the sandbox does");
        2
    });
    std::process::exit(code);
}
