//! This crate defines the FFI ABI used for the libprofcast library.

use std::{
    ffi::{CString, c_char},
    sync::OnceLock,
};

/// Returns a null-terminated string showing the program name and version,
/// eg. "profcast 0.1.0".
///
/// In the case of an internal library error for this, it will
/// return "profcast <unknown>".
#[unsafe(no_mangle)]
pub extern "C" fn profcast_version() -> *const c_char {
    static VERSION: OnceLock<Option<CString>> = OnceLock::new();
    VERSION
        .get_or_init(|| CString::new(profcast_core::VERSION_STRING).ok())
        .as_ref()
        .map_or_else(|| c"profcast <unknown>".as_ptr(), |cstr| cstr.as_ptr())
}

#[cfg(test)]
mod test {
    use std::ffi::CStr;

    use super::*;
    use anyhow::Result;

    #[test]
    fn version_is_nonnull() {
        assert_ne!(profcast_version(), std::ptr::null_mut());
    }

    #[test]
    fn version_is_correct() -> Result<()> {
        // SAFETY: needed to consume c pointer for test
        let ffi_cstr = unsafe { CStr::from_ptr(profcast_version()) }.to_str()?;
        assert_eq!(ffi_cstr, profcast_core::VERSION_STRING);
        Ok(())
    }
}
