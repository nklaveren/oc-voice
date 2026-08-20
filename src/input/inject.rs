use crate::process::CommandRunner;
use tracing::debug;

pub fn type_text(runner: &dyn CommandRunner, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Ok(found) = runner.output("which", &["wtype"]) {
        if found.status.success() {
            let result = runner.output("wtype", &[text]);
            match result {
                Ok(o) if o.status.success() => {
                    debug!(text = %text, "injected via wtype");
                }
                Ok(o) => {
                    debug!(stderr = %String::from_utf8_lossy(&o.stderr), "wtype failed");
                }
                Err(e) => {
                    debug!(error = %e, "wtype exec failed");
                }
            }
            return;
        }
    }
    if let Ok(found) = runner.output("which", &["xdotool"]) {
        if found.status.success() {
            let result = runner.output("xdotool", &["type", "--clearmodifiers", text]);
            match result {
                Ok(o) if o.status.success() => {
                    debug!(text = %text, "injected via xdotool");
                }
                Ok(o) => {
                    debug!(stderr = %String::from_utf8_lossy(&o.stderr), "xdotool failed");
                }
                Err(e) => {
                    debug!(error = %e, "xdotool exec failed");
                }
            }
            return;
        }
    }
    debug!("no text injection tool found (wtype / xdotool)");
}

pub fn type_key(runner: &dyn CommandRunner, key: &str) {
    if let Ok(found) = runner.output("which", &["wtype"]) {
        if found.status.success() {
            let result = runner.output("wtype", &["-k", key]);
            if let Ok(o) = result {
                if o.status.success() {
                    debug!(key = %key, "key press via wtype");
                } else {
                    debug!(stderr = %String::from_utf8_lossy(&o.stderr), "wtype key failed");
                }
            }
            return;
        }
    }
    if let Ok(found) = runner.output("which", &["xdotool"]) {
        if found.status.success() {
            let result = runner.output("xdotool", &["key", key]);
            if let Ok(o) = result {
                if o.status.success() {
                    debug!(key = %key, "key press via xdotool");
                } else {
                    debug!(stderr = %String::from_utf8_lossy(&o.stderr), "xdotool key failed");
                }
            }
        }
    }
}

pub fn type_shift_return(runner: &dyn CommandRunner) {
    if let Ok(found) = runner.output("which", &["wtype"]) {
        if found.status.success() {
            let result = runner.output("wtype", &["-M", "shift", "-k", "Return"]);
            if let Ok(o) = result {
                if o.status.success() {
                    debug!("Shift+Return via wtype");
                } else {
                    debug!(stderr = %String::from_utf8_lossy(&o.stderr), "wtype shift+return failed");
                }
            }
            return;
        }
    }
    if let Ok(found) = runner.output("which", &["xdotool"]) {
        if found.status.success() {
            let result = runner.output("xdotool", &["key", "Shift+Return"]);
            if let Ok(o) = result {
                if o.status.success() {
                    debug!("Shift+Return via xdotool");
                }
            }
        }
    }
}
