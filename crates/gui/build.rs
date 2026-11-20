use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(QmlModule::new("com.pdf2word.app").qml_files(["qml/Main.qml"]))
        .qt_module("Quick")
        .qt_module("QuickControls2")
        .files(["src/converter.rs"])
        .build();

    // Application icon and version information in the Windows executable.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=../../packaging/windows/app.rc");
        if let Err(e) = embed_resource::compile("../../packaging/windows/app.rc", embed_resource::NONE).manifest_optional() {
            println!("cargo:warning=could not embed the application icon: {e}");
        }
    }
}
