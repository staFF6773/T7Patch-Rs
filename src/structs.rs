//! Integer discriminants and byte flags permit malformed network values without Rust enum/bool UB.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NetAdr {
    pub ipv4: [u8; 4],
    pub port: u16,
    pub pad: u16,
    pub kind: i32,
    pub local_net_id: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LobbySession {
    pub module: i32,
    pub kind: i32,
    pub mode: i32,
    pub pad: [u8; 0x34],
    pub active: i32,
    pub pad2: [u8; 0x121d4],
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct JoinPartyMember {
    pub xuid: u64,
    pub lobby_id: u64,
    pub skill: f32,
    pub variance: f32,
    pub probation: [i32; 2],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FixedClientInfo {
    pub xuid: u64,
    pub gamertag: [u8; 32],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SessionInfo {
    pub in_session: u8,
    pub pad: [u8; 7],
    pub address: NetAdr,
    pub last_message: i64,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ActiveClient {
    pub pad: [u8; 0x410],
    pub fixed: FixedClientInfo,
    pub sessions: [SessionInfo; 2],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SessionClient {
    pub pad: [u8; 8],
    pub active: *mut ActiveClient,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Msg {
    pub overflowed: u8,
    pub read_only: u8,
    pub pad1: i16,
    pub pad2: i32,
    pub data: *mut u8,
    pub split_data: *mut u8,
    pub max_size: u32,
    pub cur_size: u32,
    pub split_size: u32,
    pub read_count: u32,
    pub bit: i32,
    pub last_entity: i32,
    pub flush: i32,
    pub target: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LobbyMsg {
    pub msg: Msg,
    pub msg_type: i32,
    pub encode_flags: u8,
    pub package_type: i32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct InfoResponseLobby {
    pub valid: u8,
    pub pad: [u8; 7],
    pub host_xuid: u64,
    pub host_name: [u8; 32],
    pub security_id: u64,
    pub security_key: [u8; 16],
    pub address_valid: u8,
    pub address: [u8; 37],
    pub pad3: [u8; 2],
    pub network_mode: i32,
    pub main_mode: i32,
    pub ugc_name: [u8; 32],
    pub ugc_version: i32,
    pub pad4: [u8; 4],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct InfoResponse {
    pub nonce: i32,
    pub ui_screen: i32,
    pub nat_type: u8,
    pub pad: [u8; 7],
    pub lobby: [InfoResponseLobby; 2],
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};
    #[test]
    fn game_abi() {
        assert_eq!(size_of::<NetAdr>(), 16);
        assert_eq!(size_of::<JoinPartyMember>(), 32);
        assert_eq!(size_of::<Msg>(), 0x38);
        assert_eq!(offset_of!(Msg, data), 8);
        assert_eq!(offset_of!(Msg, read_count), 0x24);
        assert_eq!(offset_of!(LobbyMsg, msg_type), 0x38);
        assert_eq!(offset_of!(LobbyMsg, package_type), 0x40);
        assert_eq!(size_of::<LobbyMsg>(), 0x48);
        assert_eq!(align_of::<LobbyMsg>(), 8);
        assert_eq!(offset_of!(ActiveClient, fixed), 0x410);
        assert_eq!(offset_of!(InfoResponseLobby, network_mode), 0x70);
        assert_eq!(size_of::<InfoResponseLobby>(), 0xa0);
        assert_eq!(offset_of!(InfoResponse, lobby), 0x10);
        assert_eq!(size_of::<InfoResponse>(), 0x150);
    }
}
