// OS-level ambient signals for the Pixy sprite: battery state (lowpower) and
// how long since the user last touched keyboard/mouse anywhere on the
// machine (sleeping) — deliberately OS-wide via GetLastInputInfo rather than
// tracking clicks inside this app's own windows, since "the user stepped
// away" is true regardless of which window last had focus.
use serde::Serialize;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::GetLastInputInfo;
use windows::Win32::UI::Input::KeyboardAndMouse::LASTINPUTINFO;

#[derive(Serialize, Clone)]
pub struct PowerStatus {
    // 255 means "unknown" (some desktops/VMs report no battery at all) —
    // passed through as-is rather than coerced to 0/100, so the frontend can
    // treat "unknown" as "don't show lowpower" instead of misreading it as
    // an empty or full battery.
    percent: u8,
    charging: bool,
    has_battery: bool,
}

#[tauri::command]
pub fn get_power_status() -> Result<PowerStatus, String> {
    unsafe {
        let mut status = SYSTEM_POWER_STATUS::default();
        GetSystemPowerStatus(&mut status).map_err(|e| format!("GetSystemPowerStatus failed: {e}"))?;
        Ok(PowerStatus {
            percent: status.BatteryLifePercent,
            // ACLineStatus: 1 = online/charging, 0 = offline, 255 = unknown
            charging: status.ACLineStatus == 1,
            has_battery: status.BatteryFlag != 128 && status.BatteryLifePercent != 255,
        })
    }
}

// Seconds since the last keyboard/mouse input anywhere on the desktop —
// GetLastInputInfo returns a tick-count timestamp, so this is just "now -
// that timestamp" in GetTickCount's own units (ms since boot, wraps at
// ~49.7 days — irrelevant here since we only ever compare it against the
// current tick count taken in the same call).
#[tauri::command]
pub fn get_idle_seconds() -> Result<u64, String> {
    unsafe {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            ..Default::default()
        };
        if !GetLastInputInfo(&mut info).as_bool() {
            return Err("GetLastInputInfo failed".to_string());
        }
        let now = GetTickCount();
        Ok(now.saturating_sub(info.dwTime) as u64 / 1000)
    }
}
