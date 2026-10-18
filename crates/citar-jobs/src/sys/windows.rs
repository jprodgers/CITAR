//! Windows: owner-only files by their DACL (P4.3.7), written and read back through the security
//! API (`SetNamedSecurityInfoW`, `GetNamedSecurityInfoW`), never by parsing `icacls`, whose
//! output is localised; and the job host's limits (P4.3.6, 4-08): a Job Object the host puts
//! itself in ([`JobObject::hold_self`]), which caps its CPU and its memory hard, and the host's
//! own committed memory ([`committed_bytes`]), which that limit counts beside its runs'.
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

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_CALL_NOT_IMPLEMENTED, ERROR_SERVICE_SPECIFIC_ERROR,
    ERROR_SUCCESS, GENERIC_ALL, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree,
    NO_ERROR, SetHandleInformation,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSecurityDescriptorControl,
    GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetTokenInformation, INHERIT_ONLY_ACE,
    INHERITED_ACE, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SetKernelObjectSecurity,
    TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
    WRITE_OWNER,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_CPU_RATE_CONTROL_ENABLE,
    JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectCpuRateControlInformation,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
    SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING, SERVICE_STOPPED,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
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
        // SAFETY: the handle came from OpenProcessToken, CreateJobObjectW or CreateFileW and is
        // closed exactly once, here.
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

/// Whether this process runs elevated (its token's `TokenElevation`): an administrator's process
/// started with "Run as administrator", or one of SYSTEM's. A Windows service is installed only so,
/// and the helper says how rather than elevating (design/phase4-helper.md P4.3.9); the tests that
/// could install anything for real abort when it is (P4.7.1).
///
/// # Errors
/// The token could not be read.
pub fn is_elevated() -> io::Result<bool> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: as in `current_user_sid`: a pseudo-handle, and a token handle `Handle` closes.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    // SAFETY: `elevation` is a TOKEN_ELEVATION of exactly the size given, alive across the call.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_u32::<TOKEN_ELEVATION>(),
            &raw mut len,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(elevation.TokenIsElevated != 0)
}

/// Gives `path` a protected DACL: full control for the current user, SYSTEM and Administrators,
/// nothing for anyone else, and no inheritance from its folder.
pub(super) fn restrict_to_owner(path: &Path) -> io::Result<()> {
    restrict(path, "")
}

/// Gives the folder `path` a protected DACL as [`restrict_to_owner`] gives a file, its entries
/// inherited by every file and folder made in it (`OICI`); the API carries them down to what it
/// already holds.
pub(super) fn restrict_dir_to_owner(path: &Path) -> io::Result<()> {
    if !std::fs::metadata(path)?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a folder"));
    }
    restrict(path, "OICI")
}

/// The protected DACL of [`restrict_to_owner`], each entry with the inheritance `flags` (SDDL).
fn restrict(path: &Path, flags: &str) -> io::Result<()> {
    let user = current_user_sid()?;
    apply(
        path,
        &format!(
            "D:P(A;{flags};FA;;;{user})(A;{flags};FA;;;{SYSTEM})(A;{flags};FA;;;{ADMINISTRATORS})"
        ),
        false,
    )
}

/// Gives the folder `path`, and every file and folder under it, a protected DACL granting full
/// control to `principals` (SID strings) and nobody else anything, the folders' entries inherited
/// by what is made in them (`OICI`); with `owner`, also that owner (a SID string). Returns how many
/// objects were set.
///
/// **Nothing is set unless all of it may be:** first every object is checked, and the call refuses,
/// changing nothing, when `path` or a folder above it is a link or a junction, when anything under
/// it is one (any reparse point), when a file there has more than one name (a hard link: its other
/// name is elsewhere, and its access list would change there too), or when anything there is owned
/// by an account outside `owners` (SID strings). A link would carry the access list to wherever it
/// leads, and an object another account owns is not this folder's to take over.
///
/// **And nothing appears unchecked between the check and the change** (4-14's review): each object
/// is opened as itself (a link or junction is opened, never followed), checked again on that very
/// handle, and only then given its access list through it, folders first, each folder listed only
/// once it is set, when no account outside `principals` can add anything to it any more. The
/// access list is written on the object alone (`SetKernelObjectSecurity`): no automatic
/// propagation to what a folder holds, which would reach an object before its own check. Anything
/// found then is refused, with the objects already set named.
///
/// The Windows service's home (design/phase4-helper.md P4.3.9): SYSTEM, Administrators and the
/// service's virtual account, with nothing inherited from `%ProgramData%` (which gives Users read
/// of everything under it), owned by Administrators, so a file an owner wrote there before (a
/// `helper.json` made owner-only for its user by `citar-helper setup`) is the service's to read
/// and the helper's own checks find nobody else on it. Setting the owner to Administrators needs an
/// elevated process.
///
/// # Errors
/// The first object refused, or that the OS refused, with its path; the objects set before it
/// (none when the first check refused) are named.
pub fn restrict_tree_to(
    path: &Path,
    owner: Option<&str>,
    principals: &[&str],
    owners: &[&str],
) -> io::Result<usize> {
    restrict_tree_with(path, owner, principals, owners, &mut || {})
}

/// [`restrict_tree_to`], with `between` called after the first check and before the first change
/// (the tests plant there what appears in that moment).
pub(super) fn restrict_tree_with(
    path: &Path,
    owner: Option<&str>,
    principals: &[&str],
    owners: &[&str],
    between: &mut dyn FnMut(),
) -> io::Result<usize> {
    if !std::fs::metadata(path)?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a folder"));
    }
    let at = |p: &Path, e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", p.display()));
    let refuse = |p: &Path, why: String| {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{}: {why}", p.display())))
    };
    let mut up = Some(path);
    while let Some(p) = up {
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
            return refuse(p, "a link or junction: refused".to_owned());
        }
        up = p.parent();
    }
    // First, everything checked and nothing changed.
    let look = READ_CONTROL | FILE_READ_ATTRIBUTES;
    let mut all = vec![path.to_path_buf()];
    let mut i = 0;
    while i < all.len() {
        let here = all[i].clone();
        i += 1;
        let o = open_as_itself(&here, look).map_err(|e| at(&here, e))?;
        if let Some(why) = tree_refusal(&o, owners).map_err(|e| at(&here, e))? {
            return refuse(&here, why);
        }
        if o.is_dir() {
            for entry in std::fs::read_dir(&here).map_err(|e| at(&here, e))? {
                all.push(entry.map_err(|e| at(&here, e))?.path());
            }
        }
    }
    between();
    // Then each object set through its own handle, checked again on it first; a folder is listed
    // only once it is set, when no account outside `principals` can add to it any more.
    let entries = |flags: &str| -> String {
        let mut s = String::new();
        if let Some(o) = owner {
            s.push_str(&format!("O:{o}"));
        }
        s.push_str("D:P");
        for p in principals {
            s.push_str(&format!("(A;{flags};FA;;;{p})"));
        }
        s
    };
    let (folder, file) = (descriptor(&entries("OICI"))?, descriptor(&entries(""))?);
    let change = look | WRITE_DAC | if owner.is_some() { WRITE_OWNER } else { 0 };
    let mut set: Vec<std::path::PathBuf> = Vec::new();
    let late = |p: &Path, why: &str, set: &[std::path::PathBuf]| {
        let shown: Vec<String> = set.iter().take(5).map(|s| s.display().to_string()).collect();
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{}: {why} (found after the first check; {} object(s) already set: {}{})",
                p.display(),
                set.len(),
                if shown.is_empty() { "none".to_owned() } else { shown.join(", ") },
                if set.len() > shown.len() { ", ..." } else { "" }
            ),
        )
    };
    let set_one = |p: &Path, want_dir: bool, set: &mut Vec<std::path::PathBuf>| {
        let o = open_as_itself(p, change).map_err(|e| late(p, &e.to_string(), set))?;
        if let Some(why) = tree_refusal(&o, owners).map_err(|e| late(p, &e.to_string(), set))? {
            return Err(late(p, &why, set));
        }
        if o.is_dir() != want_dir {
            return Err(late(p, "changed kind (a folder for a file, or one for the other)", set));
        }
        let sd = if want_dir { &folder } else { &file };
        set_on_handle(&o, sd, owner.is_some()).map_err(|e| late(p, &e.to_string(), set))?;
        set.push(p.to_path_buf());
        Ok::<(), io::Error>(())
    };
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        set_one(&dir, true, &mut set)?;
        for entry in std::fs::read_dir(&dir).map_err(|e| late(&dir, &e.to_string(), &set))? {
            let entry = entry.map_err(|e| late(&dir, &e.to_string(), &set))?;
            let kind = entry.file_type().map_err(|e| late(&entry.path(), &e.to_string(), &set))?;
            if kind.is_dir() && !kind.is_symlink() {
                // Checked and set when its turn comes, before it is listed.
                stack.push(entry.path());
            } else {
                // A link or junction among them is opened as itself and refused there.
                set_one(&entry.path(), false, &mut set)?;
            }
        }
    }
    Ok(set.len())
}

/// An object opened as itself (a link or junction is opened, never followed), with its
/// attributes and its number of names.
struct Opened {
    handle: Handle,
    attributes: u32,
    links: u32,
}

impl Opened {
    fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

/// Opens `path` as itself with `access`: `FILE_FLAG_OPEN_REPARSE_POINT` opens a link or junction
/// rather than what it leads to, and `FILE_FLAG_BACKUP_SEMANTICS` lets a folder be opened at all.
fn open_as_itself(path: &Path, access: u32) -> io::Result<Opened> {
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL-terminated and alive across the call; no security attributes and no
    // template; on success the handle is `Handle`'s to close.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = Handle(raw);
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open; `info` is the structure the call fills, alive across it.
    if unsafe { GetFileInformationByHandle(handle.0, &raw mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Opened { handle, attributes: info.dwFileAttributes, links: info.nNumberOfLinks })
}

/// The owner of the object `o`, read through its handle.
fn handle_owner(o: &Opened) -> io::Result<Option<String>> {
    let mut owner: PSID = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is open with READ_CONTROL; on success the API allocates the descriptor
    // (freed by `Local`) and points `owner` into it.
    let rc = unsafe {
        GetSecurityInfo(
            o.handle.0,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc.cast_signed()));
    }
    let _sd = Local(sd);
    if owner.is_null() {
        return Ok(None);
    }
    // SAFETY: `owner` points into the descriptor, alive until `_sd` drops.
    Ok(Some(unsafe { sid_string(owner) }?))
}

/// Why the object `o` may not be given a tree's access list, or `None`: a link or junction (any
/// reparse point), a file with more than one name, or an owner outside `owners`.
fn tree_refusal(o: &Opened, owners: &[&str]) -> io::Result<Option<String>> {
    if o.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(Some("a link or junction: refused".to_owned()));
    }
    if !o.is_dir() && o.links > 1 {
        return Ok(Some(format!(
            "a file with {} names (hard links): another of them is elsewhere, where its access list \
             would change too: refused",
            o.links
        )));
    }
    let owner = handle_owner(o)?.unwrap_or_default();
    if !owners.contains(&owner.as_str()) {
        return Ok(Some(format!(
            "owned by {}, not by an account this folder belongs to: remove it",
            named(&owner)
        )));
    }
    Ok(None)
}

/// Writes the descriptor `sd` onto the object `o` alone, through its handle: its DACL (protected,
/// as the descriptor's `D:P` says) and with `with_owner` its owner. The low-level call changes
/// nothing else: a folder's new entries reach only what is made in it from now on.
fn set_on_handle(o: &Opened, sd: &Local, with_owner: bool) -> io::Result<()> {
    let mut info = DACL_SECURITY_INFORMATION;
    if with_owner {
        info |= OWNER_SECURITY_INFORMATION;
    }
    // SAFETY: the handle is open with WRITE_DAC (and WRITE_OWNER when the owner is set); `sd` is a
    // valid self-relative descriptor alive across the call, which copies it.
    if unsafe { SetKernelObjectSecurity(o.handle.0, info, sd.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The security descriptor `sddl` describes, allocated by the OS (freed by `Local`).
fn descriptor(sddl: &str) -> io::Result<Local> {
    let sddl = wide(OsStr::new(sddl));
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
    Ok(Local(sd))
}

/// Makes the folder `path` with the security descriptor `sddl` from its first moment
/// (`CreateDirectoryW` given it): nothing it would otherwise inherit applies to it, even for an
/// instant. `Ok(false)` when something was there already, left as it was (the caller checks it);
/// the folder above must exist.
///
/// # Errors
/// The descriptor does not parse, or the OS refused (with the path).
pub fn create_dir_with(path: &Path, sddl: &str) -> io::Result<bool> {
    let sd = descriptor(sddl)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_u32::<SECURITY_ATTRIBUTES>(),
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL-terminated; `attributes` and the descriptor it points to are alive
    // across the call, which copies the descriptor onto the new folder.
    if unsafe { CreateDirectoryW(name.as_ptr(), &raw const attributes) } != 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(ERROR_ALREADY_EXISTS.cast_signed()) {
        return Ok(false);
    }
    Err(io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// TrustedInstaller (`NT SERVICE\TrustedInstaller`), which owns a volume's root and Windows' own
/// folders.
pub const TRUSTED_INSTALLER: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

/// The rights over a folder that hand it to whoever holds them: deleting or renaming it
/// (`DELETE`), deleting or renaming what it holds whatever their own access lists say
/// (`FILE_DELETE_CHILD`), and changing its access list or owner (`WRITE_DAC`, `WRITE_OWNER`;
/// `GENERIC_ALL` is all of them).
const TAKES_OVER: u32 = DELETE | FILE_DELETE_CHILD | WRITE_DAC | WRITE_OWNER | GENERIC_ALL;

/// What `mask`'s rights of [`TAKES_OVER`] let their holder do, in words.
fn takeover_words(mask: u32) -> String {
    if mask & GENERIC_ALL != 0 {
        return "do anything with it".to_owned();
    }
    let mut w = Vec::new();
    if mask & DELETE != 0 {
        w.push("delete or rename it");
    }
    if mask & FILE_DELETE_CHILD != 0 {
        w.push("delete or rename what it holds");
    }
    if mask & WRITE_DAC != 0 {
        w.push("change its permissions");
    }
    if mask & WRITE_OWNER != 0 {
        w.push("take its ownership");
    }
    w.join(", ")
}

/// What lets an account outside `trusted` (SID strings) take the folder whose owner and DACL are
/// `d` over, or `None`: an owner outside them (an owner may always change the access list), no
/// DACL at all (everyone may do anything), or an entry that grants another account a right of
/// [`TAKES_OVER`]. On a volume's root (`root`), which cannot be deleted or renamed, `DELETE` is
/// harmless. An entry that applies only to what is made in the folder (inherit-only) grants nothing
/// on it; one for "OWNER RIGHTS" is the owner's, already trusted.
#[must_use]
pub fn takeover(d: &Dacl, trusted: &[&str], root: bool) -> Option<String> {
    match d.owner.as_deref() {
        Some(o) if trusted.contains(&o) => {}
        Some(o) => return Some(format!("owned by {}", named(o))),
        None => return Some("owned by no account".to_owned()),
    }
    let Some(aces) = &d.aces else {
        return Some("it has no access list, which lets everyone do anything".to_owned());
    };
    let rights = if root { TAKES_OVER & !DELETE } else { TAKES_OVER };
    for a in aces.iter().filter(|a| a.kind == AceKind::Allow && !a.inherit_only) {
        match a.sid.as_deref() {
            Some(s) if trusted.contains(&s) || s == OWNER_RIGHTS => {}
            Some(_) if a.mask & rights == 0 => {}
            Some(s) => {
                return Some(format!("lets {} {}", named(s), takeover_words(a.mask & rights)));
            }
            None => return Some("an object entry this cannot read".to_owned()),
        }
    }
    None
}

/// One folder above the service's home, as [`check_ancestors`] checks it: refused (with its path)
/// when it is a link or junction, not a folder, or [`takeover`] names what lets another account
/// take it over. A folder that is not there passes: whoever makes it decides its access list.
///
/// # Errors
/// The refusal, or what the OS could not say.
pub fn check_ancestor(p: &Path, trusted: &[&str]) -> io::Result<()> {
    let refuse = |why: String| {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{}: {why}", p.display())))
    };
    let meta = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", p.display()))),
    };
    if meta.file_type().is_symlink() {
        return refuse("a link or junction".to_owned());
    }
    if !meta.is_dir() {
        return refuse("not a folder".to_owned());
    }
    let d = dacl(p).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", p.display())))?;
    match takeover(&d, trusted, p.parent().is_none()) {
        Some(why) => refuse(why),
        None => Ok(()),
    }
}

/// Refuses, naming the folder, when a folder above `path` (its parent, up to the volume's root) is
/// a link or junction, or lets an account outside `trusted` (SID strings) take it over
/// ([`check_ancestor`]). The Windows service's binary path names its home, so an account that
/// could delete, rename or re-permission a folder above the home (under `%ProgramData%` any user
/// may make a folder, and owns it) could move the home aside, put its own in its place, and have
/// its program run as the service (4-14's review).
///
/// # Errors
/// The first folder refused, or what the OS could not say.
pub fn check_ancestors(path: &Path, trusted: &[&str]) -> io::Result<()> {
    check_ancestors_until(path, trusted, None)
}

/// [`check_ancestors`], stopping below `until` when it is one of them (the tests check their own
/// folders, whose ancestors are the machine's).
pub(super) fn check_ancestors_until(
    path: &Path,
    trusted: &[&str],
    until: Option<&Path>,
) -> io::Result<()> {
    let mut up = path.parent();
    while let Some(p) = up {
        if p.as_os_str().is_empty() || until.is_some_and(|u| p == u) {
            break;
        }
        check_ancestor(p, trusted)?;
        up = p.parent();
    }
    Ok(())
}

/// Writes the descriptor `sddl` describes onto `path`: its DACL, protected from inheritance, and
/// with `with_owner` its owner.
pub(super) fn apply(path: &Path, sddl: &str, with_owner: bool) -> io::Result<()> {
    let sd = descriptor(sddl)?;
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
    let mut owner: PSID = ptr::null_mut();
    let mut info = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    if with_owner {
        let mut owner_defaulted = 0;
        // SAFETY: as for the DACL above: the owner pointer it writes points into `sd`.
        let ok =
            unsafe { GetSecurityDescriptorOwner(sd.0, &raw mut owner, &raw mut owner_defaulted) };
        if ok == 0 || owner.is_null() {
            return Err(io::Error::other("the descriptor names no owner"));
        }
        info |= OWNER_SECURITY_INFORMATION;
    }
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL-terminated; `acl` and `owner` (null unless asked for) are valid while
    // `sd` lives; the API copies them into the file's descriptor and keeps no pointer.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            info,
            owner,
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
    exposure_of(path, "")
}

/// A folder: as [`exposure`], and the fixing command's grants inherited by what it holds.
pub(super) fn dir_exposure(path: &Path) -> io::Result<Option<Exposure>> {
    exposure_of(path, "(OI)(CI)")
}

/// Who besides the user, SYSTEM and Administrators the DACL of `path` lets in, and the `icacls`
/// line that shuts them out, its grants carrying `inherit` (icacls' inheritance flags).
fn exposure_of(path: &Path, inherit: &str) -> io::Result<Option<Exposure>> {
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
        "icacls \"{}\" /inheritance:r /grant:r *{me}:{inherit}F *{SYSTEM}:{inherit}F \
         *{ADMINISTRATORS}:{inherit}F",
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
    /// The assignment is the last step: once this process is in the job, dropping the job's
    /// handle would end it (`KILL_ON_JOB_CLOSE`), so no failure may follow it, and a failure
    /// before it leaves the process as it was, able to say `fatal limits`.
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
        // SAFETY: this process's pseudo-handle and a valid priority class.
        if unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) } == 0 {
            return Err(step("SetPriorityClass"));
        }
        // SAFETY: the job handle is open; GetCurrentProcess is a pseudo-handle that needs no
        // closing.
        if unsafe { AssignProcessToJobObject(job.0, GetCurrentProcess()) } == 0 {
            return Err(step("AssignProcessToJobObject"));
        }
        Ok(Self { _job: job })
    }
}

/// This process's committed private memory in bytes (`PrivateUsage`): what a Job Object's
/// `JobMemoryLimit` counts of it.
///
/// # Errors
/// The OS could not say.
pub fn committed_bytes() -> io::Result<u64> {
    let size = size_u32::<PROCESS_MEMORY_COUNTERS_EX>();
    let mut counters = PROCESS_MEMORY_COUNTERS_EX { cb: size, ..Default::default() };
    // SAFETY: `counters` is a PROCESS_MEMORY_COUNTERS_EX of the size given in `cb`, which the call
    // takes through a pointer to its first part and fills; the pseudo-handle needs no closing.
    let ok =
        unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut counters).cast(), size) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::try_from(counters.PrivateUsage).unwrap_or(u64::MAX))
}

// ---------------------------------------------------------------------------- the service's entry

/// What [`run_service`] runs: kept where the service manager's callbacks, which carry no context
/// of their own, can find it.
struct Registered {
    /// The service's name, NUL-terminated.
    name: Vec<u16>,
    /// Called (on the service manager's thread) when it asks the service to stop.
    on_stop: Box<dyn Fn() + Send + Sync>,
    /// The service itself; taken once, by the service's main function.
    main: std::sync::Mutex<Option<Box<dyn FnOnce() -> u32 + Send>>>,
    /// The handle its status is reported through, once registered.
    status: std::sync::Mutex<Option<StatusHandle>>,
}

/// A service status handle, valid on any thread of the process for its whole life.
#[derive(Clone, Copy)]
struct StatusHandle(SERVICE_STATUS_HANDLE);

// SAFETY: the service manager's status handle is not a pointer this process dereferences; it is
// an opaque value SetServiceStatus takes from any thread, and it is never closed.
unsafe impl Send for StatusHandle {}

static SERVICE: std::sync::OnceLock<Registered> = std::sync::OnceLock::new();

/// Reports the service's state to the service manager; `code` is the service's exit code when it
/// has stopped (0 is a stop, anything else a failure: `ERROR_SERVICE_SPECIFIC_ERROR` with the
/// code, which the failure actions count when `fFailureActionsOnNonCrashFailures` is set).
fn report(state: u32, code: u32) {
    let Some(reg) = SERVICE.get() else { return };
    let Some(handle) = *reg.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) else {
        return;
    };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: if code == 0 { NO_ERROR } else { ERROR_SERVICE_SPECIFIC_ERROR },
        dwServiceSpecificExitCode: code,
        dwCheckPoint: 0,
        dwWaitHint: if state == SERVICE_STOP_PENDING { 30_000 } else { 0 },
    };
    // SAFETY: the handle came from RegisterServiceCtrlHandlerExW; `status` is a valid
    // SERVICE_STATUS alive across the call, which copies it.
    unsafe { SetServiceStatus(handle.0, &raw const status) };
}

/// The service manager's control requests: stop and shutdown ask the service to stop.
unsafe extern "system" fn control(
    request: u32,
    _event: u32,
    _data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    match request {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            report(SERVICE_STOP_PENDING, 0);
            if let Some(reg) = SERVICE.get() {
                (reg.on_stop)();
            }
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

/// The service's main function, which the dispatcher calls on a thread of its own: it registers
/// the control handler, says the service runs, runs it, and says it stopped with its code.
unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let Some(reg) = SERVICE.get() else { return };
    // SAFETY: the name is NUL-terminated and lives in the static for the process's life; the
    // handler is a function of the right type; no context.
    let handle =
        unsafe { RegisterServiceCtrlHandlerExW(reg.name.as_ptr(), Some(control), ptr::null()) };
    if handle.is_null() {
        return;
    }
    *reg.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(StatusHandle(handle));
    report(SERVICE_RUNNING, 0);
    let main = reg.main.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    // A panic must not unwind out of a function the service manager called: it is a failure (1).
    let code =
        main.map_or(1, |m| std::panic::catch_unwind(std::panic::AssertUnwindSafe(m)).unwrap_or(1));
    report(SERVICE_STOPPED, code);
}

/// Runs this process as the Windows service `name` (design/phase4-helper.md P4.3.9: `citar-helper
/// service run`, the command the service's definition names): connects to the service manager,
/// runs `main` once it starts the service, calls `on_stop` when it asks the service to stop, and
/// reports `main`'s result as the service's exit code. Returns once the service has stopped.
///
/// # Errors
/// The service manager did not start this process (`ERROR_FAILED_SERVICE_CONTROLLER_CONNECT`, 1063:
/// it was run from a console), or it was called twice.
pub fn run_service(
    name: &str,
    on_stop: Box<dyn Fn() + Send + Sync>,
    main: Box<dyn FnOnce() -> u32 + Send>,
) -> io::Result<()> {
    let registered = Registered {
        name: wide(OsStr::new(name)),
        on_stop,
        main: std::sync::Mutex::new(Some(main)),
        status: std::sync::Mutex::new(None),
    };
    if SERVICE.set(registered).is_err() {
        return Err(io::Error::other("run_service was called twice"));
    }
    let Some(reg) = SERVICE.get() else {
        return Err(io::Error::other("the service was not registered"));
    };
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: reg.name.as_ptr().cast_mut(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW { lpServiceName: ptr::null_mut(), lpServiceProc: None },
    ];
    // SAFETY: the table ends with a null entry, its name lives in the static for the process's
    // life, and the dispatcher only reads it; the call blocks until the service has stopped.
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Makes this process's standard input, output and error handles not inheritable, so that a
/// process it starts with handles inherited (as the standard library starts every child) holds no
/// copy of them: `citar-helper run --detach` starts the Run supervisor to outlive it, and a
/// supervisor holding the pipe a script reads the detaching process's output from would keep that
/// script waiting for the pipe to close for as long as the supervisor runs (design/phase4-helper.md
/// P4.3.9). A handle that is absent or no handle at all is left as it is.
pub fn keep_std_handles_to_self() {
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle takes one of the three constants and returns a handle this process
        // owns, or null or INVALID_HANDLE_VALUE when there is none.
        let h = unsafe { GetStdHandle(which) };
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: `h` is this process's own standard handle; clearing its inherit flag changes
        // nothing but whether children get a copy, and a failure (a pseudo-handle) is harmless.
        unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) };
    }
}
