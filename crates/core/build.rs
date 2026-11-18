//! Links libtesseract when the `ocr` feature is enabled.
//!
//! Resolution order:
//! 1. `TESSERACT_LIB_DIR` (+ optional `TESSERACT_LIB_NAME`, default `tesseract`)
//! 2. pkg-config (`tesseract.pc`)
//! 3. a plain `-ltesseract` and hope the linker finds it

fn main() {
    println!("cargo:rerun-if-env-changed=TESSERACT_LIB_DIR");
    println!("cargo:rerun-if-env-changed=TESSERACT_LIB_NAME");
    #[cfg(feature = "ocr")]
    link_tesseract();
}

#[cfg(feature = "ocr")]
fn link_tesseract() {
    if let Ok(dir) = std::env::var("TESSERACT_LIB_DIR") {
        let name = std::env::var("TESSERACT_LIB_NAME").unwrap_or_else(|_| "tesseract".into());
        println!("cargo:rustc-link-search=native={dir}");
        println!("cargo:rustc-link-lib={name}");
        return;
    }
    match pkg_config::Config::new().atleast_version("4.0").probe("tesseract") {
        Ok(_) => {}
        Err(e) => {
            println!("cargo:warning=pkg-config could not find tesseract ({e}); falling back to -ltesseract");
            println!("cargo:rustc-link-lib=tesseract");
        }
    }
}
