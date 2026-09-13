use crate::{
    config,
    game_build::address,
    memory::{self, bounded_string, copy_string, game_string, read, store},
    minhook, packets, protection,
    structs::*,
};
use std::{
    ffi::c_char,
    sync::atomic::{AtomicUsize, Ordering},
};

macro_rules! hook {
    ($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty, $rva:expr, |$original:ident| $body:block) => {
        mod $name {
            use super::*;
            pub const RVA: usize = $rva;
            pub static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
            pub static METRIC: crate::profiling::Metric = crate::profiling::Metric::new(concat!("hook.", stringify!($name)));
            pub unsafe extern "C" fn detour($($arg: $ty),*) -> $ret {
                let _pending = METRIC.track();
                let _sample = METRIC.enter();
                #[allow(unused_variables)]
                let $original: unsafe extern "C" fn($($ty),*) -> $ret = std::mem::transmute(ORIGINAL.load(Ordering::Acquire));
                $body
            }
        }
    };
}

pub const WINDOW_TEXT: &[u8] = b"Call of Duty: Black Ops III (community patch by serious)\0";
const VERSION: &[u8] = concat!(
    "T7 Patch Rust v",
    env!("CARGO_PKG_VERSION"),
    " (experimental)"
)
.as_bytes();

extern "C" {
    fn __report_gsfailure(cookie: usize);
}
unsafe extern "C" fn report_gsfailure(_cookie: usize) {
    // Preserve the original CRT stack-failure route into crashes.log.
    std::arch::asm!("mov qword ptr [rax], rcx", in("rax") 0x17usize, in("rcx") 0usize, options(nostack));
}

fn valid_path(bytes: &[u8]) -> bool {
    bytes.split(|&b| b == b'.').all(|part| part.len() < 64)
}
// These UI callbacks receive live engine-owned strings, just as the original
// hkUI_Model_* hooks (strlen + key-size check). Querying the address space here
// made each model lookup take milliseconds in the reported September build.
unsafe fn valid_game_path(path: *const c_char) -> bool {
    game_string(path, 65536).is_some_and(valid_path)
}
unsafe fn key_is(key: *const c_char, expected: &[u8]) -> bool {
    bounded_string(key, 256).is_some_and(|key| key.eq_ignore_ascii_case(expected))
}
pub unsafe fn campaign() -> bool {
    let mode = game_fn!(0x20EAD00, unsafe extern "C" fn() -> *const c_char)();
    bounded_string(mode, 32).is_some_and(|mode| mode.eq_ignore_ascii_case(b"CP"))
}

hook!(build_title() -> *const c_char, 0x20E31C0, |original| { WINDOW_TEXT.as_ptr().cast() });
hook!(system_info(controller: i32, kind: i32, output: *mut c_char, capacity: i32) -> bool, 0x1E016C0, |original| {
    if kind != 0 { return original(controller, kind, output, capacity); }
    capacity > 0 && copy_string(output, capacity as usize, VERSION)
});
hook!(print_message() -> (), 0x1EEA940, |original| {});
hook!(print_debug() -> i32, 0x1EEA680, |original| { 0 });
hook!(exec_lua() -> (), 0x1EF8A60, |original| {});
hook!(init_server() -> (), 0x1EA9C80, |original| {});
hook!(browser(_arg: usize) -> u8, 0x1EA4E30, |original| { 0 });
hook!(subscribe(_arg: usize) -> bool, 0x20CB0C0, |original| { false });
hook!(message_name(index: i32) -> *const c_char, 0x1EDFA60, |original| {
    if !(-1..=32).contains(&index) { c"invalid".as_ptr() } else { original(index) }
});
hook!(copy(dest: *mut c_char, source: *mut c_char, size: i32) -> i64, 0x2BC4EB0, |original| {
    if size < 0 {
        if memory::readable(dest as usize, 1) { *dest = 0; }
        if memory::readable(source as usize, 1) { *source = 0; }
        return 0;
    }
    original(dest, source, size)
});
hook!(character(object: usize, index: i32, arg: i32) -> f32, 0x22C9650, |original| {
    if index < 0 || object == 0 { return -1.0; }
    let slot = object.wrapping_add(24usize.wrapping_mul(index as usize)).wrapping_add(56);
    if !memory::readable(slot, 8) || read::<usize>(slot) == 0 { -1.0 } else { original(object, index, arg) }
});
hook!(get_name(controller: i32, output: *mut c_char, capacity: i32) -> bool, 0x1EBB200, |original| {
    if capacity <= 0 || !memory::readable(output as usize, capacity as usize) { return false; }
    std::ptr::write_bytes(output, 0, capacity as usize);
    game_fn!(0x1EA4960, unsafe extern "C" fn(*mut c_char, i32, u8) -> u8)(output, capacity, 1);
    let _ = controller;
    true
});
hook!(presence(a: usize, b: usize) -> i64, 0x1E85450, |original| {
    if a == 0 || b == 0 || !memory::readable(a + 16, 8) { return 0; }
    let packed_ptr = read::<usize>(a + 16);
    if !memory::readable(packed_ptr, 4) { return 0; }
    let packed = read::<u32>(packed_ptr);
    if (packed >> 2) & 31 > 18 { store(packed_ptr, (packed & !(31 << 2)) | (18 << 2)); }
    original(a, b)
});

hook!(invite(controller: u32, message: *mut u32, xuid: u64) -> (), 0x1E19B30, |original| {
    if protection::allow_friend(xuid) { original(controller, message, xuid); }
});
hook!(join_action(controller: u32, message: *mut u32, xuid: u64) -> (), 0x1E72040, |original| {
    if protection::allow_friend(xuid) { original(controller, message, xuid); }
});
hook!(join_info(controller: u32, xuid: u64, arg: i64) -> i64, 0x1E724A0, |original| {
    if protection::allow_friend(xuid) { original(controller, xuid, arg) } else { 0 }
});

hook!(config_string(index: i32) -> *const c_char, 0x1321130, |original| {
    if [3514, 3627].contains(&index) && !campaign()
        && bounded_string(original(index), 65536).is_some_and(|s| s.len() >= 9 && s[..9].eq_ignore_ascii_case(b"mspreload")) {
            game_fn!(0x13667E0, unsafe extern "C" fn(i32, *const c_char) -> *const c_char)(index, c"".as_ptr());
    }
    original(index)
});

const LEGIT_PACKETS: &[&[u8]] = &[
    b"connectResponse",
    b"statresponse",
    b"LM",
    b"disconnect",
    b"loadoutResponse",
    b"infoResponse",
    b"statusResponse",
    b"keyAuthorize",
    b"error",
    b"print",
    b"fastrestart",
    b"ping",
    b"pinga",
    b"steamAuthReq",
    b"cfl",
];
hook!(connectionless(client: i32, from: *mut NetAdr, msg: *mut Msg) -> bool, 0x134CD70, |original| {
    let tls = game_fn!(0x212B3D0, unsafe extern "C" fn() -> usize)();
    if !memory::readable(tls + 24, 8) { return true; }
    let args = read::<usize>(tls + 24);
    if !memory::readable(args, 4) { return true; }
    let nesting = read::<i32>(args);
    if !(0..8).contains(&nesting) { return true; }
    let slot = args + (2 * nesting as usize + 34) * 4;
    if !memory::readable(slot, 8) { return true; }
    let argv = read::<usize>(slot);
    if !memory::readable(argv, 8) { return true; }
    let Some(command) = bounded_string(read::<*const c_char>(argv), 1024) else { return true; };
    if LEGIT_PACKETS.contains(&command) || (campaign() && (command.eq_ignore_ascii_case(b"requeststats") || command.eq_ignore_ascii_case(b"requeststats\n"))) {
        original(client, from, msg)
    } else { true }
});

fn sanitize_binding(input: &mut [u8]) {
    let mut i = 0;
    while i + 1 < input.len() {
        if &input[i..i + 2] == b"^B" {
            match input[i + 2..].iter().position(|&b| b == b'^') {
                Some(n) if n < 64 => {
                    i += n + 3;
                    continue;
                }
                _ => input[i] = b'.',
            }
        }
        i += 1;
    }
    for i in 0..input.len().saturating_sub(1) {
        if &input[i..i + 2] == b"[{" {
            let end = input[i + 2..].windows(2).position(|w| w == b"}]");
            if !end.is_some_and(|n| n + 2 < 256) {
                input[i] = b'.';
                input[i + 1] = b'.';
            }
        }
    }
}
hook!(binding(client: i32, translated: *const c_char, output: *mut c_char) -> *const c_char, 0x221CE90, |original| {
    if output.is_null() { return std::ptr::null(); }
    let source = if translated.is_null() { &[][..] } else {
        let Some(source) = game_string(translated, 4096) else { return std::ptr::null(); }; source
    };
    let mut input = [0u8; 4096];
    input[..source.len()].copy_from_slice(source);
    sanitize_binding(&mut input[..source.len()]);
    original(client, input.as_ptr().cast(), output)
});
hook!(model_string(controller: i32, element: *mut c_char, source: *const c_char, dest: *mut c_char, capacity: u32) -> bool, 0x1F27400, |original| {
    let Some(source) = game_string(source, 4096) else { return false; };
    let mut input = [0u8; 4096];
    input[..source.len()].copy_from_slice(source);
    let mut replaced = false;
    let mut i = 0;
    while i + 1 < source.len() {
        if &input[i..i + 2] == b"$(" {
            if let Some(n) = input[i + 2..source.len()].iter().position(|&b| b == b')') { i += n + 3; continue; }
            input[i] = b'.'; input[i + 1] = b'.'; replaced = true;
        }
        i += 1;
    }
    if replaced { copy_string(dest, capacity as usize, &input[..source.len()]) }
    else { original(controller, element, input.as_ptr().cast(), dest, capacity) }
});

hook!(model_path0(parent: i64, path: *const c_char) -> i32, 0x200CF00, |original| {
    if valid_game_path(path) { original(parent, path) } else { 0 }
});
hook!(model_path(parent: i64, path: *const c_char) -> i32, 0x200D5B0, |original| {
    if valid_game_path(path) { original(parent, path) } else { 0 }
});
hook!(model_create(parent: i64, path: *const c_char) -> i32, 0x200CFC0, |original| {
    if valid_game_path(path) { original(parent, path) } else { 0 }
});
hook!(model_alloc(parent: i32, path: *const c_char, persistent: bool) -> i32, 0x200CD00, |original| {
    if read::<u16>(address(0x16293150)) != 0 && game_string(path, 64).is_some() { original(parent, path, persistent) } else { 0 }
});

hook!(package_int(msg: *mut LobbyMsg, key: *const c_char, value: *mut i32) -> bool, 0x1EEA3E0, |original| {
    let result = original(msg, key, value);
    if result && [b"lobbytype".as_slice(), b"srclobbytype", b"destlobbytype"].iter().any(|k| key_is(key, k)) {
        return (0..=2).contains(&value.read_unaligned());
    }
    result
});
hook!(package_uint(msg: *mut LobbyMsg, key: *const c_char, value: *mut u32) -> bool, 0x1EEA490, |original| {
    let result = original(msg, key, value);
    if result && [b"lobbytype".as_slice(), b"srclobbytype", b"destlobbytype"].iter().any(|k| key_is(key, k)) && value.read_unaligned() > 2 { return false; }
    if result && key_is(key, b"datamask") && read::<u32>(address(0x1686E874)) & 15 == 0 { return value.read_unaligned() & 512 == 0; }
    result
});
hook!(package_uchar(msg: *mut LobbyMsg, key: *const c_char, value: *mut u8) -> bool, 0x1EEA450, |original| {
    let result = original(msg, key, value);
    if result && key_is(key, b"nattype") && *value > 4 { *value = 4; }
    result
});
hook!(prep_write(msg: usize, data: usize, length: i32, kind: i32) -> bool, 0x1EEA560, |original| {
    if !original(msg, data, length, kind) { return false; }
    let pass = config::password()[1];
    if (pass >> 16) as u8 != 0 {
        let write = game_fn!(0x20FF3C0, unsafe extern "C" fn(usize, u8));
        write(msg, (pass >> 16) as u8); write(msg, (pass >> 24) as u8);
    }
    true
});
hook!(prep_read(msg: usize) -> bool, 0x1EEB8D0, |original| {
    if !original(msg) { return false; }
    let pass = config::password()[1];
    if (pass >> 16) as u8 == 0 { return true; }
    let read_byte = game_fn!(0x20FD050, unsafe extern "C" fn(usize) -> u8);
    read_byte(msg) == (pass >> 16) as u8 && read_byte(msg) == (pass >> 24) as u8
});
hook!(verify_checksum(payload: *const u8, length: i32) -> i64, 0x211F5A0, |original| {
    if length < 2 || !memory::readable(payload as usize, length as usize) { return -1; }
    let n = length - 2;
    let checksum = game_fn!(0x211F430, unsafe extern "C" fn(*const u8, i32) -> u16)(payload, n);
    let received = read::<u16>(payload as usize + n as usize);
    let pass = config::password();
    if received == checksum ^ pass[1] as u16 || (windows_sys::Win32::System::SystemInformation::GetTickCount64().saturating_sub(pass[2]) <= 1500 && received == checksum ^ pass[0] as u16) { n as i64 } else { -1 }
});
hook!(checksum_copy(dest: *mut u8, source: *const u8, length: i32) -> u16, 0x211F500, |original| {
    original(dest, source, length) ^ config::password()[1] as u16
});
hook!(instant(sender: u64, _controller: u32, message: *const u8, length: u32) -> i64, 0x143A620, |original| {
    if length < 2 || length > i32::MAX as u32 || !memory::readable(message as usize, length as usize) { return 0; }
    let mut msg = Msg::default();
    game_fn!(0x20FCC10, unsafe extern "C" fn(*mut Msg, *const u8, i32))(&mut msg, message, length as i32);
    game_fn!(0x20FC900, unsafe extern "C" fn(*mut Msg))(&mut msg);
    let read_byte = game_fn!(0x20FD050, unsafe extern "C" fn(*mut Msg) -> u8);
    if read_byte(&mut msg) != b'1' { return 0; }
    let kind = read_byte(&mut msg);
    let Some(remaining) = msg.cur_size.checked_sub(msg.read_count) else { return 0; };
    if msg.overflowed != 0 || remaining >= 2048 || [0x65, 0x6d].contains(&kind) { return 0; }
    if kind == 0x66 && (remaining != 0x64 || !memory::readable(msg.data as usize + msg.read_count as usize, 4) || read::<u32>(msg.data as usize + msg.read_count as usize) == 0) { return 0; }
    if kind == 0x68 && packets::check_pending_info(sender, &msg) { return 0; }
    let mut data = [0u8; 2048];
    game_fn!(0x20FD0B0, unsafe extern "C" fn(*mut Msg, *mut u8, i32))(&mut msg, data.as_mut_ptr(), remaining as i32);
    if msg.overflowed != 0 { return 0; }
    game_fn!(packets::LOBBY_HANDLE_IM_RVA, unsafe extern "C" fn(u32, u64, *mut u8, i32) -> i64)(0, sender, data.as_mut_ptr(), remaining as i32)
});

unsafe fn menu_response(ent: *mut u8, cached: bool) {
    if !memory::readable(ent as usize, 4) {
        return;
    }
    let mut response = [0u8; 1024];
    let mut menu = [0u8; 1024];
    let nesting = read::<i32>(address(0x1681BEB0));
    if !(0..8).contains(&nesting) {
        return;
    }
    let argc = read::<i32>(address(0x1681BF14) + 4 * nesting as usize);
    let argv = game_fn!(0x20E2FD0, unsafe extern "C" fn(i32, *mut u8, i32));
    if argc != 4 {
        response[..3].copy_from_slice(b"bad");
    } else {
        argv(1, menu.as_mut_ptr(), 1024);
        if atoi(&menu) != read::<i32>(address(0x17679580)) {
            return;
        }
        argv(2, menu.as_mut_ptr(), 1024);
        let index = atoi(&menu) as u32;
        menu.fill(0);
        if index < 64 {
            let name = game_fn!(0xA7DE0, unsafe extern "C" fn(u32, u32) -> *const c_char)(0, index);
            if let Some(name) = bounded_string(name, 1024) {
                copy_string(menu.as_mut_ptr().cast(), 1024, name);
            }
        }
        argv(3, response.as_mut_ptr(), 1024);
        if cached {
            let index = atoi(&response) as u32;
            if index >= 256 {
                return;
            }
            let event =
                game_fn!(0xA78A0, unsafe extern "C" fn(u32, u32) -> *const c_char)(0, index);
            let Some(event) = bounded_string(event, 1024) else {
                return;
            };
            copy_string(response.as_mut_ptr().cast(), 1024, event);
        }
    }
    let len = response.iter().position(|&b| b == 0).unwrap_or(1023);
    let value = &response[..len];
    if value.eq_ignore_ascii_case(b"badspawn") {
        return;
    }
    let client = read::<i32>(ent as usize);
    let end_game = [b"killserverpc".as_slice(), b"endgame", b"endround"]
        .iter()
        .any(|s| value.eq_ignore_ascii_case(s));
    if (client != 0 && (end_game || value.eq_ignore_ascii_case(b"restart_level_zm")))
        || (end_game && !game_fn!(0x1ECCDC0, unsafe extern "C" fn(u32) -> bool)(1))
    {
        let command = format!("tempBanClient {client}\n\0");
        game_fn!(0x20DFF50, unsafe extern "C" fn(i32, *const u8, i32) -> bool)(
            0,
            command.as_ptr(),
            0,
        );
        return;
    }
    let add = game_fn!(0x12E9A50, unsafe extern "C" fn(i32, *const u8));
    add(0, response.as_ptr());
    add(0, menu.as_ptr());
    let thread = game_fn!(0x1B20280, unsafe extern "C" fn(*mut u8, usize, i32) -> i32)(
        ent,
        read(address(0xA5A6810)),
        2,
    );
    game_fn!(0x12EAB70, unsafe extern "C" fn(i32, i32))(0, thread);
}
fn atoi(bytes: &[u8]) -> i32 {
    let bytes = bytes.split(|&b| b == 0).next().unwrap_or_default();
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start();
    let end = text
        .char_indices()
        .take_while(|&(i, c)| c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')))
        .last()
        .map_or(0, |(i, _)| i + 1);
    text[..end].parse().unwrap_or(0)
}
hook!(menu(ent: *mut u8) -> (), 0x195F100, |original| { menu_response(ent, false); });
hook!(menu_cached(ent: *mut u8) -> (), 0x195EFA0, |original| { menu_response(ent, true); });

pub unsafe extern "C" fn block_instant(
    _this: usize,
    _sender: u64,
    _name: *const c_char,
    _msg: usize,
    _size: u32,
) -> bool {
    false
}

pub unsafe fn create(targets: &mut Vec<usize>) -> Result<(), String> {
    macro_rules! add { ($($name:ident),* $(,)?) => { $(
        let target = address($name::RVA);
        $name::METRIC.register();
        let original = minhook::create(target, $name::detour as *const () as usize)?;
        $name::ORIGINAL.store(original, Ordering::Release);
        targets.push(target);
    )* }; }
    add!(
        build_title,
        system_info,
        print_message,
        print_debug,
        exec_lua,
        init_server,
        browser,
        subscribe,
        message_name,
        copy,
        character,
        get_name,
        presence,
        invite,
        join_action,
        join_info,
        config_string,
        connectionless,
        binding,
        model_string,
        model_path0,
        model_path,
        model_create,
        model_alloc,
        package_int,
        package_uint,
        package_uchar,
        prep_write,
        prep_read,
        verify_checksum,
        checksum_copy,
        instant,
        menu,
        menu_cached
    );
    let target = __report_gsfailure as *const () as usize;
    minhook::create(target, report_gsfailure as *const () as usize)?;
    targets.push(target);
    Ok(())
}

pub unsafe fn memory_patches(patches: &mut Vec<memory::Patch>) -> Result<(), String> {
    let entries: &[(usize, &[u8])] = &[
        (0x1964766, &[0x90; 5]),
        (0x19646A7, &[0x90; 5]),
        (0x1EEACF3, &[0xb7]),
        (0x20E2A41, &[0x2f]),
        (0x22BC1CD, &[0, 0x80, 0, 0]),
        (0x22BC1D2, &[0, 0x80, 0, 0]),
        (0x1F21EF8, &[0xb6]),
        (0x1CA4C84, &[0xb6]),
    ];
    for &(rva, bytes) in entries {
        patches.push(memory::Patch::prepare(address(rva), bytes)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_path_boundaries() {
        assert!(valid_path(&[b'a'; 63]));
        assert!(!valid_path(&[b'a'; 64]));
        assert!(valid_path(b"foo.bar"));
        assert!(!valid_path(
            &[b"foo.".as_slice(), &[b'x'; 64], b".end"].concat()
        ));
    }
    #[test]
    fn game_path_matches_original_segment_checks() {
        unsafe {
            assert!(!valid_game_path(std::ptr::null()));
            for left in [0, 1, 63, 64, 65] {
                for right in [0, 1, 63, 64, 65] {
                    let path = [vec![b'a'; left], vec![b'.'], vec![b'b'; right]].concat();
                    let path = std::ffi::CString::new(path).unwrap();
                    assert_eq!(valid_game_path(path.as_ptr()), left < 64 && right < 64);
                }
            }
            assert!(valid_game_path(c"".as_ptr()));
            let mut path = b"a.".repeat(32768);
            assert!(!valid_game_path(path.as_ptr().cast())); // No NUL within the cap.
            path[65535] = 0;
            assert!(valid_game_path(path.as_ptr().cast()));
        }
    }
    #[test]
    fn malformed_directives() {
        let mut input = b"^Bunclosed [{unclosed".to_vec();
        sanitize_binding(&mut input);
        assert_eq!(input, b".Bunclosed ..unclosed");
        let mut valid = b"^Bshort^ [{key}]".to_vec();
        sanitize_binding(&mut valid);
        assert_eq!(valid, b"^Bshort^ [{key}]");
        let mut long = [b"^B".as_slice(), &[b'x'; 64], b"^"].concat();
        sanitize_binding(&mut long);
        assert_eq!(long[0], b'.');
    }
}
