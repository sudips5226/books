//! Qt Quick desktop application for converting PDF files to Word documents.

mod converter;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // A consistent, desktop-like look on every platform unless the user overrides it.
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Fusion");
    }

    let mut app = QGuiApplication::new();
    if let Some(mut app) = app.as_mut() {
        app.as_mut().set_application_name(&QString::from("PDF to Word"));
        app.as_mut().set_organization_name(&QString::from("pdf2word"));
    }

    let mut engine = QQmlApplicationEngine::new();
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/com/pdf2word/app/qml/Main.qml"));
    }

    if let Some(app) = app.as_mut() {
        std::process::exit(app.exec());
    }
}
