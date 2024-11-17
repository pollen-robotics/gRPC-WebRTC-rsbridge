use log::{debug, error, info, warn};

use crate::grpc::reachy_api::reachy::reachy_service_client::ReachyServiceClient;
use crate::grpc::reachy_api::reachy::Reachy;
use tonic::{transport::Channel, Response};

pub struct GrpcClient {
    client: ReachyServiceClient<Channel>,
}

impl GrpcClient {
    pub async fn new(host: String, port: u16) -> Result<Self, Box<dyn std::error::Error>> {
        debug!("Constructor Grpc client {host}:{port}");
        let client = ReachyServiceClient::connect(format!("http://{host}:{port}")).await?;

        Ok(Self { client: client })
    }

    pub async fn get_reachy(&mut self) -> Result<Response<Reachy>, Box<dyn std::error::Error>> {
        debug!("Get reachy");
        let request = tonic::Request::new(());
        let response = self.client.get_reachy(request).await?;
        info!("Response: {:?}", response);
        Ok(response)
    }
}

#[tokio::test]
async fn test_get_reachy() {
    env_logger::init();

    let mut grpc_client = GrpcClient::new("localhost".to_string(), 50051)
        .await
        .unwrap();

    let reachy: Response<Reachy> = grpc_client.get_reachy().await.unwrap();
    let reachy_id = reachy.into_inner().id.unwrap();

    assert!(
        !reachy_id.name.is_empty(),
        "Reachy name should not be empty"
    );
    assert!(reachy_id.id > 0, "Reachy id should be greater than 0");
}
