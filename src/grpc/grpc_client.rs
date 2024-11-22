use log::debug;

use crate::grpc::reachy_api::reachy::reachy_service_client::ReachyServiceClient;
use crate::grpc::reachy_api::reachy::Reachy;
use tokio::runtime::Runtime;
use tonic::transport::Channel;

pub struct GrpcClient {
    client: ReachyServiceClient<Channel>,
    rt: Runtime, // see https://tokio.rs/tokio/topics/bridging
}

impl GrpcClient {
    pub fn new(address: String) -> Self {
        debug!("Constructor Grpc client {address}");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let client = rt
            .block_on(ReachyServiceClient::connect(address))
            .expect("Connection failed");

        Self {
            client: client,
            rt: rt,
        }
    }

    pub fn get_reachy(&mut self) -> Reachy {
        debug!("Get reachy");
        let request = tonic::Request::new(());

        self.rt
            .block_on(self.client.get_reachy(request))
            .unwrap()
            .into_inner()
    }
}

#[test]
fn test_get_reachy() {
    env_logger::init();

    let grpc_address = format!("http://localhost:50051");

    let mut grpc_client = GrpcClient::new(grpc_address);

    let reachy: Reachy = grpc_client.get_reachy();
    let reachy_id = reachy.id.unwrap();

    assert!(
        !reachy_id.name.is_empty(),
        "Reachy name should not be empty"
    );
    assert!(reachy_id.id > 0, "Reachy id should be greater than 0");
}
