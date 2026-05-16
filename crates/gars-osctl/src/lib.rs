//! OS-level controls used by gars tools: ADB shell driver, XOR-encrypted
//! local keychain, and physical input on macOS (via `osascript`) and Linux
//! (via `xdotool`). Windows is not supported.

pub mod adb;
pub mod input;
pub mod keychain;

pub use adb::{AdbDevice, AdbNode, adb_devices, adb_swipe, adb_tap, adb_text, adb_ui};
pub use input::{InputAction, input_act};
pub use keychain::{Keychain, KeychainEntry};
