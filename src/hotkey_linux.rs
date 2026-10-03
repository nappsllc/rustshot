//! Linux global hotkey stub; real X11/Rust grab comes later.

use super::*;

pub fn hotkey_thread(specs: [(i32, String, HotEvent); 2], tx: mpsc::Sender<HotEvent>) {
    // TODO: real key grab; parsing keeps `key_vk` (and the shared `key`
    // constants) reachable for dead-code analysis.
    let _ = tx;
    for (_, spec, _) in &specs {
        let _ = parse_hotkey(spec);
    }
}
