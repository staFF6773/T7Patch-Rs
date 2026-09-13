use crate::{
    config,
    game_build::address,
    hooks,
    memory::{self, bounded_string, read, Patch},
    packets,
    structs::*,
};
use std::{
    collections::{HashMap, HashSet},
    ffi::c_char,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        LazyLock, Mutex,
    },
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;

pub static OLD_LOBBY_PRINTS: AtomicU64 = AtomicU64::new(0);
static ORIGINAL_PROCESSOR_FEATURE: AtomicUsize = AtomicUsize::new(0);

struct Friends {
    next: u64,
    xuids: HashSet<u64>,
    refreshing: bool,
}
static FRIENDS: LazyLock<Mutex<Friends>> = LazyLock::new(|| {
    Mutex::new(Friends {
        next: 0,
        xuids: HashSet::new(),
        refreshing: false,
    })
});
static DLC: LazyLock<Mutex<HashMap<i32, bool>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
type DownloadCache = HashMap<i32, (u64, i64, i64)>;
static DOWNLOAD: LazyLock<Mutex<DownloadCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub unsafe fn own_xuid() -> u64 {
    let ptr = address(0x3390190);
    if !memory::readable(ptr, 8) {
        return 0;
    }
    let data = read::<usize>(ptr);
    if memory::readable(data, 8) {
        read(data)
    } else {
        0
    }
}
pub unsafe fn is_friend(xuid: u64) -> bool {
    let _pending = crate::profiling::FRIENDS_REFRESH.track();
    let _sample = crate::profiling::FRIENDS_REFRESH.enter();
    let now = GetTickCount64();
    cached_friend(
        &FRIENDS,
        xuid,
        now,
        read::<u8>(address(0x1686E99E)) != 0,
        || fetch_friends().map(|friends| (friends, GetTickCount64())),
    )
}

fn cached_friend(
    cache: &Mutex<Friends>,
    xuid: u64,
    now: u64,
    can_refresh: bool,
    fetch: impl FnOnce() -> Option<(HashSet<u64>, u64)>,
) -> bool {
    {
        let mut friends = cache.lock().unwrap_or_else(|e| e.into_inner());
        if !can_refresh || now < friends.next || friends.refreshing {
            return friends.xuids.contains(&xuid);
        }
        friends.refreshing = true;
    }
    // Steam may call back into the patch or wait for another game thread. Never
    // hold the cache mutex across these calls. Reentrant readers use the last
    // complete snapshot (unknown XUIDs remain rejected).
    struct Refresh<'a>(&'a Mutex<Friends>);
    impl Drop for Refresh<'_> {
        fn drop(&mut self) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).refreshing = false;
        }
    }
    let _refresh = Refresh(cache);
    let result = fetch();
    let mut friends = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((snapshot, completed)) = result {
        friends.xuids = snapshot;
        friends.next = completed.saturating_add(30000);
    }
    friends.xuids.contains(&xuid)
}

unsafe fn fetch_friends() -> Option<HashSet<u64>> {
    let object = read::<usize>(address(0x10B3DC20));
    if !memory::readable(object, 8) {
        return None;
    }
    let vtable = read::<usize>(object);
    if !memory::readable(vtable + 0x18, 16) {
        return None;
    }
    let count_fn: unsafe extern "C" fn(usize, i32) -> i32 =
        std::mem::transmute(read::<usize>(vtable + 0x18));
    let friend_fn: unsafe extern "C" fn(usize, *mut u64, i32, i32) =
        std::mem::transmute(read::<usize>(vtable + 0x20));
    let count = count_fn(object, 4);
    if !(0..=100000).contains(&count) {
        return None;
    }
    let mut friends = HashSet::new();
    for i in 0..count {
        let mut friend = 0;
        friend_fn(object, &mut friend, i, 4);
        if friend != 0 {
            friends.insert(friend);
        }
    }
    Some(friends)
}
pub unsafe fn allow_friend(xuid: u64) -> bool {
    !config::FRIENDS_ONLY.load(Ordering::Acquire) || is_friend(xuid)
}

macro_rules! steam_hook {
    ($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty, |$original:ident| $body:block) => {
        mod $name {
            use super::*;
            pub static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
            pub static METRIC: crate::profiling::Metric = crate::profiling::Metric::new(concat!("steam.", stringify!($name)));
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
steam_hook!(username(_object: usize) -> *const c_char, |original| { config::name_ptr() });
steam_hook!(username_xuid(object: usize, xuid: u64) -> *const c_char, |original| {
    if xuid == own_xuid() { config::name_ptr() } else { original(object, xuid) }
});
steam_hook!(owns(object: usize, item: i32) -> bool, |original| {
    if let Some(&value) = DLC.lock().unwrap_or_else(|e| e.into_inner()).get(&item) { return value; }
    let value = original(object, item);
    DLC.lock().unwrap_or_else(|e| e.into_inner()).insert(item, value);
    value
});
steam_hook!(owns2(object: usize, item: i32) -> bool, |original| {
    if let Some(&value) = DLC.lock().unwrap_or_else(|e| e.into_inner()).get(&item) { return value; }
    let value = original(object, item);
    DLC.lock().unwrap_or_else(|e| e.into_inner()).insert(item, value);
    value
});
steam_hook!(vac(_object: usize) -> bool, |original| { false });
steam_hook!(download(object: usize, item: i32, done: *mut i64, total: *mut i64) -> (), |original| {
    if done.is_null() || total.is_null() { return; }
    let now = GetTickCount64();
    if let Some(&(next, a, b)) = DOWNLOAD.lock().unwrap_or_else(|e| e.into_inner()).get(&item) {
        if now < next { done.write_unaligned(a); total.write_unaligned(b); return; }
    }
    original(object, item, done, total);
    DOWNLOAD.lock().unwrap_or_else(|e| e.into_inner()).insert(item, (now + 300000, done.read_unaligned(), total.read_unaligned()));
});
steam_hook!(create_lobby(object: usize, _kind: i32, max_players: i32) -> u64, |original| { original(object, 1, max_players) });
steam_hook!(read_p2p(object: usize, dest: *mut u8, capacity: u32, size: *mut u32, remote: *mut u64, channel: i32) -> bool, |original| {
    let result = original(object, dest, capacity, size, remote, channel);
    if result && !size.is_null() && size.read_unaligned() > 5 {
        if size.read_unaligned() > capacity || !memory::readable(dest as usize, 6) { return false; }
        match *dest.add(5) {
            104 => if remote.is_null() || !allow_friend(remote.read_unaligned()) { return false; },
            102 | 101 | 109 => return false,
            _ => {}
        }
    }
    result
});
steam_hook!(chat(object: usize, lobby: u64, chat_id: i32, user: *mut u64, data: *mut u8, capacity: i32, entry_type: *mut i32) -> i32, |original| {
    let result = original(object, lobby, chat_id, user, data, capacity, entry_type);
    if user.is_null() || result <= 0 || capacity <= 0 || result > capacity { return 0; }
    let controlling = game_fn!(0x1EBFDA0, unsafe extern "C" fn(i32) -> u8)(1);
    let session = game_fn!(0x1EC1650, unsafe extern "C" fn(u32) -> *mut LobbySession)(if controlling != 0 { 1 } else { 0 });
    if session.is_null() { return 0; }
    let get_client = game_fn!(0x1EF3FB0, unsafe extern "C" fn(*mut LobbySession, i32) -> *mut SessionClient);
    let sender = user.read_unaligned();
    let mut found = false;
    for i in 0..18 {
        let client = get_client(session, i);
        if !memory::readable(client as usize, size_of::<SessionClient>()) { continue; }
        let active = read::<usize>(client as usize + 8);
        if memory::readable(active + 0x410, 8) && read::<u64>(active + 0x410) == sender { found = true; break; }
    }
    if !found { return 0; }
    if !entry_type.is_null() && entry_type.read_unaligned() != 0 && memory::readable(data as usize, result as usize) {
        let bytes = std::slice::from_raw_parts_mut(data, result as usize);
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        for i in 0..len {
            if bytes[i] == b'^' || bytes[i] == b'%' || !(32..=126).contains(&bytes[i])
                || (i + 1 < len && ((bytes[i] == b'$' && bytes[i + 1] == b'(') || (bytes[i] == b'[' && bytes[i + 1] == b'{'))) { bytes[i] = b'.'; }
        }
    }
    result
});

unsafe extern "C" fn idle_update(state: i64) -> i32 {
    let _pending = crate::profiling::IDLE_UPDATE.track();
    let _sample = crate::profiling::IDLE_UPDATE.enter();
    if hooks::campaign() {
        game_fn!(0x131E350, unsafe extern "C" fn(i64) -> i32)(state)
    } else {
        0
    }
}
unsafe extern "system" fn processor_feature(feature: u32) -> i32 {
    if feature == 0x17 {
        // Intentional AV, matching the source's fast-fail-to-crash-log route.
        // Assembly avoids creating an invalid Rust reference to address 0x17.
        std::arch::asm!("mov qword ptr [rax], rcx", in("rax") 0x17usize, in("rcx") feature as usize, options(nostack));
    }
    let original: unsafe extern "system" fn(u32) -> i32 =
        std::mem::transmute(ORIGINAL_PROCESSOR_FEATURE.load(Ordering::Acquire));
    original(feature)
}

unsafe fn find_import(name: &std::ffi::CStr) -> Option<usize> {
    let base = crate::game_build::image_base();
    let nt = base + read::<u32>(base + 0x3c) as usize;
    let rva = read::<u32>(nt + 24 + 112 + 8) as usize;
    let size = read::<u32>(nt + 24 + 112 + 12) as usize;
    if rva == 0 {
        return None;
    }
    for offset in (0..size).step_by(20) {
        let descriptor = base + rva + offset;
        if !memory::readable(descriptor, 20) || read::<u32>(descriptor + 12) == 0 {
            break;
        }
        let names = read::<u32>(descriptor) as usize;
        let thunk = read::<u32>(descriptor + 16) as usize;
        if names == 0 || thunk == 0 {
            continue;
        }
        for i in 0..65536 {
            if !memory::readable(base + names + i * 8, 8)
                || !memory::readable(base + thunk + i * 8, 8)
            {
                break;
            }
            let entry = read::<u64>(base + names + i * 8);
            if entry == 0 {
                break;
            }
            if entry & (1 << 63) != 0 {
                continue;
            }
            if bounded_string((base + entry as usize + 2) as _, 256)
                .is_some_and(|s| s == name.to_bytes())
            {
                return Some(base + thunk + i * 8);
            }
        }
    }
    None
}

pub unsafe fn prepare(patches: &mut Vec<Patch>) -> std::result::Result<(), String> {
    macro_rules! steam {
        ($rva:expr, $offset:expr, $name:ident) => {{
            $name::METRIC.register();
            let object = read::<usize>(address($rva));
            if !memory::readable(object, 8) {
                return Err(format!("Steam object not ready: {:#x}", $rva));
            }
            let slot = read::<usize>(object) + $offset;
            let patch = Patch::prepare(slot, &($name::detour as *const () as usize).to_le_bytes())?;
            $name::ORIGINAL.store(read(slot), Ordering::Release);
            patches.push(patch);
        }};
    }
    steam!(0x10B3DC20, 0, username);
    steam!(0x10B3DC20, 0x38, username_xuid);
    steam!(0x10B3DC40, 0x38, owns);
    steam!(0x10B3DC40, 0x30, owns2);
    steam!(0x10B3DC40, 0x18, vac);
    steam!(0x10B3DC40, 0xb0, download);
    steam!(0x10B3DC30, 0xd8, chat);
    steam!(0x10B3DC30, 0x68, create_lobby);
    steam!(0x10B3DC50, 0x10, read_p2p);
    let import = find_import(c"IsProcessorFeaturePresent")
        .ok_or("Missing IsProcessorFeaturePresent import")?;
    ORIGINAL_PROCESSOR_FEATURE.store(read(import), Ordering::Release);
    patches.push(Patch::prepare(
        import,
        &(processor_feature as *const () as usize).to_le_bytes(),
    )?);
    patches.push(Patch::prepare(
        address(0x32A75A8),
        &(idle_update as *const () as usize).to_le_bytes(),
    )?);

    let lobby = read::<usize>(address(0x9B35878));
    if !memory::readable(lobby + 1384, 8) {
        return Err("Demonware lobby not ready".into());
    }
    let object = read::<usize>(lobby + 1384);
    if !memory::readable(object, 8) {
        return Err("Demonware object not ready".into());
    }
    let vtable = read::<usize>(object);
    if !memory::readable(vtable, 50 * 8) {
        return Err("Demonware vtable not ready".into());
    }
    // Stable lifetime: callbacks can be in flight when Unload deactivates the patch.
    let mut table = Box::new([0usize; 50]);
    for (i, entry) in table.iter_mut().enumerate() {
        *entry = read(vtable + i * 8);
    }
    table[24] = hooks::block_instant as *const () as usize;
    let table = Box::leak(table).as_ptr() as usize;
    patches.push(Patch::prepare(object, &table.to_le_bytes())?);
    let prints = address(0x156CE8D0);
    OLD_LOBBY_PRINTS.store(read(prints), Ordering::Release);
    patches.push(Patch::prepare(
        prints,
        &0xFFEEDDCC44332212u64.to_le_bytes(),
    )?);
    Ok(())
}

pub unsafe fn configure_game(patches: &mut Vec<Patch>) -> std::result::Result<(), String> {
    for rva in [0x1686ED20, 0xA0378B8] {
        let dvar = read::<usize>(address(rva));
        if !memory::readable(dvar + 0x18, 4) {
            return Err("Dvar not ready".into());
        }
        patches.push(Patch::prepare(dvar + 0x18, &0u32.to_le_bytes())?);
    }
    Ok(())
}
pub unsafe fn apply_game_settings() {
    let set = game_fn!(
        0x226B0A0,
        unsafe extern "C" fn(*const c_char, *const c_char, bool) -> usize
    );
    set(c"ui_error_callstack_ship".as_ptr(), c"1".as_ptr(), true);
    set(c"g_allowvote".as_ptr(), c"0".as_ptr(), true);
    set(c"maxvoicepacketsperframe".as_ptr(), c"0".as_ptr(), true);
    game_fn!(0x1EA6010, unsafe extern "C" fn(usize))(address(0x113A4A60));
}

pub unsafe fn inspect_exception(
    context: &mut windows_sys::Win32::System::Diagnostics::Debug::CONTEXT,
) -> bool {
    if context.Rcx != 0xFFEEDDCC44332212 {
        return false;
    }
    let _sample = crate::profiling::LOBBY_EXCEPTIONS.enter();
    let rsp = context.Rsp as usize;
    if memory::readable(rsp + 0x28, 8) && read::<usize>(rsp + 0x28) == address(0x1EEBF74) {
        packets::inspect((rsp + 0x60) as *mut LobbyMsg);
    }
    let old = OLD_LOBBY_PRINTS.load(Ordering::Acquire);
    context.Rcx = old;
    context.Rbx = old;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> Mutex<Friends> {
        Mutex::new(Friends {
            next: 0,
            xuids: HashSet::from([7]),
            refreshing: false,
        })
    }

    #[test]
    fn friend_refresh_allows_reentrant_and_concurrent_readers() {
        let cache = cache();
        assert!(cached_friend(&cache, 9, 10, true, || {
            assert!(
                cache.try_lock().is_ok(),
                "must release the lock before Steam"
            );
            assert!(cached_friend(&cache, 7, 10, true, || panic!(
                "recursive refresh"
            )));
            assert!(!cached_friend(&cache, 9, 10, true, || panic!(
                "recursive refresh"
            )));
            std::thread::scope(|scope| {
                assert!(scope
                    .spawn(|| cached_friend(&cache, 7, 10, true, || panic!("concurrent refresh")))
                    .join()
                    .unwrap());
            });
            Some((HashSet::from([9]), 20))
        }));
        assert!(!cached_friend(&cache, 7, 21, true, || panic!(
            "premature refresh"
        )));
        assert!(cached_friend(&cache, 9, 21, true, || panic!(
            "premature refresh"
        )));
        assert_eq!(cache.lock().unwrap().next, 30020);
    }

    #[test]
    fn failed_friend_refresh_preserves_snapshot_and_can_retry() {
        let cache = cache();
        assert!(cached_friend(&cache, 7, 10, true, || None));
        assert!(!cache.lock().unwrap().refreshing);
        assert!(cached_friend(&cache, 7, 10, false, || panic!(
            "in-game refresh"
        )));
        assert!(cached_friend(&cache, 9, 11, true, || Some((
            HashSet::from([9]),
            11
        ))));
        assert!(!cached_friend(&cache, 7, 12, true, || panic!(
            "premature refresh"
        )));
    }
}
