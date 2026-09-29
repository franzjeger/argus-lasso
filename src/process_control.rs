//! Stable Linux process handles for GUI signals, including delayed actions.
use nix::{errno::Errno, libc, sys::signal::Signal};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[derive(Debug)]
pub struct ProcessHandle(OwnedFd);
impl ProcessHandle {
    /// Open first, then validate against the displayed snapshot. A recycled PID
    /// cannot redirect subsequent signals because they use the descriptor.
    /// Refuses this process: stopping or killing ourselves is never intended.
    pub fn open(pid: u32, start_ticks: u64) -> Result<Self, Errno> {
        if pid == std::process::id() {
            return Err(Errno::EPERM);
        }
        Self::track(pid, start_ticks)
    }
    /// As `open`, but also for this process: for holding an identity while a
    /// priority or affinity change is prepared, which is harmless to apply to
    /// ourselves.
    pub fn track(pid: u32, start_ticks: u64) -> Result<Self, Errno> {
        if pid == 0 || pid > i32::MAX as u32 {
            return Err(Errno::EINVAL);
        }
        // SAFETY: pidfd_open takes two integer arguments and returns an owned fd.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if fd < 0 {
            return Err(Errno::last());
        }
        // SAFETY: a successful pidfd_open gives us unique ownership.
        let handle = Self(unsafe { OwnedFd::from_raw_fd(fd as i32) });
        if !crate::fast_proc::read_stat(pid, &mut [0; 1024])
            .is_some_and(|s| s.starttime == start_ticks)
        {
            return Err(Errno::ESRCH);
        }
        Ok(handle)
    }
    /// True once the process has exited, reaped or not. A pidfd polls
    /// readable from that moment, so this never blocks.
    pub fn has_exited(&self) -> bool {
        let mut pfd = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd that lives across the call; a zero timeout
        // returns immediately.
        unsafe { libc::poll(&mut pfd, 1, 0) > 0 }
    }
    pub fn signal(&self, signal: Signal) -> Result<(), Errno> {
        // SAFETY: the descriptor remains owned; null siginfo requests a standard signal.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                signal as i32,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 {
            Err(Errno::last())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_process_groups_and_self_suspension() {
        assert!(matches!(ProcessHandle::open(0, 0), Err(Errno::EINVAL)));
        assert!(matches!(
            ProcessHandle::open(u32::MAX, 0),
            Err(Errno::EINVAL)
        ));
        assert!(matches!(
            ProcessHandle::open(std::process::id(), 0),
            Err(Errno::EPERM)
        ));
    }
    #[test]
    fn stale_snapshot_is_rejected_and_dead_handle_does_not_signal_another_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let ticks = crate::fast_proc::read_stat(pid, &mut [0; 1024])
            .unwrap()
            .starttime;
        assert!(matches!(
            ProcessHandle::open(pid, ticks.wrapping_add(1)),
            Err(Errno::ESRCH)
        ));
        let handle = ProcessHandle::open(pid, ticks).unwrap();
        assert!(!handle.has_exited());
        handle.signal(Signal::SIGTERM).unwrap();
        child.wait().unwrap();
        assert!(handle.has_exited());
        assert_eq!(handle.signal(Signal::SIGKILL), Err(Errno::ESRCH));
    }
}
