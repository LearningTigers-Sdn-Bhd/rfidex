//! RfiDex core: device-neutral logic shared by the desktop app and tools.

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod client;
pub mod codec;
pub mod contract;
pub mod device;
pub mod health;
pub mod station;
pub mod store;
pub mod sync;
pub mod tag;

#[cfg(test)]
mod tests {
    #[test]
    fn app_version_is_set() {
        assert!(!super::APP_VERSION.is_empty());
    }
}
