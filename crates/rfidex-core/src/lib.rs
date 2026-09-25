//! RfiDex core: device-neutral logic shared by the desktop app and tools.

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod codec;
pub mod contract;
pub mod store;
pub mod tag;

#[cfg(test)]
mod tests {
    #[test]
    fn app_version_is_set() {
        assert_eq!(super::APP_VERSION, "0.1.0");
    }
}
