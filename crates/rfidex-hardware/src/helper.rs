//! The helper child process.
//!
//! On Windows the child is started with `CreateProcessW` and
//! `STARTF_FORCEOFFFEEDBACK`. `std::process::Command` cannot set that flag,
//! and without it Windows shows the "working in background" cursor for every
//! helper the app starts. Elsewhere this is a plain `std` child.

use std::ffi::OsStr;
use std::io;

#[cfg(not(windows))]
pub struct Helper(std::process::Child);

#[cfg(not(windows))]
impl Helper {
    pub fn spawn(exe: &std::path::Path, args: &[&OsStr]) -> io::Result<Helper> {
        std::process::Command::new(exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .spawn()
            .map(Helper)
    }

    /// True once the process has really exited.
    pub fn exited(&mut self) -> io::Result<bool> {
        self.0.try_wait().map(|status| status.is_some())
    }

    pub fn kill(&mut self) -> io::Result<()> {
        self.0.kill()
    }

    pub fn wait(&mut self) -> io::Result<()> {
        self.0.wait().map(|_| ())
    }
}

#[cfg(windows)]
pub use windows::Helper;

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null_mut;

    const STARTF_FORCEOFFFEEDBACK: u32 = 0x0000_0080;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const WAIT_OBJECT_0: u32 = 0;

    #[repr(C)]
    struct StartupInfoW {
        cb: u32,
        reserved: *mut u16,
        desktop: *mut u16,
        title: *mut u16,
        x: u32,
        y: u32,
        x_size: u32,
        y_size: u32,
        x_count_chars: u32,
        y_count_chars: u32,
        fill_attribute: u32,
        flags: u32,
        show_window: u16,
        cb_reserved2: u16,
        reserved2: *mut u8,
        std_input: *mut c_void,
        std_output: *mut c_void,
        std_error: *mut c_void,
    }

    #[repr(C)]
    struct ProcessInformation {
        process: *mut c_void,
        thread: *mut c_void,
        process_id: u32,
        thread_id: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateProcessW(
            application: *const u16,
            command_line: *mut u16,
            process_attributes: *mut c_void,
            thread_attributes: *mut c_void,
            inherit_handles: i32,
            creation_flags: u32,
            environment: *mut c_void,
            current_directory: *const u16,
            startup: *mut StartupInfoW,
            info: *mut ProcessInformation,
        ) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn TerminateProcess(handle: *mut c_void, exit_code: u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    /// The process handle, kept as an integer so the type stays `Send`.
    pub struct Helper(usize);

    impl Helper {
        pub fn spawn(exe: &Path, args: &[&OsStr]) -> io::Result<Helper> {
            let mut application: Vec<u16> = exe.as_os_str().encode_wide().collect();
            if application.contains(&0) {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let mut line = Vec::new();
            quote(exe.as_os_str(), &mut line);
            for arg in args {
                line.push(u16::from(b' '));
                quote(arg, &mut line);
            }
            application.push(0);
            line.push(0);
            let mut startup = StartupInfoW {
                cb: std::mem::size_of::<StartupInfoW>() as u32,
                reserved: null_mut(),
                desktop: null_mut(),
                title: null_mut(),
                x: 0,
                y: 0,
                x_size: 0,
                y_size: 0,
                x_count_chars: 0,
                y_count_chars: 0,
                fill_attribute: 0,
                flags: STARTF_FORCEOFFFEEDBACK,
                show_window: 0,
                cb_reserved2: 0,
                reserved2: null_mut(),
                std_input: null_mut(),
                std_output: null_mut(),
                std_error: null_mut(),
            };
            let mut info = ProcessInformation {
                process: null_mut(),
                thread: null_mut(),
                process_id: 0,
                thread_id: 0,
            };
            // No handles are inherited: the helper talks over its socket only.
            let ok = unsafe {
                CreateProcessW(
                    application.as_ptr(),
                    line.as_mut_ptr(),
                    null_mut(),
                    null_mut(),
                    0,
                    0,
                    null_mut(),
                    std::ptr::null(),
                    &mut startup,
                    &mut info,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            unsafe { CloseHandle(info.thread) };
            Ok(Helper(info.process as usize))
        }

        pub fn exited(&mut self) -> io::Result<bool> {
            match unsafe { WaitForSingleObject(self.0 as *mut c_void, 0) } {
                WAIT_OBJECT_0 => Ok(true),
                0x102 => Ok(false),
                _ => Err(io::Error::last_os_error()),
            }
        }

        pub fn kill(&mut self) -> io::Result<()> {
            if unsafe { TerminateProcess(self.0 as *mut c_void, 1) } == 0 {
                // Already gone is the outcome a kill wants.
                return match self.exited() {
                    Ok(true) => Ok(()),
                    _ => Err(io::Error::last_os_error()),
                };
            }
            Ok(())
        }

        pub fn wait(&mut self) -> io::Result<()> {
            match unsafe { WaitForSingleObject(self.0 as *mut c_void, INFINITE) } {
                WAIT_OBJECT_0 => Ok(()),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }

    impl Drop for Helper {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0 as *mut c_void) };
        }
    }

    /// One argument, quoted the way `CommandLineToArgvW` reads it back.
    fn quote(arg: &OsStr, out: &mut Vec<u16>) {
        let wide: Vec<u16> = arg.encode_wide().collect();
        let plain = !wide.is_empty()
            && !wide
                .iter()
                .any(|&c| c == u16::from(b' ') || c == u16::from(b'\t') || c == u16::from(b'"'));
        if plain {
            out.extend(wide);
            return;
        }
        out.push(u16::from(b'"'));
        let mut backslashes = 0;
        for c in wide {
            if c == u16::from(b'\\') {
                backslashes += 1;
                continue;
            }
            if c == u16::from(b'"') {
                out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2 + 1));
            } else {
                out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
            }
            backslashes = 0;
            out.push(c);
        }
        out.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2));
        out.push(u16::from(b'"'));
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn quoted(text: &str) -> String {
            let mut out = Vec::new();
            quote(OsStr::new(text), &mut out);
            String::from_utf16(&out).unwrap()
        }

        #[test]
        fn arguments_survive_the_command_line() {
            assert_eq!(quoted("47001"), "47001");
            assert_eq!(quoted(""), "\"\"");
            assert_eq!(quoted(r"C:\Program Files\a b"), r#""C:\Program Files\a b""#);
            assert_eq!(quoted(r#"say "hi""#), r#""say \"hi\"""#);
            assert_eq!(quoted(r"dir with space\"), r#""dir with space\\""#);
        }
    }
}
