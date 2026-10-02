//! The process table, read with system calls alone and into memory the caller owns, so the exit
//! watcher can read it in a forked child, where allocating isn't safe.

use std::io;

/// One live process. Zombies are left out, since they can't be signalled or start anything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub sid: i32,
    /// When it started, in the OS's own unit. With the pid, it tells a reused pid apart.
    pub start: u64,
}

/// Room for the pid list on macOS. Linux reads `/proc` in small pieces and needs none.
pub const PID_SCRATCH: usize = if cfg!(target_os = "macos") {
    super::table::CAPACITY
} else {
    0
};

/// Calls `found` with every live process until it returns false. On macOS `pids` holds the pid
/// list, so it bounds how many processes are seen.
pub fn each_process(pids: &mut [i32], found: impl FnMut(Proc) -> bool) -> io::Result<()> {
    os::each_process(pids, found)
}

/// Whether `pid`'s environment holds `entry` (`NAME=value`) as a whole entry. False for a process
/// whose environment can't be read, such as another user's.
#[cfg(target_os = "linux")]
pub fn environ_has(pid: i32, entry: &[u8]) -> bool {
    os::environ_has(pid, entry)
}

#[cfg(target_os = "macos")]
mod os {
    use std::io;
    use std::mem::{MaybeUninit, size_of, size_of_val};

    use nix::libc;
    use nix::unistd::{Pid, getsid};

    use super::Proc;

    /// `SZOMB` in `<sys/proc.h>`.
    const ZOMBIE: u32 = 5;

    pub fn each_process(pids: &mut [i32], mut found: impl FnMut(Proc) -> bool) -> io::Result<()> {
        let bytes = libc::c_int::try_from(size_of_val(pids)).unwrap_or(libc::c_int::MAX);
        // SAFETY: the kernel writes at most `bytes` bytes into `pids`.
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let Ok(count) = usize::try_from(count) else {
            return Err(io::Error::last_os_error());
        };
        for &pid in &pids[..count.min(pids.len())] {
            if let Some(proc) = read(pid)
                && !found(proc)
            {
                break;
            }
        }
        Ok(())
    }

    fn read(pid: i32) -> Option<Proc> {
        if pid <= 0 {
            return None;
        }
        let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: the kernel writes at most `size` bytes into `info`.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if got != size {
            // Gone, or not ours to read.
            return None;
        }
        // SAFETY: filled whole by the kernel, and all-zero is a valid `proc_bsdinfo` anyway.
        let info = unsafe { info.assume_init() };
        if info.pbi_status == ZOMBIE {
            return None;
        }
        let sid = getsid(Some(Pid::from_raw(pid))).ok()?;
        Some(Proc {
            pid,
            ppid: i32::try_from(info.pbi_ppid).ok()?,
            pgid: i32::try_from(info.pbi_pgid).ok()?,
            sid: sid.as_raw(),
            start: info
                .pbi_start_tvsec
                .saturating_mul(1_000_000)
                .saturating_add(info.pbi_start_tvusec),
        })
    }
}

#[cfg(target_os = "linux")]
mod os {
    use std::ffi::CStr;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    use nix::libc;

    use super::{EntryMatch, Proc, dirent_names, parse_pid, parse_stat};

    pub fn each_process(_pids: &mut [i32], mut found: impl FnMut(Proc) -> bool) -> io::Result<()> {
        let proc_dir = open(c"/proc", libc::O_DIRECTORY)?;
        let mut entries = [0u8; 8 * 1024];
        loop {
            // SAFETY: the kernel writes at most `entries.len()` bytes into `entries`.
            let read = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    proc_dir.as_raw_fd(),
                    entries.as_mut_ptr(),
                    entries.len(),
                )
            };
            let Ok(read) = usize::try_from(read) else {
                return Err(io::Error::last_os_error());
            };
            if read == 0 {
                return Ok(());
            }
            for name in dirent_names(&entries[..read.min(entries.len())]) {
                if let Some(proc) = parse_pid(name).and_then(stat)
                    && !found(proc)
                {
                    return Ok(());
                }
            }
        }
    }

    pub fn environ_has(pid: i32, entry: &[u8]) -> bool {
        let mut path = [0u8; 32];
        let Some(path) = proc_file(&mut path, pid, b"environ") else {
            return false;
        };
        let Ok(file) = open(path, 0) else {
            return false;
        };
        let mut matcher = EntryMatch::new(entry);
        let mut chunk = [0u8; 4 * 1024];
        loop {
            let Some(read) = read_into(&file, &mut chunk) else {
                return false;
            };
            if read == 0 {
                return matcher.finish();
            }
            if matcher.feed(&chunk[..read]) {
                return true;
            }
        }
    }

    fn stat(pid: i32) -> Option<Proc> {
        let mut path = [0u8; 32];
        let file = open(proc_file(&mut path, pid, b"stat")?, 0).ok()?;
        // The fields up to the start time fit well within this, since the name is at most 15
        // bytes and none of the 22 fields is longer than 20.
        let mut text = [0u8; 1024];
        let read = read_into(&file, &mut text)?;
        parse_stat(pid, &text[..read])
    }

    /// `/proc/<pid>/<file>` written into `buf`.
    fn proc_file<'a>(buf: &'a mut [u8; 32], pid: i32, file: &[u8]) -> Option<&'a CStr> {
        let mut digits = [0u8; 10];
        let mut rest = u32::try_from(pid).ok()?;
        let mut start = digits.len();
        loop {
            start -= 1;
            digits[start] = b'0' + (rest % 10) as u8;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        let parts: [&[u8]; 4] = [b"/proc/", &digits[start..], b"/", file];
        let mut len = 0;
        for part in parts {
            buf.get_mut(len..len + part.len())?.copy_from_slice(part);
            len += part.len();
        }
        *buf.get_mut(len)? = 0;
        CStr::from_bytes_with_nul(&buf[..=len]).ok()
    }

    fn open(path: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
        // SAFETY: `path` is NUL-terminated, and the descriptor returned is owned only here.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC | flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` was just opened and nothing else holds it.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn read_into(file: &OwnedFd, buf: &mut [u8]) -> Option<usize> {
        loop {
            // SAFETY: the kernel writes at most `buf.len()` bytes into `buf`.
            let read = unsafe { libc::read(file.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            match usize::try_from(read) {
                Ok(read) => return Some(read.min(buf.len())),
                Err(_) if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod os {
    use std::io;

    use super::Proc;

    pub fn each_process(_pids: &mut [i32], _found: impl FnMut(Proc) -> bool) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "reading the process table works on Linux and macOS only",
        ))
    }
}

/// The names in a buffer of `linux_dirent64` records: an 8-byte inode, an 8-byte offset, a
/// 2-byte record length, a type byte, then the NUL-terminated name.
#[cfg(any(target_os = "linux", test))]
fn dirent_names(mut records: &[u8]) -> impl Iterator<Item = &[u8]> {
    const NAME_AT: usize = 19;
    std::iter::from_fn(move || {
        let len = usize::from(u16::from_ne_bytes([*records.get(16)?, *records.get(17)?]));
        if len <= NAME_AT || len > records.len() {
            return None;
        }
        let name = &records[NAME_AT..len];
        let name = name.split(|&byte| byte == 0).next().unwrap_or(name);
        records = &records[len..];
        Some(name)
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_pid(name: &[u8]) -> Option<i32> {
    if name.is_empty() || !name.iter().all(u8::is_ascii_digit) {
        return None;
    }
    name.iter().try_fold(0i32, |pid, &digit| {
        pid.checked_mul(10)?.checked_add(i32::from(digit - b'0'))
    })
}

/// `/proc/<pid>/stat`: the name in parentheses, which may hold anything, then the state, parent,
/// group, session, and 16 fields on, the start time. Zombies and dead processes are left out.
#[cfg(any(target_os = "linux", test))]
fn parse_stat(pid: i32, text: &[u8]) -> Option<Proc> {
    let after_name = text.iter().rposition(|&byte| byte == b')')? + 1;
    let mut fields = text[after_name..]
        .split(|&byte| byte == b' ')
        .filter(|field| !field.is_empty());
    if matches!(fields.next()?, [b'Z' | b'X' | b'x']) {
        return None;
    }
    let mut number = || -> Option<i32> {
        let field = fields.next()?;
        let (negative, digits) = match field {
            [b'-', digits @ ..] => (true, digits),
            _ => (false, field),
        };
        let value = parse_pid(digits)?;
        Some(if negative { -value } else { value })
    };
    let (ppid, pgid, sid) = (number()?, number()?, number()?);
    // From the terminal (field 7) to the interval timer (field 21).
    let start = fields.nth(15)?;
    if start.is_empty() || !start.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let start = start.iter().try_fold(0u64, |value, &digit| {
        value.checked_mul(10)?.checked_add(u64::from(digit - b'0'))
    })?;
    Some(Proc {
        pid,
        ppid,
        pgid,
        sid,
        start,
    })
}

/// Matches one `NAME=value` entry against NUL-separated entries fed in pieces of any size.
#[cfg(any(target_os = "linux", test))]
struct EntryMatch<'a> {
    entry: &'a [u8],
    matched: usize,
    still_matching: bool,
}

#[cfg(any(target_os = "linux", test))]
impl<'a> EntryMatch<'a> {
    fn new(entry: &'a [u8]) -> Self {
        Self {
            entry,
            matched: 0,
            still_matching: true,
        }
    }

    /// True once a whole entry has matched.
    fn feed(&mut self, bytes: &[u8]) -> bool {
        for &byte in bytes {
            if byte == 0 {
                if self.finish() {
                    return true;
                }
                self.matched = 0;
                self.still_matching = true;
            } else if self.still_matching && self.entry.get(self.matched) == Some(&byte) {
                self.matched += 1;
            } else {
                self.still_matching = false;
            }
        }
        false
    }

    /// Whether the last entry, cut off without its NUL, matched.
    fn finish(&self) -> bool {
        self.still_matching && self.matched == self.entry.len() && !self.entry.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_fields_follow_the_last_parenthesis() {
        let stat = b"42 (a) b (c)) S 7 42 40 0 -1 4194560 0 0 0 0 1 2 0 0 20 0 1 0 98765 1 2";
        assert_eq!(
            parse_stat(42, stat),
            Some(Proc {
                pid: 42,
                ppid: 7,
                pgid: 42,
                sid: 40,
                start: 98_765,
            })
        );
        assert_eq!(parse_stat(42, b"42 (sh) Z 7 42 40 0"), None);
        assert_eq!(parse_stat(42, b"42 (sh) S 7 42 40 0 -1 4194560 0"), None);
        assert_eq!(parse_stat(42, b"42 (sh) S 7"), None);
        assert_eq!(parse_stat(42, b"no name at all"), None);
    }

    #[test]
    fn pids_are_plain_decimal() {
        assert_eq!(parse_pid(b"4021"), Some(4021));
        for name in [&b""[..], b"self", b"12a", b"99999999999"] {
            assert_eq!(parse_pid(name), None, "{name:?}");
        }
    }

    #[test]
    fn dirent_records_yield_their_names() {
        let mut records = Vec::new();
        for (name, len) in [(&b"1"[..], 24usize), (b"self", 24), (b"12345", 32)] {
            let mut record = vec![0u8; len];
            record[16..18].copy_from_slice(&(len as u16).to_ne_bytes());
            record[19..19 + name.len()].copy_from_slice(name);
            records.extend(record);
        }
        let names: Vec<&[u8]> = dirent_names(&records).collect();
        assert_eq!(names, [&b"1"[..], b"self", b"12345"]);
        assert_eq!(dirent_names(&records[..30]).count(), 1);
    }

    #[test]
    fn environment_entries_match_whole() {
        let entry = b"ARGONAUTS_LAUNCH=abc";
        let found = |environ: &[u8], piece: usize| {
            let mut matcher = EntryMatch::new(entry);
            environ.chunks(piece).any(|chunk| matcher.feed(chunk)) || matcher.finish()
        };
        for piece in [1, 3, 64] {
            assert!(found(b"A=1\0ARGONAUTS_LAUNCH=abc\0B=2\0", piece));
            assert!(found(b"ARGONAUTS_LAUNCH=abc", piece));
            assert!(!found(b"ARGONAUTS_LAUNCH=abcd\0", piece));
            assert!(!found(b"XARGONAUTS_LAUNCH=abc\0", piece));
            assert!(!found(b"ARGONAUTS_LAUNCH=ab\0c\0", piece));
        }
    }
}
