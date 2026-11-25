// SPDX-License-Identifier: MIT

//! The directory a shell reports with OSC 7.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;

/// The local path in an OSC 7 report: a `file://host/path` URL (or kitty's
/// `kitty-shell-cwd://`), or a bare absolute path. The host is not checked.
pub(crate) fn path(report: &[u8]) -> Option<PathBuf> {
    let rest = ["file://", "kitty-shell-cwd://"]
        .iter()
        .find_map(|scheme| report.strip_prefix(scheme.as_bytes()));
    let encoded = match rest {
        Some(rest) => &rest[rest.iter().position(|&byte| byte == b'/')?..],
        None if report.starts_with(b"/") => report,
        None => return None,
    };
    Some(PathBuf::from(OsString::from_vec(percent_decode(encoded)?)))
}

fn percent_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    let mut bytes = input.iter();
    while let Some(&byte) = bytes.next() {
        if byte == b'%' {
            let high = hex(*bytes.next()?)?;
            let low = hex(*bytes.next()?)?;
            out.push(high << 4 | low);
        } else {
            out.push(byte);
        }
    }
    Some(out)
}

fn hex(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}
