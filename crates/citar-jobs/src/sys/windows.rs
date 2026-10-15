//! Windows: owner-only files by their DACL (P4.3.7), written and read back through the security
//! API (`SetNamedSecurityInfoW`, `GetNamedSecurityInfoW`), never by parsing `icacls`, whose
//! output is localised; and the job host's limits (P4.3.6, 4-08): a Job Object the host puts
//! itself in ([`JobObject::hold_self`]), which caps its CPU and its memory hard.
//!
//! Every block below calls an API with pointers into buffers this module owns or the API
//! allocated; each says why that is sound. What the API allocates is freed by [`Local`]'s drop,
//! and the token by [`Handle`]'s, on every path.

use std::ffi::{OsStr, c_void};
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSecurityDescriptorControl,
    GetSecurityDescriptorDacl, GetTokenInformation, INHERIT_ONLY_ACE, INHERITED_ACE,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
    JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectCpuRateControlInformation,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, OpenProcessToken, SetPriorityClass,
};

use super::Exposure;

/// The local system account.
const SYSTEM: &str = "S-1-5-18";
/// The local Administrators group.
const ADMINISTRATORS: &str = "S-1-5-32-544";
/// "OWNER RIGHTS": an entry for whoever owns the object.
const OWNER_RIGHTS: &str = "S-1-3-4";

/// The ACE types whose body is a mask then a SID (`ACCESS_ALLOWED_ACE`'s layout): allowed,
/// denied, and their callback forms. The object forms (5, 6, 11, 12) carry GUIDs first.
const ALLOWED: u8 = 0;
const DENIED: u8 = 1;
const ALLOWED_OBJECT: u8 = 5;
const DENIED_OBJECT: u8 = 6;
const ALLOWED_CALLBACK: u8 = 9;
const DENIED_CALLBACK: u8 = 10;
const ALLOWED_CALLBACK_OBJECT: u8 = 11;
const DENIED_CALLBACK_OBJECT: u8 = 12;

/// What one access-control entry does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AceKind {
    Allow,
    Deny,
    /// Any other type (audit and alarm entries belong in a SACL, not here).
    Other(u8),
}

/// One access-control entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ace {
    pub kind: AceKind,
    /// The principal, as a SID string; `None` for the object forms, whose SID this does not read.
    pub sid: Option<String>,
    /// The access mask.
    pub mask: u32,
    /// Inherited from the parent folder.
    pub inherited: bool,
    /// Applies to children only, not to this object.
    pub inherit_only: bool,
}

/// A file's owner and DACL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dacl {
    /// The owner's SID.
    pub owner: Option<String>,
    /// Protected from inheritance.
    pub protected: bool,
    /// The entries; `None` for a NULL DACL, which grants everyone everything.
    pub aces: Option<Vec<Ace>>,
}

/// Memory a security call allocated with `LocalAlloc`.
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from an API documented to allocate with LocalAlloc
            // (GetNamedSecurityInfoW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            // ConvertSidToStringSidW), and it is freed exactly once, here.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// A kernel handle this module opened.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcessToken or CreateJobObjectW and is closed exactly
        // once, here.
        unsafe { CloseHandle(self.0) };
    }
}

/// `s` as a NUL-terminated UTF-16 string.
fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// The text of a SID.
///
/// # Safety
/// `sid` must point to a valid SID that outlives the call.
unsafe fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: the caller guarantees `sid`; on success the API allocates `text`, NUL-terminated,
    // which `Local` frees.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _owned = Local(text.cast());
    let mut len = 0usize;
    // SAFETY: `text` is NUL-terminated, so every index up to the terminator is in bounds.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` units before the terminator, all initialised, alive until `_owned` drops.
    let units = unsafe { std::slice::from_raw_parts(text, len) };
    Ok(String::from_utf16_lossy(units))
}

/// The SID of the user this process runs as: its token's user.
///
/// # Errors
/// The token could not be read.
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing; on success
    // OpenProcessToken writes a handle that `Handle` closes.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let mut len = 0u32;
    // SAFETY: a null buffer of length 0 asks for the size; the call writes only `len`.
    unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &raw mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64s, so the buffer is aligned for TOKEN_USER (a pointer and a u32).
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` bytes, suitably aligned, and outlives the call.
    let ok = unsafe {
        GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &raw mut len)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer begins with a TOKEN_USER whose SID points into the same
    // buffer, which lives until the end of this function.
    let user = unsafe { ptr::read(buf.as_ptr().cast::<TOKEN_USER>()) };
    // SAFETY: the SID lies in `buf`, alive across the call.
    unsafe { sid_string(user.User.Sid) }
}

/// Gives `path` a protected DACL: full control for the current user, SYSTEM and Administrators,
/// nothing for anyone else, and no inheritance from its folder.
pub(super) fn restrict_to_owner(path: &Path) -> io::Result<()> {
    let user = current_user_sid()?;
    let sddl = wide(OsStr::new(&format!(
        "D:P(A;;FA;;;{user})(A;;FA;;;{SYSTEM})(A;;FA;;;{ADMINISTRATORS})"
    )));
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated and alive across the call; on success the API allocates
    // the descriptor, which `Local` frees.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut sd,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let sd = Local(sd);
    let (mut present, mut defaulted) = (0, 0);
    let mut acl: *mut ACL = ptr::null_mut();
    // SAFETY: `sd.0` is the valid descriptor made above; the ACL pointer it writes points into
    // that descriptor, alive until `sd` drops at the end of this function.
    let ok = unsafe {
        GetSecurityDescriptorDacl(sd.0, &raw mut present, &raw mut acl, &raw mut defaulted)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if present == 0 || acl.is_null() {
        return Err(io::Error::other("the owner-only descriptor has no DACL"));
    }
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL-terminated; `acl` is valid while `sd` lives; the API copies it into
    // the file's descriptor and keeps no pointer.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            acl,
            ptr::null(),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc.cast_signed()));
    }
    Ok(())
}

/// The owner and DACL of `path`, read through the security API.
///
/// # Errors
/// The file is missing, or its security cannot be read.
pub fn dacl(path: &Path) -> io::Result<Dacl> {
    let name = wide(path.as_os_str());
    let mut owner: PSID = ptr::null_mut();
    let mut acl: *mut ACL = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `name` is NUL-terminated; on success the API allocates the descriptor (freed by
    // `Local`) and points `owner` and `acl` into it.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            &raw mut acl,
            ptr::null_mut(),
            &raw mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc.cast_signed()));
    }
    let sd = Local(sd);
    let (mut control, mut revision) = (0u16, 0u32);
    // SAFETY: `sd.0` is the valid descriptor the call above allocated.
    if unsafe { GetSecurityDescriptorControl(sd.0, &raw mut control, &raw mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let owner = if owner.is_null() {
        None
    } else {
        // SAFETY: `owner` points into `sd`, alive until the end of this function.
        Some(unsafe { sid_string(owner) }?)
    };
    let aces = if acl.is_null() { None } else { Some(read_aces(acl)?) };
    Ok(Dacl { owner, protected: control & SE_DACL_PROTECTED != 0, aces })
}

/// The entries of a DACL that lives in a descriptor the caller holds.
fn read_aces(acl: *mut ACL) -> io::Result<Vec<Ace>> {
    let mut info = ACL_SIZE_INFORMATION::default();
    let size = u32::try_from(size_of::<ACL_SIZE_INFORMATION>()).unwrap_or(u32::MAX);
    // SAFETY: `acl` is a valid ACL (the caller's descriptor holds it) and `info` is a buffer of
    // exactly the size given.
    let ok = unsafe { GetAclInformation(acl, (&raw mut info).cast(), size, AclSizeInformation) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out = Vec::with_capacity(info.AceCount as usize);
    for i in 0..info.AceCount {
        let mut ace: *mut c_void = ptr::null_mut();
        // SAFETY: `i` is below the ACL's count; the API points `ace` into the ACL.
        if unsafe { GetAce(acl, i, &raw mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let base = ace.cast::<u8>();
        // SAFETY: every ACE begins with an ACE_HEADER; read unaligned, as a copy.
        let header = unsafe { ptr::read_unaligned(base.cast::<ACE_HEADER>()) };
        let kind = match header.AceType {
            ALLOWED | ALLOWED_CALLBACK | ALLOWED_OBJECT | ALLOWED_CALLBACK_OBJECT => AceKind::Allow,
            DENIED | DENIED_CALLBACK | DENIED_OBJECT | DENIED_CALLBACK_OBJECT => AceKind::Deny,
            other => AceKind::Other(other),
        };
        let (mask, sid) =
            if matches!(header.AceType, ALLOWED | DENIED | ALLOWED_CALLBACK | DENIED_CALLBACK) {
                // SAFETY: these types have ACCESS_ALLOWED_ACE's layout (header, mask, then the SID,
                // which runs on to AceSize): both offsets are inside the entry.
                let mask = unsafe {
                    ptr::read_unaligned(
                        base.add(offset_of!(ACCESS_ALLOWED_ACE, Mask)).cast::<u32>(),
                    )
                };
                // SAFETY: the SID starts at SidStart and lies inside the ACL, alive with it.
                let sid = unsafe {
                    sid_string(base.add(offset_of!(ACCESS_ALLOWED_ACE, SidStart)).cast::<c_void>())
                }?;
                (mask, Some(sid))
            } else {
                (0, None)
            };
        let flags = u32::from(header.AceFlags);
        out.push(Ace {
            kind,
            sid,
            mask,
            inherited: flags & INHERITED_ACE != 0,
            inherit_only: flags & INHERIT_ONLY_ACE != 0,
        });
    }
    Ok(out)
}

/// A well-known SID's usual (English) name, for the message only.
fn known_name(sid: &str) -> Option<&'static str> {
    Some(match sid {
        "S-1-1-0" => "Everyone",
        "S-1-5-11" => "Authenticated Users",
        "S-1-5-32-545" => "Users",
        "S-1-5-32-546" => "Guests",
        "S-1-5-4" => "Interactive",
        "S-1-5-32-547" => "Power Users",
        "S-1-3-0" => "Creator Owner",
        "S-1-3-1" => "Creator Group",
        _ => return None,
    })
}

fn named(sid: &str) -> String {
    match known_name(sid) {
        Some(n) => format!("{sid} ({n})"),
        None => sid.to_owned(),
    }
}

pub(super) fn exposure(path: &Path) -> io::Result<Option<Exposure>> {
    let d = dacl(path)?;
    let me = current_user_sid()?;
    let allowed = [me.as_str(), SYSTEM, ADMINISTRATORS];
    let owner_ok = d.owner.as_deref().is_none_or(|o| allowed.contains(&o));
    let mut who: Vec<String> = Vec::new();
    let mut explicit: Vec<String> = Vec::new();
    let add = |who: &mut Vec<String>, entry: String| {
        if !who.contains(&entry) {
            who.push(entry);
        }
    };
    if !owner_ok && let Some(o) = &d.owner {
        add(&mut who, format!("its owner {}", named(o)));
    }
    match &d.aces {
        None => add(&mut who, "everyone (the file has no DACL)".to_owned()),
        Some(aces) => {
            for a in aces.iter().filter(|a| a.kind == AceKind::Allow && !a.inherit_only) {
                match a.sid.as_deref() {
                    Some(s) if allowed.contains(&s) => {}
                    Some(OWNER_RIGHTS) if owner_ok => {}
                    Some(s) => {
                        add(&mut who, named(s));
                        if !a.inherited && !explicit.iter().any(|e| e == s) {
                            explicit.push(s.to_owned());
                        }
                    }
                    None => add(&mut who, "an object entry".to_owned()),
                }
            }
        }
    }
    if who.is_empty() {
        return Ok(None);
    }
    let mut fix = format!(
        "icacls \"{}\" /inheritance:r /grant:r *{me}:F *{SYSTEM}:F *{ADMINISTRATORS}:F",
        path.display()
    );
    if !explicit.is_empty() {
        fix.push_str(" /remove:g");
        for s in &explicit {
            fix.push_str(&format!(" *{s}"));
        }
    }
    Ok(Some(Exposure { who, fix }))
}

/// A Job Object holding this process: its CPU capped by a hard rate and its committed memory by a
/// limit (P4.3.6's Windows row). Kept for the life of the process: with `KILL_ON_JOB_CLOSE`, the
/// job's last handle closing (this process ending, however it ends) ends anything left in it.
#[derive(Debug)]
pub struct JobObject {
    _job: Handle,
}

// SAFETY: a Job Object's handle is a kernel handle, valid on any thread of the process; nothing
// here reads or writes through it after creation, and it is closed once, by its drop.
unsafe impl Send for JobObject {}
// SAFETY: as for Send: a shared reference allows nothing but holding the handle.
unsafe impl Sync for JobObject {}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Handle")
    }
}

/// A structure's size as the Job Object calls take it.
fn size_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).unwrap_or(u32::MAX)
}

impl JobObject {
    /// Creates a Job Object whose processes together get at most `cpu_rate` hundredths of a
    /// percent of the machine's CPU cycles (`CpuRate`, 1 to 10,000, a hard cap) and commit at
    /// most `memory` bytes (`JobMemoryLimit`), with `KILL_ON_JOB_CLOSE`; puts this process in it
    /// (a job inside the job its parent may have put it in: nested jobs, Windows 8 and later);
    /// and lowers this process to `BELOW_NORMAL_PRIORITY_CLASS`, so lent CPU yields to its
    /// owner's own work.
    ///
    /// # Errors
    /// The step that failed, with the OS's reason: nothing is held then, and the caller refuses
    /// to run jobs (`limits`).
    pub fn hold_self(cpu_rate: u32, memory: usize) -> io::Result<Self> {
        let step = |what: &str| {
            let e = io::Error::last_os_error();
            io::Error::new(e.kind(), format!("{what}: {e}"))
        };
        // SAFETY: no security attributes and no name: an anonymous job, whose handle `Handle`
        // closes.
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw.is_null() {
            return Err(step("CreateJobObjectW"));
        }
        let job = Handle(raw);
        let mut cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
            ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
            ..JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default()
        };
        cpu.Anonymous.CpuRate = cpu_rate.clamp(1, 10_000);
        // SAFETY: `cpu` is the structure this information class takes, of the size given, alive
        // across the call; the API copies it.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectCpuRateControlInformation,
                (&raw const cpu).cast(),
                size_u32::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>(),
            )
        };
        if ok == 0 {
            return Err(step("the CPU rate (SetInformationJobObject)"));
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_JOB_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        limits.JobMemoryLimit = memory;
        // SAFETY: as above, with the extended limits' structure.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_u32::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(),
            )
        };
        if ok == 0 {
            return Err(step("the memory limit (SetInformationJobObject)"));
        }
        // SAFETY: the job handle is open; GetCurrentProcess is a pseudo-handle that needs no
        // closing.
        if unsafe { AssignProcessToJobObject(job.0, GetCurrentProcess()) } == 0 {
            return Err(step("AssignProcessToJobObject"));
        }
        // SAFETY: this process's pseudo-handle and a valid priority class.
        if unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) } == 0 {
            return Err(step("SetPriorityClass"));
        }
        Ok(Self { _job: job })
    }
}
