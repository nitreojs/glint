use std::sync::OnceLock;

static SINK: OnceLock<fn(&str)> = OnceLock::new();

pub fn set_sink(sink: fn(&str)) {
    let _ = SINK.set(sink);
}

pub fn log(msg: &str) {
    if let Some(sink) = SINK.get() {
        sink(msg);
    }
}
