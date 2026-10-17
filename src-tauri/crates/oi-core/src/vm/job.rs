//! Process containment: `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
//!
//! `OILinux_PRD.md` §6.3: when the Tauri process dies for any reason, the associated QEMU
//! process must die with it. There is no resident daemon to supervise anything, so the job
//! object is what removes the orphaned writer from `user.qcow2`.

#[cfg(windows)]
mod imp {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    use crate::error::{OiError, OiResult};

    /// A job object whose only purpose is to kill everything assigned to it when the last handle
    /// is closed – which happens when this process exits, normally or not.
    pub struct KillOnCloseJob {
        handle: HANDLE,
    }

    // The handle is owned exclusively by this value and is only ever used with the job object
    // APIs, which are safe to call from any thread.
    unsafe impl Send for KillOnCloseJob {}
    unsafe impl Sync for KillOnCloseJob {}

    impl KillOnCloseJob {
        pub fn new() -> OiResult<Self> {
            unsafe {
                let handle = CreateJobObjectW(None, PCWSTR::null())
                    .map_err(|e| OiError::vm(format!("CreateJobObjectW failed: {e}")))?;

                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

                let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
                if let Err(e) = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    size,
                ) {
                    let _ = CloseHandle(handle);
                    return Err(OiError::vm(format!(
                        "SetInformationJobObject(KILL_ON_JOB_CLOSE) failed: {e}"
                    )));
                }
                Ok(KillOnCloseJob { handle })
            }
        }

        pub fn assign(&self, child: &std::process::Child) -> OiResult<()> {
            use std::os::windows::io::AsRawHandle;
            let process = HANDLE(child.as_raw_handle());
            unsafe {
                AssignProcessToJobObject(self.handle, process).map_err(|e| {
                    OiError::vm(format!(
                        "AssignProcessToJobObject failed: {e}; the QEMU process would not be \
                         cleaned up after a crash"
                    ))
                })
            }
        }

        /// Terminate every process in the job. Used as the last resort when a graceful shutdown
        /// is refused.
        pub fn terminate(&self) -> OiResult<()> {
            use windows::Win32::System::JobObjects::TerminateJobObject;
            unsafe {
                TerminateJobObject(self.handle, 1)
                    .map_err(|e| OiError::vm(format!("TerminateJobObject failed: {e}")))
            }
        }
    }

    impl Drop for KillOnCloseJob {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use crate::error::{OiError, OiResult};

    /// On non-Windows hosts the job object has no equivalent; the process group is used instead.
    pub struct KillOnCloseJob;

    impl KillOnCloseJob {
        pub fn new() -> OiResult<Self> {
            Ok(KillOnCloseJob)
        }

        pub fn assign(&self, _child: &std::process::Child) -> OiResult<()> {
            Ok(())
        }

        pub fn terminate(&self) -> OiResult<()> {
            Err(OiError::vm(
                "job objects are a Windows facility and are unavailable here".into(),
            ))
        }
    }
}

pub use imp::KillOnCloseJob;

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::sys::command;

    #[test]
    fn a_process_in_the_job_is_killed_when_the_job_closes() {
        let job = KillOnCloseJob::new().expect("create job");
        let mut child = command("timeout")
            .args(["/t", "60", "/nobreak"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .or_else(|_| {
                command("cmd")
                    .args(["/C", "ping -n 60 127.0.0.1 > nul"])
                    .stdout(std::process::Stdio::null())
                    .spawn()
            })
            .expect("spawn a long running child");
        job.assign(&child).expect("assign to job");
        assert!(child.try_wait().unwrap().is_none());
        drop(job);
        // The close of the last job handle must terminate the child.
        let mut exited = false;
        for _ in 0..50 {
            if child.try_wait().unwrap().is_some() {
                exited = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if !exited {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(exited, "the child must be terminated when the job closes");
    }
}
