use crate::proto::{self, bottles_server::Bottles, wine_bridge_client::WineBridgeClient};

#[derive(Debug, Default)]
pub struct BottlesService;

#[tonic::async_trait]
impl Bottles for BottlesService {
    async fn health(
        &self,
        request: tonic::Request<proto::HealthRequest>,
    ) -> Result<tonic::Response<proto::HealthResponse>, tonic::Status> {
        let request = request.get_ref();
        println!("Received request: {:?}", request);
        Ok(tonic::Response::new(proto::HealthResponse { ok: true }))
    }

    async fn notify(
        &self,
        request: tonic::Request<proto::NotifyRequest>,
    ) -> Result<tonic::Response<proto::NotifyResponse>, tonic::Status> {
        let request = request.get_ref();
        println!("Received request: {:?}", request);
        let mut client = WineBridgeClient::connect("http://[::1]:50051")
            .await
            .map_err(|e| tonic::Status::from_error(Box::new(e)))?;

        let request = proto::MessageRequest {
            message: request.message.clone(),
        };
        let response = client.message(request).await?;
        let response = response.get_ref();
        Ok(tonic::Response::new(proto::NotifyResponse {
            success: response.success,
        }))
    }
}
