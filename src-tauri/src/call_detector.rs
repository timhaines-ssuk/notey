//! Detect when a call is in progress (Discord, Teams) so capture can auto-start.
//!
//! v1 approach: poll the process list once a second; if Teams.exe or Discord.exe
//! is running AND a microphone or output device is *active* (something is
//! reading or writing to it), treat that as "call in progress". A more reliable
//! detector would use IAudioSessionEnumerator from WASAPI to check whether the
//! target process has an active audio session — that's the next step once we
//! have something to test against.

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessesToUpdate, System};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CallApp {
    Discord,
    Teams,
    None,
}

pub fn detect_call_app(sys: &mut System) -> CallApp {
    sys.refresh_processes(ProcessesToUpdate::All);
    let names: Vec<String> = sys
        .processes()
        .values()
        .map(|p| p.name().to_string_lossy().to_lowercase())
        .collect();
    if names.iter().any(|n| n.starts_with("ms-teams") || n == "teams.exe") {
        CallApp::Teams
    } else if names.iter().any(|n| n == "discord.exe") {
        CallApp::Discord
    } else {
        CallApp::None
    }
}
