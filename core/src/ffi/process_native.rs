use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ProcessSnapshot {
    pub pid: u32,
    pub parent_pid: u32,
    pub started: u64,
    pub name: String,
    pub executable: String,
    pub working_directory: Option<String>,
    pub resident_bytes: Option<u64>,
    pub cpu_time_ns: Option<u64>,
    pub user_id: u32,
    pub command_line: Option<String>,
    pub children: Vec<u32>,
}

fn bsd(pid: u32) -> Option<libc::proc_bsdinfo> {
    let pid = i32::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: the allocation has exactly the size passed to proc_pidinfo; it is read only after a complete write.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: proc_pidinfo returned the full initialized structure.
    Some(unsafe { info.assume_init() })
}

fn start(info: &libc::proc_bsdinfo) -> Option<u64> {
    info.pbi_start_tvsec
        .checked_mul(1_000_000)?
        .checked_add(info.pbi_start_tvusec)
}

pub(crate) fn identity(pid: u32) -> Option<u64> {
    start(&bsd(pid)?)
}

fn string(bytes: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = bytes
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

pub(crate) fn snapshot(pid: u32, detailed: bool) -> Option<ProcessSnapshot> {
    let info = bsd(pid)?;
    let started = start(&info)?;
    let mut path = [0i8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: path is a writable byte array whose exact capacity is supplied.
    let length =
        unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
    let mut result = ProcessSnapshot {
        pid,
        parent_pid: info.pbi_ppid,
        started,
        name: if info.pbi_name.first().copied().unwrap_or(0) == 0 {
            string(&info.pbi_comm)
        } else {
            string(&info.pbi_name)
        },
        executable: if length > 0 {
            string(&path)
        } else {
            String::new()
        },
        working_directory: None,
        resident_bytes: None,
        cpu_time_ns: None,
        user_id: info.pbi_uid,
        command_line: None,
        children: Vec::new(),
    };
    if detailed {
        result.command_line=arguments(pid).map(|arguments|arguments.into_iter().map(|argument| {
            if argument.bytes().all(|byte|byte.is_ascii_alphanumeric() || b"/_-.".contains(&byte)) && !argument.is_empty() {argument}
            else {format!("'{}'",argument.replace('\'',"'\"'\"'"))}
        }).collect::<Vec<_>>().join(" "));
        result.children=list(&AtomicBool::new(false)).into_iter().filter(|child|child.parent_pid==pid).map(|child|child.pid).take(256).collect();
        let mut task = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        // SAFETY: task has size bytes of writable storage; the result is checked before reading.
        let written = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTASKINFO,
                0,
                task.as_mut_ptr().cast(),
                size,
            )
        };
        if written == size {
            // SAFETY: the kernel initialized the whole structure.
            let task = unsafe { task.assume_init() };
            result.resident_bytes = Some(task.pti_resident_size);
            result.cpu_time_ns = task.pti_total_user.checked_add(task.pti_total_system).and_then(mach_ns);
        }
        let mut vnode = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::uninit();
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
        // SAFETY: vnode has size bytes of writable storage; the result is checked before reading.
        let written = unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                vnode.as_mut_ptr().cast(),
                size,
            )
        };
        if written == size {
            // SAFETY: the kernel initialized the whole structure.
            let vnode = unsafe { vnode.assume_init() };
            let cwd = string(vnode.pvi_cdir.vip_path.as_flattened());
            if !cwd.is_empty() {
                result.working_directory = Some(cwd);
            }
        }
    }
    (identity(pid) == Some(started)).then_some(result)
}

/// `proc_taskinfo` CPU totals are Mach absolute-time units. They equal nanoseconds only on Intel;
/// Apple silicon's timebase is 125/3, so reading them as nanoseconds under-reports CPU about 42x.
#[allow(deprecated, reason = "libc marks the Mach timebase call deprecated in favour of a crate this project does not depend on")]
fn mach_ns(ticks: u64) -> Option<u64> {
    static TIMEBASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: info is valid writable storage that the kernel fills completely on success.
        let status = unsafe { libc::mach_timebase_info(&mut info) };
        if status == 0 && info.denom != 0 { (info.numer, info.denom) } else { (1, 1) }
    });
    u64::try_from(u128::from(ticks) * u128::from(numer) / u128::from(denom)).ok()
}

/// Resident memory and total CPU nanoseconds for one process, without the argument, child and
/// working-directory lookups that make a detailed snapshot enumerate every process.
pub(crate) fn usage(pid: u32) -> Option<(u64, u64)> {
    let mut task = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    // SAFETY: task has size bytes of writable storage; the result is checked before reading.
    let written = unsafe { libc::proc_pidinfo(pid as i32, libc::PROC_PIDTASKINFO, 0, task.as_mut_ptr().cast(), size) };
    if written != size {
        return None;
    }
    // SAFETY: the kernel initialized the whole structure.
    let task = unsafe { task.assume_init() };
    Some((task.pti_resident_size, mach_ns(task.pti_total_user.checked_add(task.pti_total_system)?)?))
}

fn arguments(pid:u32)->Option<Vec<String>> {
    let mut buffer=vec![0u8;262_144];
    let mut length=buffer.len();
    let mut mib=[libc::CTL_KERN,libc::KERN_PROCARGS2,i32::try_from(pid).ok()?];
    // SAFETY: mib has three initialized integers; buffer is writable for length bytes;
    // no new value is supplied, so this only reads the target process's argument data.
    if unsafe {libc::sysctl(mib.as_mut_ptr(),3,buffer.as_mut_ptr().cast(),&mut length,std::ptr::null_mut(),0)}!=0 || length>buffer.len() {return None;}
    buffer.truncate(length);
    let argc=i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?);
    if !(1..=4096).contains(&argc) {return None;}
    let data=buffer.get(4..)?;
    let executable_end=data.iter().position(|byte|*byte==0)?;
    let arguments=data.get(executable_end..)?;
    let start=arguments.iter().position(|byte|*byte!=0)?;
    let mut rest=arguments.get(start..)?;
    let mut result=Vec::new();
    for _ in 0..argc {
        let end=rest.iter().position(|byte|*byte==0)?;
        result.push(String::from_utf8_lossy(rest.get(..end)?).into_owned());
        rest=rest.get(end+1..)?;
    }
    Some(result)
}

pub(crate) fn list(cancel: &AtomicBool) -> Vec<ProcessSnapshot> {
    let mut pids = vec![0i32; 65_536];
    // SAFETY: pids is an initialized array of pid_t, and the supplied size is its byte capacity.
    let count = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            std::mem::size_of_val(pids.as_slice()) as i32,
        )
    };
    pids.into_iter()
        .take(count.max(0) as usize)
        .take_while(|_| !cancel.load(Ordering::Acquire))
        .filter_map(|pid| u32::try_from(pid).ok().and_then(|pid| snapshot(pid, false)))
        .collect()
}

pub(crate) fn signal(pid: u32, expected_start: u64, force: bool) -> bool {
    if pid <= 1 || pid == std::process::id() || expected_start == 0 {
        return false;
    }
    let Some(info) = bsd(pid) else {
        return false;
    };
    // SAFETY: geteuid takes no pointers and has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if info.pbi_uid != uid || start(&info) != Some(expected_start) {
        return false;
    }
    // macOS has no public pidfd signal operation. Recheck immediately before signaling;
    // this narrows, but cannot atomically eliminate, the process-exit/PID-reuse race.
    if identity(pid) != Some(expected_start) {
        return false;
    }
    // SAFETY: pid was validated as a positive, non-special PID; only two fixed signals are allowed.
    unsafe {
        libc::kill(
            pid as i32,
            if force { libc::SIGKILL } else { libc::SIGTERM },
        ) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_time_is_reported_in_nanoseconds_not_mach_ticks() {
        let pid = std::process::id();
        let (_, before) = usage(pid).expect("own task info");
        let started = std::time::Instant::now();
        let mut state = 1u64;
        while started.elapsed() < std::time::Duration::from_millis(300) {
            state = std::hint::black_box(state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
        }
        let (resident, after) = usage(pid).expect("own task info");
        // ~300 ms of busy loop on this thread; unconverted Apple-silicon ticks would read ~7 ms.
        assert!(after - before >= 200_000_000, "{} ns", after - before);
        assert!(resident > 0);
    }

    #[test]
    fn identity_survives_inspection_and_special_pids_are_refused() {
        let own = snapshot(std::process::id(), true).expect("own process is readable");
        assert_eq!(identity(own.pid), Some(own.started));
        assert!(own.started > 0);
        assert!(!signal(0, own.started, false));
        assert!(!signal(1, own.started, true));
        assert!(!signal(own.pid, own.started, true));
    }

    #[test]
    fn stale_identity_cannot_signal_and_valid_identity_can() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("fixture child");
        let started = identity(child.id()).expect("child identity");
        assert!(!signal(child.id(), started + 1, false));
        assert!(child.try_wait().expect("wait").is_none());
        assert!(signal(child.id(), started, false));
        assert!(!child.wait().expect("reap").success());
    }
}
