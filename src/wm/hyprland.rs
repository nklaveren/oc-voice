use crate::input::inject::{type_key, type_text};
use crate::process::CommandRunner;
use crate::wm::backend::WmBackend as _;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// Focus a window by its hyprctl address, then type the text into it.
pub fn focus_address_and_type(runner: &Arc<dyn CommandRunner>, address: &str, text: &str) {
    if !address.is_empty() {
        // Through the seam like every other window command: this is the last
        // one that spelled hyprctl out by hand, and a port would have found
        // it only by grepping.
        crate::wm::backend::Hyprctl::new(runner.clone()).dispatch(
            &crate::wm::backend::WmAction::FocusWindow {
                address: address.to_string(),
            },
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    type_text(&**runner, text);
    type_key(&**runner, "Return");
    info!(address, "sent text to target window");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::FakeRunner;

    #[test]
    fn focuses_resolved_window_before_typing() {
        let fake = Arc::new(FakeRunner::new(
            br#"[{"class":"code","title":"main.rs","address":"0x123"}]"#.to_vec(),
        ));
        let runner: Arc<dyn CommandRunner> = fake.clone();
        let windows = crate::wm::target::live_windows(&runner);
        let categories = std::collections::HashMap::new();
        let resolved = crate::wm::target::resolve("code", &categories, &windows, 0.82)
            .expect("code window resolves");
        focus_address_and_type(&runner, &resolved.address, "hello");
        let calls = fake.calls();
        assert!(calls
            .iter()
            .any(|(p, a)| p == "hyprctl" && a == &["dispatch", "focuswindow", "address:0x123"]));
    }
}
