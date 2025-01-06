use super::proto::{self, wine_bridge_server::WineBridge};
#[cfg(feature = "winebridge")]
use windows::{
    core::{s, PCSTR},
    Win32::UI::WindowsAndMessaging::{MessageBoxA, MB_OK},
};

#[derive(Debug, Default)]
pub struct WineBridgeService;

#[tonic::async_trait]
impl WineBridge for WineBridgeService {
    async fn message(
        &self,
        request: tonic::Request<proto::MessageRequest>,
    ) -> Result<tonic::Response<proto::MessageResponse>, tonic::Status> {
        let request = request.get_ref();
        println!("Got a request: {:?}", request);
        let message = request.message.as_str();
        let c_message = std::ffi::CString::new(message).unwrap();
        unsafe {
            MessageBoxA(
                None,
                PCSTR(c_message.as_ptr() as *const u8),
                s!("Hello"),
                MB_OK,
            );
        }
        Ok(tonic::Response::new(proto::MessageResponse {
            success: true,
        }))
    }
}
