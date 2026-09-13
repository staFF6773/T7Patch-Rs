//! Versioned, pointer-free request used by a Windows thread entry point in the DLL.
pub const API_VERSION: u32 = 1;
pub const WAITING: u32 = 1;
pub const ACTIVE: u32 = 2;
pub const UNSUPPORTED: u32 = 3;
pub const FAILED: u32 = 4;
pub const DEACTIVATED: u32 = 5;
pub const BAD_REQUEST: u32 = 6;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StartRequest {
    pub size: u32,
    pub version: u32,
    pub status: u32,
    pub reserved: u32,
    pub config_path: [u16; 1024],
    pub message: [u16; 256],
}
impl Default for StartRequest {
    fn default() -> Self {
        Self {
            size: size_of::<Self>() as u32,
            version: API_VERSION,
            status: 0,
            reserved: 0,
            config_path: [0; 1024],
            message: [0; 256],
        }
    }
}
impl StartRequest {
    pub fn reply(&mut self, status: u32, message: &str) {
        self.status = status;
        self.message.fill(0);
        for (slot, character) in self.message[..255].iter_mut().zip(message.encode_utf16()) {
            *slot = character;
        }
    }
    pub fn text(&self) -> String {
        String::from_utf16_lossy(
            &self.message[..self.message.iter().position(|&c| c == 0).unwrap_or(256)],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_abi_and_bounded_reply() {
        assert_eq!(size_of::<StartRequest>(), 2576);
        assert_eq!(std::mem::offset_of!(StartRequest, config_path), 16);
        assert_eq!(std::mem::offset_of!(StartRequest, message), 2064);
        let mut request = StartRequest::default();
        request.reply(FAILED, &"x".repeat(1000));
        assert_eq!(request.text().len(), 255);
        assert_eq!(request.message[255], 0);
        assert_eq!(request.status, FAILED);
    }
}
