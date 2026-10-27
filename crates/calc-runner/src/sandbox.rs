//! Running a child process (trusted converters) inside a sandbox, with limits, talking to it over two pipes.
//!
//! - **macOS**: `sandbox-exec` with a profile that denies everything except reading the system's libraries, the
//!   interpreter's own files and the run's scratch folder, and writing in the scratch folder. No network, no
//!   other programs (not even forking), nothing else on the disk.
//! - **Linux**: `bwrap` with every namespace unshared (no network), the system and the interpreter mounted read
//!   only, and only the scratch folder writable.
//! - **Off**: no sandbox, for local development only.
//!
//! Everywhere: a fresh environment, the child in its own process group (killed whole on timeout), a watchdog that
//! stops it when it runs too long, uses too much memory or fills its scratch folder, capped output, no core dumps,
//! a bounded file size and (Linux, Python) a bounded address space.
//!
//! The child writes one-line JSON requests to fd 3 and reads one-line answers from fd 4.

use std::{
    ffi::OsString,
    fs::File,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::process::{CommandExt as _, ExitStatusExt as _},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, Once},
    thread,
    time::{Duration, Instant},
};

use crate::{Limits, RunError, SandboxMode, values::group_digits};

/// The sandbox in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    MacOs,
    Bwrap(PathBuf),
    Off,
}

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

static OFF_WARNING: Once = Once::new();

impl Kind {
    /// The sandbox `mode` asks for, checked to work here.
    pub fn find(mode: SandboxMode) -> Result<Self, RunError> {
        let kind = match mode {
            SandboxMode::Off => {
                OFF_WARNING.call_once(|| {
                    tracing::warn!(
                        "CALC_CODE_SANDBOX=off: File converters run without a sandbox and can reach the network and \
                         the disk. Use this for local development only."
                    )
                });
                return Ok(Self::Off);
            }
            SandboxMode::MacOs => Self::MacOs,
            SandboxMode::Bwrap => Self::Bwrap(find_bwrap().ok_or_else(no_bwrap)?),
            SandboxMode::Auto => {
                if cfg!(target_os = "macos") {
                    Self::MacOs
                } else if cfg!(target_os = "linux") {
                    Self::Bwrap(find_bwrap().ok_or_else(no_bwrap)?)
                } else {
                    return Err(RunError::unavailable(
                        "File converters can't run here: Silicon Calc sandboxes them with sandbox-exec (macOS) or bwrap (Linux), and this system has neither.",
                        "Run the service on macOS or Linux (with bubblewrap installed).",
                    ));
                }
            }
        };
        kind.self_test()?;
        Ok(kind)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Bwrap(_) => "bwrap",
            Self::Off => "off",
        }
    }

    /// Starts `/usr/bin/true` (or `/bin/true`) in the sandbox, so a sandbox that can't work here says so at once.
    fn self_test(&self) -> Result<(), RunError> {
        let truth = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .map(Path::new)
            .find(|p| p.exists());
        let Some(truth) = truth else { return Ok(()) };
        let out = match self {
            Self::MacOs => {
                if !Path::new(SANDBOX_EXEC).exists() {
                    return Err(RunError::unavailable(
                        format!(
                            "File converters can't run here: {SANDBOX_EXEC}, which sandboxes them on macOS, is missing."
                        ),
                        "Run the service on a standard macOS, or on Linux with bubblewrap installed.",
                    ));
                }
                Command::new(SANDBOX_EXEC)
                    .arg("-p")
                    .arg("(version 1)(allow default)")
                    .arg(truth)
                    .env_clear()
                    .output()
            }
            Self::Bwrap(bwrap) => Command::new(bwrap)
                .args([
                    "--die-with-parent",
                    "--unshare-all",
                    "--ro-bind",
                    "/",
                    "/",
                    "--",
                ])
                .arg(truth)
                .env_clear()
                .output(),
            Self::Off => return Ok(()),
        };
        match out {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(RunError::unavailable(
                format!(
                    "File converters can't run here: the {} sandbox doesn't start ({}).",
                    self.name(),
                    String::from_utf8_lossy(&o.stderr).trim()
                ),
                "On Linux, bwrap needs unprivileged user namespaces (or a setuid bwrap). For local development only, CALC_CODE_SANDBOX=off runs converters without a sandbox.",
            )),
            Err(e) => Err(RunError::unavailable(
                format!(
                    "File converters can't run here: the {} sandbox can't be started ({e}).",
                    self.name()
                ),
                "Install it, or for local development only set CALC_CODE_SANDBOX=off.",
            )),
        }
    }
}

fn find_bwrap() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
    std::env::split_paths(&path)
        .chain([PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")])
        .map(|d| d.join("bwrap"))
        .find(|p| p.is_file())
}

fn no_bwrap() -> RunError {
    RunError::unavailable(
        "File converters can't run here: bwrap (bubblewrap), which sandboxes them on Linux, isn't installed.",
        "Install bubblewrap (apt install bubblewrap), then try again.",
    )
}

/// A child to start.
pub(crate) struct Spawn<'a> {
    /// The interpreter, as configured (it may be a link).
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub scratch: &'a Path,
    /// Folders and files the child may read besides the system's libraries and its scratch folder.
    pub read: Vec<PathBuf>,
    /// The paths the child may be started as.
    pub exec: Vec<PathBuf>,
    pub limits: &'a Limits,
    /// Bound Python converter address space on Linux.
    pub address_space: bool,
    /// Trusted desktop file converters need system services; arbitrary code never gets these permissions.
    pub desktop_converter: bool,
}

/// Why the watchdog stopped a child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Timeout,
    Memory,
    Disk,
}

/// How a child ended.
#[derive(Debug)]
pub(crate) struct Finished {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub stop: Option<Stop>,
    pub stdout: String,
    pub stderr: String,
}

impl Finished {
    pub fn success(&self) -> bool {
        self.stop.is_none() && self.code == Some(0)
    }
}

/// Requests longer than this end the conversation (they are small JSON; data goes through files).
const MAX_REQUEST: usize = 16 << 20;

/// Starts the child, answers its requests with `serve` until it closes its end or exits, and waits for it.
pub(crate) fn run(
    kind: &Kind,
    spawn: &Spawn<'_>,
    serve: &mut dyn FnMut(&[u8]) -> Vec<u8>,
) -> Result<Finished, RunError> {
    let (req_r, req_w) = pipe()?;
    let (resp_r, resp_w) = pipe()?;
    let mut cmd = match kind {
        Kind::MacOs => {
            let mut c = Command::new(SANDBOX_EXEC);
            c.arg("-p")
                .arg(macos_profile(spawn))
                .arg(&spawn.program)
                .args(&spawn.args);
            c
        }
        Kind::Bwrap(bwrap) => {
            let mut c = Command::new(bwrap);
            c.args(bwrap_args(spawn))
                .arg("--")
                .arg(&spawn.program)
                .args(&spawn.args);
            c
        }
        Kind::Off => {
            let mut c = Command::new(&spawn.program);
            c.args(&spawn.args);
            c
        }
    };
    cmd.env_clear()
        .envs(spawn.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .current_dir(spawn.scratch)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let (child_req, child_resp) = (req_w.as_raw_fd(), resp_r.as_raw_fd());
    let limits = ChildLimits {
        file_bytes: spawn.limits.scratch_bytes.max(1 << 20),
        address_space: (spawn.address_space && cfg!(target_os = "linux"))
            .then_some(spawn.limits.memory_bytes.saturating_add(1 << 30)),
    };
    // SAFETY: the closure only makes async-signal-safe calls (fcntl, dup2, setrlimit, getrlimit).
    unsafe {
        cmd.pre_exec(move || prepare_child(child_req, child_resp, &limits));
    }
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| {
        RunError::unavailable(
            format!(
                "The file converter couldn't start {}: {e}",
                spawn.program.display()
            ),
            "Check CALC_RUNNER_PYTHON (scripts/setup-runner.sh makes the Python).",
        )
    })?;
    // The child's ends belong to the child now.
    drop(req_w);
    drop(resp_r);
    let pid = child.id() as i32;
    let out_cap = spawn.limits.output_bytes;
    let stdout = child
        .stdout
        .take()
        .map(|s| thread::spawn(move || capped(s, out_cap)));
    let stderr = child
        .stderr
        .take()
        .map(|s| thread::spawn(move || capped(s, out_cap)));

    let reaped = Arc::new(Mutex::new(false));
    let stop: Arc<Mutex<Option<Stop>>> = Arc::default();
    let watchdog = {
        let (reaped, stop) = (reaped.clone(), stop.clone());
        let (timeout, memory, disk) = (
            spawn.limits.timeout,
            spawn.limits.memory_bytes,
            spawn.limits.scratch_bytes,
        );
        let scratch = spawn.scratch.to_owned();
        thread::spawn(move || {
            let mut last_disk = Instant::now();
            loop {
                thread::sleep(Duration::from_millis(25));
                let done = reaped.lock().expect("reaped lock");
                if *done {
                    break;
                }
                let mut why = None;
                if started.elapsed() > timeout {
                    why = Some(Stop::Timeout);
                } else if let Some(m) = tree_memory(pid) {
                    if m > memory {
                        why = Some(Stop::Memory);
                    }
                }
                if why.is_none() && last_disk.elapsed() > Duration::from_millis(500) {
                    last_disk = Instant::now();
                    if dir_size(&scratch, 200_000) > disk {
                        why = Some(Stop::Disk);
                    }
                }
                if let Some(w) = why {
                    *stop.lock().expect("stop lock") = Some(w);
                    // SAFETY: the child is not reaped yet (`done` is held), so its process group still exists.
                    unsafe {
                        libc::killpg(pid, libc::SIGKILL);
                    }
                    break;
                }
            }
        })
    };

    // Answer requests until the child closes fd 3 (or exits).
    {
        let mut reader = BufReader::with_capacity(64 << 10, File::from(req_r));
        let mut writer = File::from(resp_w);
        let mut line = Vec::new();
        loop {
            line.clear();
            match (&mut reader)
                .take(MAX_REQUEST as u64 + 1)
                .read_until(b'\n', &mut line)
            {
                Ok(0) => break,
                Ok(_) if line.len() > MAX_REQUEST => break,
                Ok(_) => {
                    let mut answer = serve(line.trim_ascii());
                    answer.push(b'\n');
                    if writer.write_all(&answer).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    }

    // Wait for the child to end without reaping it, so the watchdog never signals a reused process group.
    wait_exited(pid);
    *reaped.lock().expect("reaped lock") = true;
    let status = child
        .wait()
        .map_err(|e| RunError::internal(format!("waiting for the converter process: {e}")))?;
    let _ = watchdog.join();
    let stdout = stdout.and_then(|t| t.join().ok()).unwrap_or_default();
    let stderr = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
    let stop = *stop.lock().expect("stop lock");
    Ok(Finished {
        code: status.code(),
        signal: status.signal(),
        stop,
        stdout,
        stderr,
    })
}

struct ChildLimits {
    file_bytes: u64,
    address_space: Option<u64>,
}

/// In the child, between fork and exec: the protocol pipes to fds 3 and 4, every other inherited descriptor
/// closed on exec, and the limits.
fn prepare_child(req_w: i32, resp_r: i32, limits: &ChildLimits) -> std::io::Result<()> {
    let err = std::io::Error::last_os_error;
    unsafe {
        // Through high numbers first, so moving one never overwrites the other.
        let a = libc::fcntl(req_w, libc::F_DUPFD_CLOEXEC, 10);
        if a < 0 {
            return Err(err());
        }
        let b = libc::fcntl(resp_r, libc::F_DUPFD_CLOEXEC, 10);
        if b < 0 {
            return Err(err());
        }
        if libc::dup2(a, 3) < 0 || libc::dup2(b, 4) < 0 {
            return Err(err());
        }
        let mut nofile = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let top = if libc::getrlimit(libc::RLIMIT_NOFILE, &mut nofile) == 0 {
            nofile.rlim_cur.min(65_536) as i32
        } else {
            4096
        };
        for fd in 5..top {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
        let set = |res, value: u64| {
            let lim = libc::rlimit {
                rlim_cur: value as libc::rlim_t,
                rlim_max: value as libc::rlim_t,
            };
            libc::setrlimit(res, &lim)
        };
        set(libc::RLIMIT_CORE, 0);
        set(libc::RLIMIT_NOFILE, 1024);
        set(libc::RLIMIT_FSIZE, limits.file_bytes);
        if let Some(bytes) = limits.address_space {
            set(libc::RLIMIT_AS, bytes);
        }
    }
    Ok(())
}

/// A pipe whose ends close on exec (the child gets its own copies at fds 3 and 4).
fn pipe() -> Result<(OwnedFd, OwnedFd), RunError> {
    let mut fds = [0i32; 2];
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(RunError::internal(format!(
                "making a pipe: {}",
                std::io::Error::last_os_error()
            )));
        }
        for fd in fds {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        Ok((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])))
    }
}

/// Blocks until the process has exited, leaving it to be reaped.
fn wait_exited(pid: i32) {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if r == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return;
        }
    }
}

/// Reads a stream to its end, keeping the first `cap` bytes.
fn capped(mut s: impl std::io::Read, cap: usize) -> String {
    let mut kept = Vec::new();
    let mut total: u64 = 0;
    let mut buf = vec![0u8; 64 << 10];
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                total += n as u64;
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let mut text = String::from_utf8_lossy(&kept).into_owned();
    if total > kept.len() as u64 {
        // Don't end in the middle of a character.
        while !text.is_empty() && text.ends_with('\u{fffd}') {
            text.pop();
        }
        text.push_str(&format!(
            "\n… (cut here: the converter printed {} bytes, and a run keeps the first {})",
            group_digits(total),
            group_digits(cap as u64)
        ));
    }
    text
}

/// The memory the process and everything it started use now, in bytes.
fn tree_memory(pid: i32) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![pid];
    let mut seen = 0;
    while let Some(p) = stack.pop() {
        seen += 1;
        if seen > 256 {
            break;
        }
        if let Some(m) = process_memory(p) {
            total += m;
        } else if p == pid {
            return None;
        }
        stack.extend(children_of(p));
    }
    Some(total)
}

#[cfg(target_os = "macos")]
fn process_memory(pid: i32) -> Option<u64> {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            &mut info as *mut libc::rusage_info_v2 as *mut libc::rusage_info_t,
        )
    };
    (r == 0).then_some(info.ri_phys_footprint.max(info.ri_resident_size))
}

#[cfg(target_os = "macos")]
fn children_of(pid: i32) -> Vec<i32> {
    let mut buf = vec![0i32; 64];
    let n = unsafe {
        libc::proc_listchildpids(
            pid,
            buf.as_mut_ptr() as *mut libc::c_void,
            (buf.len() * 4) as i32,
        )
    };
    if n <= 0 {
        return Vec::new();
    }
    // Returns a count of pids on current macOS (bytes on some older ones).
    let count = if n as usize > buf.len() {
        n as usize / 4
    } else {
        n as usize
    };
    buf.truncate(count.min(buf.len()));
    buf.retain(|&p| p > 0);
    buf
}

#[cfg(target_os = "linux")]
fn process_memory(pid: i32) -> Option<u64> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let resident: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
    Some(resident * page)
}

#[cfg(target_os = "linux")]
fn children_of(pid: i32) -> Vec<i32> {
    let mut out = Vec::new();
    if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
        for t in tasks.flatten() {
            if let Ok(text) = std::fs::read_to_string(t.path().join("children")) {
                out.extend(
                    text.split_whitespace()
                        .filter_map(|s| s.parse::<i32>().ok()),
                );
            }
        }
    }
    out
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_memory(_pid: i32) -> Option<u64> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn children_of(_pid: i32) -> Vec<i32> {
    Vec::new()
}

/// The bytes of every file under `dir`, looking at no more than `max_entries` entries.
pub(crate) fn dir_size(dir: &Path, max_entries: usize) -> u64 {
    let mut total = 0;
    let mut seen = 0;
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            seen += 1;
            if seen > max_entries {
                return total;
            }
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                stack.push(e.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// `"path"` for a sandbox profile.
fn sb_string(p: &Path) -> String {
    let s = p.to_string_lossy();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A path and, when it differs, the path it resolves to (the sandbox sees resolved paths).
fn both_forms(p: &Path) -> Vec<PathBuf> {
    let mut out = vec![p.to_owned()];
    if let Ok(real) = p.canonicalize()
        && real != p
    {
        out.push(real);
    }
    out
}

/// The macOS sandbox profile for one child.
pub(crate) fn macos_profile(spawn: &Spawn<'_>) -> String {
    let mut exec = String::new();
    for p in spawn.exec.iter().flat_map(|p| both_forms(p)) {
        exec.push_str(&format!(" (literal {})", sb_string(&p)));
    }
    let mut read = String::new();
    for p in spawn.read.iter().flat_map(|p| both_forms(p)) {
        read.push_str(&format!("\n  (subpath {})", sb_string(&p)));
    }
    let scratch = sb_string(spawn.scratch);
    let desktop = if spawn.desktop_converter {
        format!(
            "(allow process-fork)\n(allow mach-lookup)\n(allow ipc-posix-shm)\n(allow network* (local unix-socket) (remote unix-socket))\n(allow file-read* (subpath \"/Library/Fonts\") (subpath \"/Library/Preferences\"))\n"
        )
    } else {
        String::new()
    };
    format!(
        "(version 1)
(deny default)
{desktop}
(allow process-exec{exec})
(allow signal (target self))
(allow sysctl-read)
(allow file-read-metadata)
(allow file-read-data (literal \"/\"))
(allow file-read*
  (subpath \"/System\")
  (subpath \"/usr/lib\")
  (subpath \"/usr/share\")
  (subpath \"/private/var/db/timezone\")
  (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/random\") (literal \"/dev/urandom\"){read}
  (subpath {scratch}))
(allow file-write* (subpath {scratch}) (literal \"/dev/null\"))
(allow file-ioctl (literal \"/dev/null\"))
"
    )
}

const SYSTEM_READ_OPTIONAL: &[&str] = &[
    "/lib",
    "/lib64",
    "/lib32",
    "/bin",
    "/sbin",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/alternatives",
];

/// Mount a requested root only once, before any independent deeper roots. Rebinding a Python executable
/// through a symlink inside an already-readonly venv can require creating its destination and fail with
/// EROFS. A root already exposes all its descendants; separate resolved targets remain separate roots.
fn extra_readonly_mounts(spawn: &Spawn<'_>) -> Vec<PathBuf> {
    let mut candidates: Vec<_> = spawn
        .read
        .iter()
        .chain(&spawn.exec)
        .flat_map(|p| both_forms(p))
        .collect();
    candidates.sort_by(|a, b| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then_with(|| a.cmp(b))
    });
    let mut mounts: Vec<PathBuf> = Vec::new();
    for path in candidates {
        if path.starts_with("/usr")
            || SYSTEM_READ_OPTIONAL
                .iter()
                .any(|root| path.starts_with(root))
            || mounts.iter().any(|root| path.starts_with(root))
        {
            continue;
        }
        mounts.push(path);
    }
    mounts
}

/// The system and interpreter are read-only, scratch is writable, and every namespace (including network) is unshared.
pub(crate) fn bwrap_args(spawn: &Spawn<'_>) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut push = |items: &[&str]| a.extend(items.iter().map(OsString::from));
    push(&[
        "--die-with-parent",
        "--new-session",
        "--unshare-all",
        "--cap-drop",
        "ALL",
        "--ro-bind",
        "/usr",
        "/usr",
    ]);
    for p in SYSTEM_READ_OPTIONAL {
        push(&["--ro-bind-try", p, p]);
    }
    push(&["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]);
    for p in extra_readonly_mounts(spawn) {
        a.push("--ro-bind-try".into());
        a.push(p.clone().into());
        a.push(p.into());
    }
    a.push("--bind".into());
    a.push(spawn.scratch.into());
    a.push(spawn.scratch.into());
    a.push("--chdir".into());
    a.push(spawn.scratch.into());
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bwrap_deduplicates_venv_and_resolved_interpreter_roots_before_binding() {
        let fixture = crate::Scratch::new(&std::env::temp_dir()).unwrap();
        let venv = fixture.path.join("runner-venv");
        let base = fixture.path.join("python/cpython-3.14.2-linux-aarch64-gnu");
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        std::fs::create_dir_all(base.join("bin")).unwrap();
        let program = venv.join("bin/python");
        let real = base.join("bin/python3.14");
        std::fs::write(&real, b"test interpreter fixture").unwrap();
        std::os::unix::fs::symlink(&real, &program).unwrap();
        let neighbor = fixture.path.join("runner-venv-extra/config");
        let limits = Limits::default();
        let spawn = Spawn {
            program: program.clone(),
            args: vec![],
            env: vec![],
            scratch: &fixture.path,
            // The same order as Probe: executable aliases first, prefixes and module paths afterwards.
            read: vec![
                program.clone(),
                real.clone(),
                venv.clone(),
                base.clone(),
                base.join("lib/python314.zip"),
                base.join("lib/python3.14"),
                base.join("lib/python3.14/lib-dynload"),
                venv.join("lib/python3.14/site-packages"),
                "/usr/lib/python3.14".into(),
                neighbor.clone(),
                venv.clone(),
            ],
            exec: vec![program.clone(), "/usr/bin/true".into()],
            limits: &limits,
            address_space: true,
            desktop_converter: false,
        };
        let mounts = extra_readonly_mounts(&spawn);
        assert_eq!(mounts.len(), 3);
        assert!(mounts.contains(&venv) && mounts.contains(&base) && mounts.contains(&neighbor));
        assert!(!mounts.contains(&program) && !mounts.contains(&real));
        assert!(
            mounts
                .windows(2)
                .all(|pair| pair[0].components().count() <= pair[1].components().count())
        );

        let args = bwrap_args(&spawn);
        let readonly = args
            .windows(3)
            .filter(|w| w[0] == "--ro-bind" || w[0] == "--ro-bind-try")
            .map(|w| PathBuf::from(&w[2]))
            .collect::<Vec<_>>();
        assert_eq!(readonly.iter().filter(|p| *p == &venv).count(), 1);
        assert_eq!(readonly.iter().filter(|p| *p == &base).count(), 1);
        assert!(!readonly.contains(&program) && !readonly.contains(&real));
        assert!(!readonly.contains(&PathBuf::from("/usr/bin/true")));
        let bind = args.iter().position(|a| a == "--bind").unwrap();
        assert_eq!(PathBuf::from(&args[bind + 1]), fixture.path);
        assert!(args[bind + 3..].iter().all(|a| a != "--ro-bind-try"));
        assert!(args.iter().any(|a| a == "--unshare-all"));
    }

    #[test]
    fn bwrap_keeps_a_symlink_target_outside_the_readable_parent() {
        let fixture = crate::Scratch::new(&std::env::temp_dir()).unwrap();
        let venv = fixture.path.join("venv");
        std::fs::create_dir(&venv).unwrap();
        let real = fixture.path.join("separate-python");
        std::fs::write(&real, b"test interpreter fixture").unwrap();
        let program = venv.join("python");
        std::os::unix::fs::symlink(&real, &program).unwrap();
        let limits = Limits::default();
        let spawn = Spawn {
            program: program.clone(),
            args: vec![],
            env: vec![],
            scratch: &fixture.path,
            read: vec![venv.clone()],
            exec: vec![program],
            limits: &limits,
            address_space: true,
            desktop_converter: false,
        };
        let mounts = extra_readonly_mounts(&spawn);
        assert_eq!(mounts.len(), 2);
        assert!(mounts.contains(&venv) && mounts.contains(&real));
    }
}
