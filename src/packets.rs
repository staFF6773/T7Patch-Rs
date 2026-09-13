//! Bounded inspection of a copy of the game's lobby reader. The original cursor is never consumed.
use crate::{
    config, memory,
    network_guard::{self, Event},
    protection,
    structs::{LobbyMsg, Msg},
};
use std::{ffi::CStr, sync::Mutex};

pub(crate) const LOBBY_HANDLE_IM_RVA: usize = 0x1EEA130;
// Existing IM reader: two header bytes followed by strictly fewer than 2048 bytes.
pub(crate) const MAX_INSTANT_BYTES: u32 = 2049;

/// Validate arithmetic before accessing either payload buffer. BO3's native
/// readers use signed 32-bit lengths, despite the unsigned ABI fields here.
fn reader_layout(msg: &Msg) -> std::result::Result<u32, ReaderFault> {
    if msg.overflowed != 0 {
        return Err(ReaderFault::Overflowed);
    }
    if msg.cur_size > msg.max_size || msg.max_size > i32::MAX as u32 {
        return Err(ReaderFault::Capacity);
    }
    let total = msg
        .cur_size
        .checked_add(msg.split_size)
        .ok_or(ReaderFault::TotalLength)?;
    if total > i32::MAX as u32 {
        return Err(ReaderFault::TotalLength);
    }
    if msg.bit < 0 || msg.bit as u64 > u64::from(total) * 8 {
        return Err(ReaderFault::BitCursor);
    }
    if (msg.cur_size != 0 && msg.data.is_null())
        || (msg.split_size != 0 && msg.split_data.is_null())
    {
        return Err(ReaderFault::MissingBuffer);
    }
    (msg.data as usize)
        .checked_add(msg.cur_size as usize)
        .ok_or(ReaderFault::AddressOverflow)?;
    (msg.split_data as usize)
        .checked_add(msg.split_size as usize)
        .ok_or(ReaderFault::AddressOverflow)?;
    total
        .checked_sub(msg.read_count)
        .ok_or(ReaderFault::ReadCursor)
}

pub(crate) fn remaining_bytes(msg: &Msg) -> Option<u32> {
    reader_layout(msg).ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReaderFault {
    DescriptorUnreadable,
    Overflowed,
    Capacity,
    TotalLength,
    BitCursor,
    MissingBuffer,
    AddressOverflow,
    ReadCursor,
    DataUnreadable,
    SplitUnreadable,
}
const READER_FAULT_COUNT: usize = 10;

#[derive(Clone, Copy)]
enum ReaderStage {
    Guarded,
    Connectionless,
}
impl ReaderStage {
    fn label(self) -> &'static str {
        match self {
            Self::Guarded => "guarded-reader",
            Self::Connectionless => "connectionless-post-command",
        }
    }
}

// Scalar-only metadata: do not retain game pointers, identifiers or packet contents.
#[derive(Clone, Copy)]
struct ReaderMetadata {
    overflowed: u8,
    capacity: u32,
    current: u32,
    split: u32,
    read: u32,
    bit: i32,
}
impl From<&Msg> for ReaderMetadata {
    fn from(msg: &Msg) -> Self {
        Self {
            overflowed: msg.overflowed,
            capacity: msg.max_size,
            current: msg.cur_size,
            split: msg.split_size,
            read: msg.read_count,
            bit: msg.bit,
        }
    }
}
#[derive(Clone, Copy)]
struct ReaderDiagnostic {
    stage: ReaderStage,
    fault: ReaderFault,
    command: &'static str,
    metadata: Option<ReaderMetadata>,
}

// At most one coherent snapshot per reason and stage in each report interval.
static READER_DIAGNOSTICS: [Mutex<Option<ReaderDiagnostic>>; READER_FAULT_COUNT * 2] =
    [const { Mutex::new(None) }; READER_FAULT_COUNT * 2];

fn record_reader(stage: ReaderStage, command: &'static str, fault: ReaderFault, msg: Option<&Msg>) {
    let slot = &READER_DIAGNOSTICS[stage as usize * READER_FAULT_COUNT + fault as usize];
    if let Ok(mut sample) = slot.try_lock() {
        if sample.is_none() {
            *sample = Some(ReaderDiagnostic {
                stage,
                fault,
                command,
                metadata: msg.map(ReaderMetadata::from),
            });
        }
    }
}

fn check_reader(msg: &Msg) -> std::result::Result<u32, ReaderFault> {
    let remaining = reader_layout(msg)?;
    if msg.cur_size != 0 && !memory::readable(msg.data as usize, msg.cur_size as usize) {
        return Err(ReaderFault::DataUnreadable);
    }
    if msg.split_size != 0 && !memory::readable(msg.split_data as usize, msg.split_size as usize) {
        return Err(ReaderFault::SplitUnreadable);
    }
    Ok(remaining)
}

/// CL_ConnectionlessCMD runs AFTER native command tokenization. Its reader state
/// must not be gated by assumptions made for readers we are about to consume.
/// Observe the old predicate to diagnose the startup regression, without changing
/// the descriptor or bypassing the command allowlist and control-rate policies.
pub(crate) fn observe_connectionless(msg: Option<&Msg>, command: &'static str) {
    let fault = msg.map_or(Some(ReaderFault::DescriptorUnreadable), |msg| {
        check_reader(msg).err()
    });
    if let Some(fault) = fault {
        record_reader(ReaderStage::Connectionless, command, fault, msg);
    }
}

pub(crate) fn report_reader_diagnostics() {
    for slot in &READER_DIAGNOSTICS {
        // Release the lock before any journal I/O.
        let sample = slot.lock().ok().and_then(|mut sample| sample.take());
        if let Some(sample) = sample {
            if let Some(msg) = sample.metadata {
                crate::diagnostics::event(format_args!(
                    "reader-diagnostic stage={} action={} reason={:?} command={} overflowed={} capacity={} cursize={} split={} readcount={} bit={}",
                    sample.stage.label(), if matches!(sample.stage, ReaderStage::Connectionless) { "observe" } else { "reject" },
                    sample.fault, sample.command, msg.overflowed, msg.capacity, msg.current, msg.split, msg.read, msg.bit,
                ));
            } else {
                crate::diagnostics::event(format_args!(
                    "reader-diagnostic stage={} action=observe reason={:?} command={} metadata=unavailable",
                    sample.stage.label(), sample.fault, sample.command,
                ));
            }
        }
    }
}

/// The descriptor and its buffers must remain live throughout the game callback.
/// Split readers are supported rather than rejecting legitimate fragmented data.
pub(crate) fn valid_message(msg: &Msg) -> bool {
    match check_reader(msg) {
        Ok(_) => true,
        Err(fault) => {
            network_guard::record(Event::Envelope);
            record_reader(ReaderStage::Guarded, "n/a", fault, Some(msg));
            false
        }
    }
}
// Verified from the live September2026 call at LobbyMsg_HandleIM+0x20:
// E8 9B 03 00 00 -> RVA 0x1EE9E30. In the baseline address map this is
// 0x1EEA4F0, NOT 0x1EEA150 (which is the call instruction itself).
const LOBBY_PREP_READ_DATA_RVA: usize = 0x1EEA4F0;

const HANDLE_IM_PREFIX: &[u8] = &[
    0x48, 0x89, 0x5c, 0x24, 0x08, 0x57, 0x48, 0x81, 0xec, 0x80, 0x00, 0x00, 0x00, 0x49, 0x8b, 0xc0,
    0x8b, 0xf9, 0x48, 0x8b, 0xda, 0x48, 0x8d, 0x4c, 0x24, 0x30, 0x45, 0x8b, 0xc1, 0x48, 0x8b, 0xd0,
];
const PREP_READ_DATA_PREFIX: &[u8] = &[
    0x48, 0x89, 0x5c, 0x24, 0x08, 0x57, 0x48, 0x83, 0xec, 0x20, 0x41, 0x8b, 0xf8, 0x48, 0x8b, 0xd9,
];
const PREP_READ_DATA_EPILOGUE: &[u8] = &[
    0x48, 0x8b, 0xcb, 0x89, 0x7b, 0x1c, 0x48, 0x8b, 0x5c, 0x24, 0x30, 0x48, 0x83, 0xc4, 0x20, 0x5f,
];

fn relative_target(code: &[u8], base: usize, offset: usize, opcode: u8) -> Option<usize> {
    let instruction = code.get(offset..offset.checked_add(5)?)?;
    if instruction[0] != opcode {
        return None;
    }
    let displacement = i32::from_le_bytes(instruction[1..5].try_into().ok()?);
    base.checked_add(offset)?
        .checked_add(5)?
        .checked_add_signed(displacement as isize)
}

fn reader_code_matches(
    handler_code: &[u8],
    handler: usize,
    prepare_code: &[u8],
    prepare: usize,
    initialize: usize,
    read_message: usize,
) -> bool {
    handler_code.starts_with(HANDLE_IM_PREFIX)
        && relative_target(handler_code, handler, 0x20, 0xe8) == Some(prepare)
        && prepare_code.starts_with(PREP_READ_DATA_PREFIX)
        && relative_target(prepare_code, prepare, 0x10, 0xe8) == Some(initialize)
        && prepare_code.get(0x15..0x25) == Some(PREP_READ_DATA_EPILOGUE)
        && relative_target(prepare_code, prepare, 0x25, 0xe9) == Some(read_message)
}

/// Validate the native call relationship before any game hooks are enabled.
/// The helper initializes Msg, publishes cursize, then tail-calls PrepReadMsg.
pub unsafe fn validate_message_reader() -> std::result::Result<(), std::string::String> {
    use crate::game_build::address;
    let handler = address(LOBBY_HANDLE_IM_RVA);
    let prepare = address(LOBBY_PREP_READ_DATA_RVA);
    if !memory::readable(handler, 0x25) || !memory::readable(prepare, 0x2a) {
        return Err("Lobby message-reader code is not readable".into());
    }
    if !reader_code_matches(
        std::slice::from_raw_parts(handler as *const u8, 0x25),
        handler,
        std::slice::from_raw_parts(prepare as *const u8, 0x2a),
        prepare,
        address(0x20FCB80),
        address(0x1EEB8D0),
    ) {
        return Err(
            "Lobby message-reader signature mismatch; refusing to call an unverified entry point"
                .into(),
        );
    }
    crate::diagnostics::event(format_args!(
        "message-reader-validated handler={handler:#x} prepare_read_data={prepare:#x}"
    ));
    Ok(())
}

#[derive(Clone, Copy)]
enum Kind {
    Int,
    UInt,
    Byte,
    Bool,
    Short,
    U64,
    Xuid,
    Float,
    String(usize),
    Glob(usize),
}
use Kind::*;
type Result<T = ()> = std::result::Result<T, ()>;

trait Wire {
    fn field(&mut self, kind: Kind, key: &CStr) -> Result<u64>;
    fn array(&mut self, key: &CStr) -> Result;
    fn element(&mut self, expected: bool) -> bool;
    fn mutable_client(&mut self) -> Result;
}
struct Engine {
    msg: *mut LobbyMsg,
}
impl Wire for Engine {
    fn field(&mut self, kind: Kind, key: &CStr) -> Result<u64> {
        let mut scratch = [0u64; 128];
        let (rva, size) = match kind {
            Int => (0x1EEA3E0, 0),
            UInt => (0x1EEA490, 0),
            Byte => (0x1EEA450, 0),
            Bool => (0x1EEA2E0, 0),
            Short => (0x1EEA400, 0),
            U64 => (0x1EEA470, 0),
            Xuid => (0x1EEA4D0, 0),
            Float => (0x1EEA390, 0),
            String(size) => (0x1EEA420, size),
            Glob(size) => (0x1EEA3B0, size),
        };
        if size > size_of_val(&scratch) {
            return Err(());
        }
        let ok = unsafe {
            if size == 0 {
                game_fn!(
                    rva,
                    unsafe extern "C" fn(*mut LobbyMsg, *const i8, *mut u64) -> u8
                )(self.msg, key.as_ptr(), scratch.as_mut_ptr())
            } else {
                game_fn!(
                    rva,
                    unsafe extern "C" fn(*mut LobbyMsg, *const i8, *mut u64, i32) -> u8
                )(self.msg, key.as_ptr(), scratch.as_mut_ptr(), size as i32)
            }
        };
        if ok == 0 {
            Err(())
        } else {
            Ok(scratch[0])
        }
    }
    fn array(&mut self, key: &CStr) -> Result {
        if unsafe {
            game_fn!(
                0x1EEA260,
                unsafe extern "C" fn(*mut LobbyMsg, *const i8) -> u8
            )(self.msg, key.as_ptr())
        } == 0
        {
            Err(())
        } else {
            Ok(())
        }
    }
    fn element(&mut self, expected: bool) -> bool {
        unsafe {
            game_fn!(0x1EEA320, unsafe extern "C" fn(*mut LobbyMsg, i32) -> u8)(
                self.msg,
                expected as i32,
            ) != 0
        }
    }
    fn mutable_client(&mut self) -> Result {
        let mut output = [0u64; 256];
        if unsafe {
            game_fn!(
                0x1EC8400,
                unsafe extern "C" fn(*mut u64, *mut LobbyMsg) -> u8
            )(output.as_mut_ptr(), self.msg)
        } == 0
        {
            Err(())
        } else {
            Ok(())
        }
    }
}

fn fields(w: &mut impl Wire, fields: &[(Kind, &CStr)]) -> Result {
    for &(kind, key) in fields {
        w.field(kind, key)?;
    }
    Ok(())
}
fn count(w: &mut impl Wire, key: &CStr, max: i32) -> Result<usize> {
    let n = w.field(Int, key)? as i32;
    if !(0..=max).contains(&n) {
        Err(())
    } else {
        Ok(n as usize)
    }
}
fn items<W: Wire>(
    w: &mut W,
    limit: usize,
    exact: bool,
    mut inspect: impl FnMut(&mut W) -> Result,
) -> Result {
    let mut next = w.element(limit > 0);
    for i in 0..limit {
        if !next {
            return if exact { Err(()) } else { Ok(()) };
        }
        inspect(w)?;
        next = w.element(i + 1 < limit);
    }
    if next {
        Err(())
    } else {
        Ok(())
    }
}

fn join(w: &mut impl Wire) -> Result {
    fields(
        w,
        &[
            (Int, c"targetlobby"),
            (Int, c"sourcelobby"),
            (Int, c"jointype"),
            (Xuid, c"probedxuid"),
            (Int, c"playlistid"),
            (Int, c"playlistver"),
            (Int, c"ffotdver"),
            (Short, c"networkmode"),
            (UInt, c"netchecksum"),
            (Int, c"protocol"),
            (Int, c"changelist"),
            (Int, c"pingband"),
            (UInt, c"dlcbits"),
            (U64, c"joinnonce"),
            (Byte, c"chunk"),
            (Byte, c"chunk"),
            (Byte, c"chunk"),
            (Bool, c"isStarterPack"),
            (String(32), c"password"),
        ],
    )?;
    let count = count(w, c"membercount", 18)?;
    w.array(c"members")?;
    items(w, count, true, |w| {
        fields(
            w,
            &[
                (Xuid, c"xuid"),
                (U64, c"lobbyid"),
                (Float, c"skillrating"),
                (Float, c"skillvariance"),
                (UInt, c"pprobation"),
                (UInt, c"aprobation"),
            ],
        )
    })
}

fn lobby(w: &mut impl Wire) -> Result<(i32, i32)> {
    fields(w, &[(Int, c"statenum"), (Int, c"networkmode")])?;
    let main_mode = w.field(Int, c"mainmode")? as i32;
    fields(w, &[(Int, c"partyprivacy"), (Int, c"lobbytype")])?;
    let lobby_mode = w.field(Int, c"lobbymode")? as i32;
    fields(
        w,
        &[
            (Int, c"sessionstatus"),
            (Int, c"uiscreen"),
            (Byte, c"leaderactivity"),
            (String(32), c"key"),
            (Xuid, c"leader"),
            (Xuid, c"platformsession"),
            (Int, c"maxclients"),
            (Bool, c"isadvertised"),
        ],
    )?;
    let clients = count(w, c"clientcount", 18)?;
    fields(
        w,
        &[
            (String(64), c"sessionid"),
            (String(64), c"sessioninfo"),
            (String(32), c"ugcName"),
            (UInt, c"ugcVersion"),
        ],
    )?;
    w.array(c"clientlist")?;
    items(w, clients, true, |w| {
        fields(
            w,
            &[
                (Xuid, c"xuid"),
                (Byte, c"clientNum"),
                (String(32), c"gamertag"),
                (Bool, c"isGuest"),
                (U64, c"lobbyid"),
                (Int, c"connectbit"),
                (Int, c"score"),
                (Glob(37), c"address"),
                (Int, c"qport"),
                (Byte, c"band"),
                (UInt, c"netsrc"),
                (UInt, c"joinorder"),
                (UInt, c"dlcBits"),
            ],
        )?;
        w.mutable_client()
    })?;
    fields(w, &[(Byte, c"migratebits"), (Int, c"lasthosttimems")])?;
    w.array(c"nomineelist")?;
    items(w, 18, false, |w| w.field(Xuid, c"xuid").map(|_| ()))?;
    Ok((main_mode, lobby_mode))
}

fn vote(w: &mut impl Wire) -> Result {
    fields(
        w,
        &[
            (Short, c"itemtype"),
            (UInt, c"item"),
            (Short, c"itemgroup"),
            (Short, c"attachment"),
            (Short, c"votetype"),
            (Xuid, c"votexuid"),
        ],
    )
}
fn pregame(w: &mut impl Wire) -> Result {
    w.array(c"clientlist")?;
    items(w, 18, false, |w| {
        fields(
            w,
            &[
                (Int, c"team"),
                (Int, c"pregamepos"),
                (Int, c"pregamestate"),
                (UInt, c"clvotecount"),
                (UInt, c"character"),
                (UInt, c"loadout"),
            ],
        )
    })
}
fn lobby_game(w: &mut impl Wire) -> Result {
    let (main_mode, lobby_mode) = lobby(w)?;
    fields(
        w,
        &[
            (Int, c"serverstatus"),
            (Int, c"launchnonce"),
            (U64, c"matchhashlow"),
            (U64, c"matchhashhigh"),
            (Int, c"status"),
            (Int, c"statusvalue"),
            (Int, c"gamemode"),
            (String(32), c"gametype"),
            (String(32), c"map"),
            (String(32), c"ugcName"),
            (UInt, c"ugcVersion"),
        ],
    )?;
    if main_mode == 0 {
        fields(
            w,
            &[(String(32), c"cpqueuedlevel"), (Bool, c"movieskipped")],
        )?;
    }
    match lobby_mode {
        0 => {
            w.array(c"clientlist")?;
            items(w, 18, false, |w| {
                fields(w, &[(Int, c"team"), (Int, c"mapvote"), (Bool, c"readyup")])
            })?;
            fields(
                w,
                &[
                    (Int, c"plistid"),
                    (Int, c"plistcurr"),
                    (Glob(8), c"plistentries"),
                    (Byte, c"plistnext"),
                    (Byte, c"plistprev"),
                    (Byte, c"plistprevcount"),
                ],
            )?;
        }
        1 => {
            let votes = count(w, c"votecount", 216)?;
            w.array(c"votes")?;
            items(w, votes, true, vote)?;
            pregame(w)?;
            let size = w.field(Int, c"settingssize")? as i32;
            if !(1..=0xc000).contains(&size) {
                return Err(());
            }
        }
        3 => {
            w.field(Int, c"compstate")?;
            let votes = count(w, c"votecount", 216)?;
            // The arena serializer has no PackageArrayStart("votes") in the supplied source.
            items(w, votes, true, vote)?;
            pregame(w)?;
        }
        _ => {}
    }
    Ok(())
}

fn heartbeat(w: &mut impl Wire) -> Result {
    fields(
        w,
        &[
            (Int, c"heartbeatnum"),
            (Int, c"lobbytype"),
            (Int, c"lasthosttimems"),
        ],
    )?;
    w.array(c"nomineelist")?;
    items(w, 18, false, |w| w.field(Xuid, c"xuid").map(|_| ()))
}
fn info_response(w: &mut impl Wire) -> Result {
    fields(
        w,
        &[(UInt, c"nonce"), (Int, c"uiscreen"), (Byte, c"nattype")],
    )?;
    w.array(c"lobbies")?;
    items(w, 2, false, |w| {
        w.array(c"lobbies")?;
        items(w, 2, false, |w| {
            if w.field(Bool, c"valid")? == 0 {
                return Ok(());
            }
            fields(
                w,
                &[
                    (Xuid, c"hostxuid"),
                    (String(32), c"hostname"),
                    (Int, c"networkmode"),
                    (Int, c"mainmode"),
                    (Glob(8), c"secid"),
                    (Glob(16), c"seckey"),
                    (Glob(37), c"addrbuff"),
                    (String(32), c"ugcName"),
                    (UInt, c"ugcVersion"),
                ],
            )
        })
    })
}

pub unsafe fn inspect(msg: *mut LobbyMsg) {
    if !memory::readable(msg as usize, size_of::<LobbyMsg>()) {
        return;
    }
    let mut copy = msg.read_unaligned();
    if !valid_message(&copy.msg) {
        std::ptr::addr_of_mut!((*msg).msg_type).write_unaligned(0xff);
        return;
    }
    let kind = copy.msg_type;
    let mut engine = Engine { msg: &mut copy };
    let result = match kind {
        0x10 => join(&mut engine),
        2 => lobby(&mut engine).map(|_| ()),
        3 => lobby_game(&mut engine),
        7 => heartbeat(&mut engine),
        0xf | 0x1e | 0x16 | 0x17 | 0x18 => Err(()),
        _ => return,
    };
    if result.is_err() || copy.msg.overflowed != 0 {
        network_guard::record(Event::Lobby);
        std::ptr::addr_of_mut!((*msg).msg_type).write_unaligned(0xff);
    }
}

pub unsafe fn check_pending_info(sender: u64, msg: &Msg) -> bool {
    if !valid_message(msg) {
        return true;
    }
    let mut copy = *msg;
    let Some(size) = remaining_bytes(&copy).filter(|&size| size < 2048) else {
        return true;
    };
    let mut data = [0u8; 2048];
    game_fn!(0x20FD0B0, unsafe extern "C" fn(*mut Msg, *mut u8, i32))(
        &mut copy,
        data.as_mut_ptr(),
        size as i32,
    );
    if copy.overflowed != 0 {
        return true;
    }
    let mut lobby = LobbyMsg::default();
    if game_fn!(
        LOBBY_PREP_READ_DATA_RVA,
        unsafe extern "C" fn(*mut LobbyMsg, *mut u8, i32) -> u8
    )(&mut lobby, data.as_mut_ptr(), size as i32)
        == 0
    {
        return true;
    }
    if !valid_message(&lobby.msg) {
        return true;
    }
    if lobby.msg_type == 1 {
        if info_response(&mut Engine { msg: &mut lobby }).is_err() || lobby.msg.overflowed != 0 {
            return true;
        }
        // Pass an actual msg_t, rather than casting the input packet bytes to a msg_t.
        let mut original_copy = *msg;
        game_fn!(0x143A7A0, unsafe extern "C" fn(u64, i32, *mut Msg) -> u8)(
            sender,
            0,
            &mut original_copy,
        );
        return true;
    }
    if sender == 0
        || sender == protection::own_xuid()
        || sender & 0xFFFF00000000000 != 0x110000000000000
    {
        return false;
    }
    config::FRIENDS_ONLY.load(std::sync::atomic::Ordering::Acquire)
        && !protection::is_friend(sender)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_diagnostics_distinguish_flags_capacity_and_consumed_cursors() {
        let mut data = [0u8; 8];
        let msg = Msg {
            data: data.as_mut_ptr(),
            cur_size: 8,
            max_size: 8,
            ..Msg::default()
        };
        for (changed, reason) in [
            (
                Msg {
                    overflowed: 1,
                    read_count: 9,
                    ..msg
                },
                ReaderFault::Overflowed,
            ),
            (Msg { max_size: 0, ..msg }, ReaderFault::Capacity),
            (
                Msg {
                    read_count: 9,
                    ..msg
                },
                ReaderFault::ReadCursor,
            ),
            (Msg { bit: 65, ..msg }, ReaderFault::BitCursor),
            (
                Msg {
                    data: std::ptr::null_mut(),
                    ..msg
                },
                ReaderFault::MissingBuffer,
            ),
        ] {
            assert_eq!(reader_layout(&changed), Err(reason));
            let snapshot = ReaderMetadata::from(&changed);
            assert_eq!(snapshot.capacity, changed.max_size);
            assert_eq!(snapshot.read, changed.read_count);
            assert_eq!(snapshot.bit, changed.bit);
            assert_eq!(snapshot.overflowed, changed.overflowed);
        }
    }

    #[test]
    fn split_reader_bounds_and_empty_messages() {
        let mut first = [0u8; 4];
        let mut second = [0u8; 8];
        let mut msg = Msg {
            data: first.as_mut_ptr(),
            split_data: second.as_mut_ptr(),
            max_size: 4,
            cur_size: 4,
            split_size: 8,
            read_count: 6,
            bit: 48,
            ..Msg::default()
        };
        assert_eq!(remaining_bytes(&msg), Some(6));
        assert!(valid_message(&msg));
        msg.read_count = 12;
        msg.bit = 96;
        assert_eq!(remaining_bytes(&msg), Some(0));
        msg.read_count = 13;
        assert_eq!(remaining_bytes(&msg), None);
        assert_eq!(remaining_bytes(&Msg::default()), Some(0));
    }

    #[test]
    fn malformed_envelopes_are_rejected_before_native_readers() {
        let mut data = [0u8; 16];
        let good = Msg {
            data: data.as_mut_ptr(),
            max_size: 16,
            cur_size: 16,
            ..Msg::default()
        };
        let cases = [
            Msg {
                overflowed: 1,
                ..good
            },
            Msg {
                cur_size: 17,
                ..good
            },
            Msg {
                read_count: 17,
                ..good
            },
            Msg { bit: -1, ..good },
            Msg { bit: 129, ..good },
            Msg {
                max_size: u32::MAX,
                ..good
            },
            Msg {
                data: std::ptr::null_mut(),
                ..good
            },
            Msg {
                data: usize::MAX as *mut u8,
                ..good
            },
            Msg {
                split_size: 1,
                ..good
            },
            Msg {
                split_size: u32::MAX,
                split_data: data.as_mut_ptr(),
                ..good
            },
            Msg {
                split_size: i32::MAX as u32,
                split_data: data.as_mut_ptr(),
                ..good
            },
        ];
        for msg in cases {
            assert_eq!(remaining_bytes(&msg), None);
            let mut lobby = LobbyMsg {
                msg,
                msg_type: 0x10,
                ..LobbyMsg::default()
            };
            unsafe {
                inspect(&mut lobby);
            }
            assert_eq!(lobby.msg_type, 0xff);
            assert!(unsafe { check_pending_info(7, &msg) });
        }
        // Structurally valid but inaccessible data must also be rejected before game_fn!.
        let msg = Msg {
            data: std::ptr::dangling_mut::<u8>(),
            ..good
        };
        assert!(remaining_bytes(&msg).is_some());
        assert!(!valid_message(&msg));
    }

    #[test]
    fn unhandled_message_types_keep_original_cursor_and_payload() {
        let mut data = [1u8, 2, 3, 4];
        let mut lobby = LobbyMsg {
            msg: Msg {
                data: data.as_mut_ptr(),
                max_size: 4,
                cur_size: 4,
                read_count: 1,
                bit: 8,
                ..Msg::default()
            },
            msg_type: 4,
            ..LobbyMsg::default()
        };
        unsafe {
            inspect(&mut lobby);
        }
        assert_eq!(lobby.msg_type, 4);
        assert_eq!(lobby.msg.read_count, 1);
        assert_eq!(lobby.msg.bit, 8);
        assert_eq!(data, [1, 2, 3, 4]);
    }

    struct Truncated {
        fail_at: usize,
        calls: usize,
    }
    impl Truncated {
        fn read(&mut self) -> Result<u64> {
            let position = self.calls;
            self.calls += 1;
            if position == self.fail_at {
                Err(())
            } else {
                Ok(0)
            }
        }
    }
    impl Wire for Truncated {
        fn field(&mut self, _: Kind, _: &CStr) -> Result<u64> {
            self.read()
        }
        fn array(&mut self, _: &CStr) -> Result {
            self.read().map(|_| ())
        }
        fn element(&mut self, _: bool) -> bool {
            false
        }
        fn mutable_client(&mut self) -> Result {
            self.read().map(|_| ())
        }
    }
    #[test]
    fn truncated_fields_stop_inspection_immediately() {
        type Inspector = fn(&mut Truncated) -> Result;
        let inspectors: [(Inspector, usize); 3] = [(join, 21), (heartbeat, 4), (info_response, 4)];
        for (inspect, calls) in inspectors {
            let mut valid = Truncated {
                fail_at: usize::MAX,
                calls: 0,
            };
            assert!(inspect(&mut valid).is_ok());
            assert_eq!(valid.calls, calls);
            for fail_at in 0..calls {
                let mut wire = Truncated { fail_at, calls: 0 };
                assert!(inspect(&mut wire).is_err());
                assert_eq!(wire.calls, fail_at + 1);
            }
        }
    }
    // Captured with ReadProcessMemory from the user's September2026 executable,
    // not reconstructed from the port's old address constants.
    const HANDLE_IM_CAPTURE: &[u8] = &[
        0x48, 0x89, 0x5c, 0x24, 0x08, 0x57, 0x48, 0x81, 0xec, 0x80, 0x00, 0x00, 0x00, 0x49, 0x8b,
        0xc0, 0x8b, 0xf9, 0x48, 0x8b, 0xda, 0x48, 0x8d, 0x4c, 0x24, 0x30, 0x45, 0x8b, 0xc1, 0x48,
        0x8b, 0xd0, 0xe8, 0x9b, 0x03, 0x00, 0x00,
    ];
    const PREPARE_CAPTURE: &[u8] = &[
        0x48, 0x89, 0x5c, 0x24, 0x08, 0x57, 0x48, 0x83, 0xec, 0x20, 0x41, 0x8b, 0xf8, 0x48, 0x8b,
        0xd9, 0xe8, 0x7b, 0x26, 0x21, 0x00, 0x48, 0x8b, 0xcb, 0x89, 0x7b, 0x1c, 0x48, 0x8b, 0x5c,
        0x24, 0x30, 0x48, 0x83, 0xc4, 0x20, 0x5f, 0xe9, 0xb6, 0x13, 0x00, 0x00,
    ];

    #[test]
    fn message_reader_matches_live_call_graph_and_rejects_old_call_site() {
        use crate::game_build::{translate_rva, Build};
        let handler = translate_rva(LOBBY_HANDLE_IM_RVA, Build::September2026);
        let prepare = translate_rva(LOBBY_PREP_READ_DATA_RVA, Build::September2026);
        assert_eq!(handler, 0x1ee9a70);
        assert_eq!(prepare, 0x1ee9e30);
        for base in [0, 0x7ff6ee2b0000usize] {
            assert!(reader_code_matches(
                HANDLE_IM_CAPTURE,
                base + handler,
                PREPARE_CAPTURE,
                base + prepare,
                base + 0x20fc4c0,
                base + 0x1eeb210
            ));
            assert!(!reader_code_matches(
                HANDLE_IM_CAPTURE,
                base + handler,
                PREPARE_CAPTURE,
                base + handler + 0x20,
                base + 0x20fc4c0,
                base + 0x1eeb210
            ));
        }
        // The old baseline 0x1EEA150 is an E8 call inside HandleIM, not a function.
        assert_eq!(
            translate_rva(0x1EEA150, Build::September2026),
            handler + 0x20
        );
        assert_eq!(HANDLE_IM_CAPTURE[0x20], 0xe8);
        let mut corrupt = PREPARE_CAPTURE.to_vec();
        corrupt[0x25] = 0xc3;
        assert!(!reader_code_matches(
            HANDLE_IM_CAPTURE,
            handler,
            &corrupt,
            prepare,
            0x20fc4c0,
            0x1eeb210
        ));
        assert_eq!(
            relative_target(&[0xe8, 0xfb, 0xff, 0xff, 0xff], 0x1000, 0, 0xe8),
            Some(0x1000)
        );
        assert_eq!(relative_target(&[0xe8], 0x1000, 0, 0xe8), None);
    }

    #[test]
    fn captured_prepare_entry_preserves_abi_and_initializes_reader_before_parsing() {
        use windows_sys::Win32::System::Memory::*;
        unsafe extern "C" fn initialize(msg: *mut Msg, data: *mut u8, length: i32) {
            msg.write(Msg {
                data,
                max_size: length as u32,
                ..Msg::default()
            });
        }
        unsafe extern "C" fn parse(lobby: *mut LobbyMsg) -> u8 {
            let msg = &mut (*lobby).msg;
            // The captured helper must publish cursize after initialize and
            // pass the same reader to this tail call.
            if msg.data.is_null() || msg.cur_size != 3 || msg.max_size != 3 || msg.read_count != 0 {
                return 0;
            }
            (*lobby).msg_type = *msg.data as i32;
            msg.read_count = 1;
            1
        }
        unsafe {
            let page = VirtualAlloc(
                std::ptr::null(),
                4096,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            );
            assert!(!page.is_null());
            struct Allocation(*mut std::ffi::c_void);
            impl Drop for Allocation {
                fn drop(&mut self) {
                    unsafe {
                        VirtualFree(self.0, 0, MEM_RELEASE);
                    }
                }
            }
            let _allocation = Allocation(page);
            let mut code = [0xcc; 256];
            code[..PREPARE_CAPTURE.len()].copy_from_slice(PREPARE_CAPTURE);
            // Only relocate the call/jump to two local absolute-jump stubs.
            code[0x11..0x15].copy_from_slice(&0x6bi32.to_le_bytes());
            code[0x26..0x2a].copy_from_slice(&0x76i32.to_le_bytes());
            for (offset, target) in [
                (0x80, initialize as *const () as usize),
                (0xa0, parse as *const () as usize),
            ] {
                code[offset..offset + 6].copy_from_slice(&[0xff, 0x25, 0, 0, 0, 0]);
                code[offset + 6..offset + 14].copy_from_slice(&target.to_le_bytes());
            }
            memory::write_bytes(page as usize, &code).unwrap();
            let mut old = 0;
            assert_ne!(VirtualProtect(page, 4096, PAGE_EXECUTE_READ, &mut old), 0);
            let prepare: unsafe extern "C" fn(*mut LobbyMsg, *mut u8, i32) -> u8 =
                std::mem::transmute(page);
            #[repr(C)]
            struct Guarded {
                before: u64,
                lobby: LobbyMsg,
                after: u64,
            }
            let mut state = Guarded {
                before: 0x12345678,
                lobby: LobbyMsg::default(),
                after: 0xabcdef01,
            };
            let mut payload = [1u8, 2, 3];
            assert_eq!(prepare(&mut state.lobby, payload.as_mut_ptr(), 3), 1);
            assert_eq!(state.lobby.msg.data, payload.as_mut_ptr());
            assert_eq!(state.lobby.msg.read_count, 1);
            assert_eq!(state.lobby.msg_type, 1);
            assert_eq!(state.before, 0x12345678);
            assert_eq!(state.after, 0xabcdef01);
        }
    }

    struct Fake {
        count: i32,
        elements: usize,
        reads: usize,
    }
    impl Wire for Fake {
        fn field(&mut self, _: Kind, key: &CStr) -> Result<u64> {
            self.reads += 1;
            Ok(if key == c"membercount" {
                self.count as u32 as u64
            } else {
                0
            })
        }
        fn array(&mut self, _: &CStr) -> Result {
            Ok(())
        }
        fn element(&mut self, _: bool) -> bool {
            if self.elements == 0 {
                false
            } else {
                self.elements -= 1;
                true
            }
        }
        fn mutable_client(&mut self) -> Result {
            Ok(())
        }
    }
    #[test]
    fn join_bounds_and_extra_elements() {
        for (count, elements, valid) in [
            (0, 0, true),
            (18, 18, true),
            (19, 19, false),
            (-1, 0, false),
            (0, 1, false),
            (1, 2, false),
            (2, 1, false),
        ] {
            let mut wire = Fake {
                count,
                elements,
                reads: 0,
            };
            assert_eq!(
                join(&mut wire).is_ok(),
                valid,
                "count={count}, elements={elements}"
            );
            if !(0..=18).contains(&count) {
                assert_eq!(wire.reads, 20);
            }
        }
    }
    #[test]
    fn heartbeat_nominee_limit() {
        for (elements, valid) in [(0, true), (18, true), (19, false)] {
            assert_eq!(
                heartbeat(&mut Fake {
                    count: 0,
                    elements,
                    reads: 0
                })
                .is_ok(),
                valid
            );
        }
    }
}
