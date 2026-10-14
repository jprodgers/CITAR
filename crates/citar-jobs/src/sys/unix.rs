//! Unix: owner-only files by their mode, as SSH holds a private key (P4.3.7). The standard
//! library's permissions API covers it; the job limits' calls (`setrlimit`, `setpriority`) join
//! this file in 4-08.

use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use super::{Exposure, shell_quote};

/// The mode bits that give the group or others anything.
const NOT_OWNER: u32 = 0o077;

pub(super) fn restrict_to_owner(path: &Path) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

pub(super) fn exposure(path: &Path) -> io::Result<Option<Exposure>> {
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & NOT_OWNER == 0 {
        return Ok(None);
    }
    let mut who = Vec::new();
    if mode & 0o070 != 0 {
        who.push("its group".to_owned());
    }
    if mode & 0o007 != 0 {
        who.push("others".to_owned());
    }
    Ok(Some(Exposure { who, fix: format!("chmod 600 {}", shell_quote(path)) }))
}
