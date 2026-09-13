//! Blocking WinHTTP confined to an updater worker, with timeouts and bounded reads.
use std::{
    ffi::c_void,
    ptr::null,
    sync::atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::Networking::WinHttp::*;

struct Internet(*mut c_void);
impl Drop for Internet {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn error(operation: &str) -> String {
    format!("{operation}: {}", std::io::Error::last_os_error())
}

pub fn split_url(url: &str) -> Result<(&str, &str), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or("Updates require HTTPS")?;
    let (host, _) = rest.split_once('/').ok_or("Invalid update URL")?;
    if ![
        "api.github.com",
        "github.com",
        "release-assets.githubusercontent.com",
        "objects.githubusercontent.com",
    ]
    .contains(&host)
        || url.bytes().any(|b| b <= 32 || b >= 127 || b == b'\\')
        || url.contains('#')
    {
        return Err("Unexpected update download host or URL".into());
    }
    Ok((host, &rest[host.len()..]))
}

pub fn get(
    url: &str,
    maximum: usize,
    stop: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> Result<(u32, Vec<u8>), String> {
    let mut url = url.to_owned();
    for _ in 0..6 {
        if stop.load(Ordering::Acquire) {
            return Err("Update cancelled".into());
        }
        let (host, path) = split_url(&url)?;
        unsafe {
            let session = Internet(WinHttpOpen(
                wide(concat!("T7Patch-Rust/", env!("CARGO_PKG_VERSION"))).as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                null(),
                null(),
                0,
            ));
            if session.0.is_null() {
                return Err(error("Opening update connection"));
            }
            if WinHttpSetTimeouts(session.0, 5000, 10000, 10000, 10000) == 0 {
                return Err(error("Setting update timeouts"));
            }
            let connection = Internet(WinHttpConnect(session.0, wide(host).as_ptr(), 443, 0));
            if connection.0.is_null() {
                return Err(error("Connecting to GitHub"));
            }
            let request = Internet(WinHttpOpenRequest(
                connection.0,
                wide("GET").as_ptr(),
                wide(path).as_ptr(),
                null(),
                null(),
                null(),
                WINHTTP_FLAG_SECURE,
            ));
            if request.0.is_null() {
                return Err(error("Creating update request"));
            }
            let policy = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
            if WinHttpSetOption(
                request.0,
                WINHTTP_OPTION_REDIRECT_POLICY,
                (&policy as *const u32).cast(),
                4,
            ) == 0
            {
                return Err(error("Setting redirect policy"));
            }
            let headers = wide(if host == "api.github.com" {
                "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n"
            } else {
                "Accept: application/octet-stream\r\n"
            });
            if WinHttpSendRequest(
                request.0,
                headers.as_ptr(),
                (headers.len() - 1) as u32,
                null(),
                0,
                0,
                0,
            ) == 0
                || WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0
            {
                return Err(error("Receiving update response"));
            }
            let mut status = 0u32;
            let mut length = 4;
            if WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut status as *mut u32).cast(),
                &mut length,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(error("Reading HTTP status"));
            }
            if [301, 302, 303, 307, 308].contains(&status) {
                let mut location = [0u16; 8192];
                let mut length = std::mem::size_of_val(&location) as u32;
                if WinHttpQueryHeaders(
                    request.0,
                    WINHTTP_QUERY_LOCATION,
                    null(),
                    location.as_mut_ptr().cast(),
                    &mut length,
                    std::ptr::null_mut(),
                ) == 0
                {
                    return Err(error("Reading update redirect"));
                }
                url = String::from_utf16(
                    &location[..location
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(location.len())],
                )
                .map_err(|e| e.to_string())?;
                continue;
            }
            if status != 200 {
                return Ok((status, Vec::new()));
            }
            let mut body = Vec::new();
            loop {
                if stop.load(Ordering::Acquire) {
                    return Err("Update cancelled".into());
                }
                let mut chunk = [0u8; 16384];
                let mut count = 0;
                if WinHttpReadData(
                    request.0,
                    chunk.as_mut_ptr().cast(),
                    chunk.len() as u32,
                    &mut count,
                ) == 0
                {
                    return Err(error("Downloading update"));
                }
                if count == 0 {
                    break;
                }
                if body.len() + count as usize > maximum {
                    return Err("Update response exceeds its size limit".into());
                }
                body.extend_from_slice(&chunk[..count as usize]);
                progress(body.len());
            }
            return Ok((status, body));
        }
    }
    Err("Too many update redirects".into())
}

pub fn status_error(status: u32) -> String {
    match status {
        403 | 429 => "GitHub request limit reached. Try again later.".into(),
        404 => "The update asset is not available on GitHub.".into(),
        _ => format!("GitHub returned HTTP {status}"),
    }
}
