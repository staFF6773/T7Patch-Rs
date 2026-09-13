//! Bounded inspection of a copy of the game's lobby reader. The original cursor is never consumed.
use crate::{
    config, memory, protection,
    structs::{LobbyMsg, Msg},
};
use std::ffi::CStr;

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
        std::ptr::addr_of_mut!((*msg).msg_type).write_unaligned(0xff);
    }
}

pub unsafe fn check_pending_info(sender: u64, msg: &Msg) -> bool {
    let mut copy = *msg;
    let Some(size) = copy
        .cur_size
        .checked_sub(copy.read_count)
        .filter(|&size| size < 2048)
    else {
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
        0x1EEA150,
        unsafe extern "C" fn(*mut LobbyMsg, *mut u8, i32) -> u8
    )(&mut lobby, data.as_mut_ptr(), size as i32)
        == 0
    {
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
