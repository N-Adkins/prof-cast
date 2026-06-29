//! Launching a program held just before `exec`, so perf events can be attached
//! and `enable_on_exec` can start sampling from its very first instruction.
//!
//! `std::process::Command` cannot express this: its own exec-synchronization
//! pipe makes `spawn` block until the child has already `exec`'d, which is too
//! late to attach. So this forks and `exec`s by hand. The child path touches
//! only async-signal-safe libc calls and never returns to Rust, which is why
//! the granular unsafe-hygiene lints are relaxed here as in `perf`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::multiple_unsafe_ops_per_block,
    clippy::undocumented_unsafe_blocks
)]

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::raw::c_char;
use std::ptr;

use profcast_core::{ProfcastError, Result};

/// A forked child blocked at a pre-`exec` barrier, plus the means to release,
/// poll, and reap it.
pub(super) struct Launched {
    pub(super) pid: u32,
    /// Write end of the barrier pipe; closing it (drop) releases the child.
    release: Option<OwnedFd>,
    reaped: bool,
}

impl Launched {
    /// Forks a child that blocks until [`release`](Launched::release), then
    /// `exec`s `argv` (`argv[0]` is the program). The child is single-threaded
    /// and pre-`exec` here, so the caller can attach perf events to its pid
    /// before letting it run.
    ///
    /// # Errors
    ///
    /// Returns [`ProfcastError::Capture`] if `argv` is empty, contains an
    /// interior NUL, or the `pipe`/`fork` fails. A failed `exec` is reported
    /// asynchronously: the child exits with status 127.
    pub(super) fn spawn(argv: &[String]) -> Result<Self> {
        // Build the C argv vector before forking; no allocation after fork.
        let c_args = argv
            .iter()
            .map(|arg| CString::new(arg.as_bytes()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| {
                ProfcastError::Capture("command contains an interior NUL byte".to_owned())
            })?;
        let prog_ptr = match c_args.first() {
            Some(prog) => prog.as_ptr(),
            None => {
                return Err(ProfcastError::Capture(
                    "empty command: no program to launch".to_owned(),
                ));
            }
        };
        let mut argv_ptrs: Vec<*const c_char> = c_args.iter().map(|arg| arg.as_ptr()).collect();
        argv_ptrs.push(ptr::null());

        let (read_fd, write_fd) = pipe_cloexec()?;
        let read_raw = read_fd.as_raw_fd();

        // SAFETY: the process is effectively single-threaded at capture time;
        // the child branch below uses only async-signal-safe calls, performs no
        // allocation, and diverges via `_exit` without returning to Rust.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(ProfcastError::Capture(format!(
                "fork failed: {}",
                io::Error::last_os_error()
            )));
        }
        if pid == 0 {
            // CHILD. Close the write end, block until the parent closes its copy
            // (EOF), then exec. On exec failure, exit with a conventional code.
            unsafe {
                libc::close(write_fd.as_raw_fd());
                block_until_released(read_raw);
                libc::execvp(prog_ptr, argv_ptrs.as_ptr());
                libc::_exit(127)
            }
        }

        // PARENT. The read end now belongs to the child.
        drop(read_fd);
        Ok(Self {
            pid: pid as u32,
            release: Some(write_fd),
            reaped: false,
        })
    }

    /// Releases the child so it `exec`s the program (by closing the barrier).
    pub(super) fn release(&mut self) {
        self.release = None;
    }

    /// Returns whether the child has exited, reaping it if so.
    pub(super) fn has_exited(&mut self) -> bool {
        if self.reaped {
            return true;
        }
        let mut status: libc::c_int = 0;
        // SAFETY: waiting on our own child pid without blocking.
        let rc = unsafe { libc::waitpid(self.pid as i32, ptr::from_mut(&mut status), libc::WNOHANG) };
        if rc == 0 {
            return false; // Still running.
        }
        // Reaped, or already gone (rc < 0): either way it is no longer alive.
        self.reaped = true;
        true
    }

    /// Kills the child if still running and reaps it.
    fn shutdown(&mut self) {
        if self.reaped {
            return;
        }
        // SAFETY: signalling and waiting on our own child pid.
        unsafe {
            libc::kill(self.pid as i32, libc::SIGKILL);
            let mut status: libc::c_int = 0;
            libc::waitpid(self.pid as i32, ptr::from_mut(&mut status), 0);
        }
        self.reaped = true;
    }
}

impl Drop for Launched {
    fn drop(&mut self) {
        // Never leave the launched child running or as a zombie.
        self.shutdown();
    }
}

/// Blocks on `fd` until EOF (the parent released us) or an unrecoverable error,
/// retrying across `EINTR`. Async-signal-safe: only `read`.
fn block_until_released(fd: RawFd) {
    let mut buf = [0_u8; 1];
    loop {
        // SAFETY: reading up to one byte into a stack buffer from a valid fd.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), 1) };
        if n < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return;
    }
}

/// Creates a close-on-exec pipe, returned as `(read, write)` owned fds.
fn pipe_cloexec() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds: [libc::c_int; 2] = [0; 2];
    // SAFETY: `fds` is a valid two-element array for pipe2 to populate.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(ProfcastError::Capture(format!(
            "pipe2 failed: {}",
            io::Error::last_os_error()
        )));
    }
    let [read, write] = fds;
    // SAFETY: pipe2 returned two fresh, owned file descriptors.
    let read = unsafe { OwnedFd::from_raw_fd(read) };
    // SAFETY: as above, for the write end.
    let write = unsafe { OwnedFd::from_raw_fd(write) };
    Ok((read, write))
}
