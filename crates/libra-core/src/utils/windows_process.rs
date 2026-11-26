//! Kill-on-close jobs for native capture helpers and trusted exporters.
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};

use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        Threading::{
            OpenProcess, OpenThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE, ResumeThread,
            THREAD_SUSPEND_RESUME,
        },
    },
};

pub const CREATE_SUSPENDED_NO_WINDOW: u32 = 0x0000_0004 | 0x0800_0000;

pub struct CaptureJob {
    _handle: OwnedHandle,
}

impl CaptureJob {
    /// The child MUST be created suspended: assignment precedes its first
    /// instruction, so even a provider that forks immediately stays in the job.
    pub fn attach_and_resume(pid: u32) -> io::Result<Self> {
        // SAFETY: all handles are owned immediately and all outputs are initialized.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = OwnedHandle::from_raw_handle(job);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return Err(io::Error::last_os_error());
            }
            let process = OwnedHandle::from_raw_handle(process);
            if AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) == 0 {
                return Err(io::Error::last_os_error());
            }
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let snapshot = OwnedHandle::from_raw_handle(snapshot);
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of_val(&entry) as u32;
            let mut found = Thread32First(snapshot.as_raw_handle(), &mut entry);
            while found != 0 {
                if entry.th32OwnerProcessID == pid {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if thread.is_null() {
                        return Err(io::Error::last_os_error());
                    }
                    let thread = OwnedHandle::from_raw_handle(thread);
                    if ResumeThread(thread.as_raw_handle()) == u32::MAX {
                        return Err(io::Error::last_os_error());
                    }
                    return Ok(Self { _handle: job });
                }
                found = Thread32Next(snapshot.as_raw_handle(), &mut entry);
            }
            Err(io::Error::other(
                "capture helper has no suspended primary thread; retry the command",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        os::windows::process::CommandExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_TIMEOUT},
        System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            WaitForSingleObject,
        },
    };

    use super::*;

    #[test]
    fn closing_job_terminates_leader_and_pipe_holding_descendant() {
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "(Start-Process -WindowStyle Hidden powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep 60' -PassThru).Id; Start-Sleep 60"])
            .creation_flags(CREATE_SUSPENDED_NO_WINDOW).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let job = CaptureJob::attach_and_resume(child.id()).unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant: u32 = line.trim().parse().unwrap();
        // SAFETY: OpenProcess returns an owned handle; zero-time waits only query it.
        let descendant_handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                descendant,
            )
        };
        assert!(!descendant_handle.is_null());
        drop(job);
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.try_wait().unwrap().is_some(), "job left leader alive");
        // SAFETY: the descendant process handle remains live until CloseHandle.
        let status = unsafe { WaitForSingleObject(descendant_handle, 5000) };
        unsafe {
            CloseHandle(descendant_handle);
        }
        assert_ne!(
            status, WAIT_TIMEOUT,
            "job left pipe-holding descendant alive"
        );
    }
}
