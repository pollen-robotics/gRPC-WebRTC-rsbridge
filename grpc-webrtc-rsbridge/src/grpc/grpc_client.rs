use log::{debug, error, trace, warn};

use reachy_api::reachy::part::arm::arm_service_client::ArmServiceClient;
use reachy_api::reachy::part::hand::hand_service_client::HandServiceClient;
use reachy_api::reachy::part::head::head_service_client::HeadServiceClient;
use reachy_api::reachy::part::mobile::base::mobility::mobile_base_mobility_service_client::MobileBaseMobilityServiceClient;
use reachy_api::reachy::part::mobile::base::utility::mobile_base_utility_service_client::MobileBaseUtilityServiceClient;
use reachy_api::reachy::reachy_service_client::ReachyServiceClient;
use reachy_api::reachy::Reachy;
use reachy_api::reachy::ReachyStreamAuditRequest;
use reachy_api::reachy::ReachyStreamStateRequest;

use reachy_api::reachy::ReachyState;
use reachy_api::reachy::ReachyStatus;
use tokio::runtime::Runtime;
use tonic::transport::Channel;

use gst::glib::WeakRef;
use gst::prelude::*;
use gstrswebrtc::signaller::Signallable;
use reachy_api::bridge::{
    any_command, AnyCommands, ArmCommand, HandCommand, MobileBaseCommand, NeckCommand,
};
use reachy_api::reachy::ReachyId;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct GrpcClient {
    reachy_stub: ReachyServiceClient<Channel>,
    arm_stub: ArmServiceClient<Channel>,
    head_stub: HeadServiceClient<Channel>,
    hand_stub: HandServiceClient<Channel>,
    mobilebase_mobility_stub: MobileBaseMobilityServiceClient<Channel>,
    mobilebase_utility_stub: MobileBaseUtilityServiceClient<Channel>,
    rt: Runtime, // see https://tokio.rs/tokio/topics/bridging
    stop_flag: Arc<AtomicBool>,
    aborting: Arc<AtomicBool>,
    signaller: WeakRef<Signallable>,
    session_id: String,
    tx_stop: std::sync::mpsc::Sender<bool>,
}

impl GrpcClient {
    pub fn new(
        address: String,
        signaller: WeakRef<Signallable>,
        session_id: Option<String>,
        tx_stop: Option<std::sync::mpsc::Sender<bool>>,
    ) -> Result<Self, tonic::transport::Error> {
        debug!("Constructor Grpc client {address}");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        let reachy_stub = rt.block_on(ReachyServiceClient::connect(address.clone()))?;

        let arm_stub = rt.block_on(ArmServiceClient::connect(address.clone()))?;

        let hand_stub = rt.block_on(HandServiceClient::connect(address.clone()))?;

        let head_stub = rt.block_on(HeadServiceClient::connect(address.clone()))?;

        let mobilebase_mobility_stub =
            rt.block_on(MobileBaseMobilityServiceClient::connect(address.clone()))?;

        let mobilebase_utility_stub =
            rt.block_on(MobileBaseUtilityServiceClient::connect(address))?;

        Ok(Self {
            reachy_stub,
            arm_stub,
            head_stub,
            hand_stub,
            mobilebase_mobility_stub,
            mobilebase_utility_stub,
            rt,
            stop_flag: Arc::new(AtomicBool::new(false)),
            aborting: Arc::new(AtomicBool::new(false)),
            signaller, //: signaller.unwrap(),
            session_id: session_id.unwrap(),
            tx_stop: tx_stop.unwrap(),
        })
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }

    fn lost_connection(&mut self) {
        if !self.aborting.load(Ordering::Relaxed) {
            self.aborting.store(true, Ordering::Relaxed);
            warn!("grpc connection lost. aborting session");
            self.tx_stop.send(true).unwrap();
            self.signaller
            //.lock()
            .upgrade()
            .unwrap()
            .//emit_by_name::<bool>("session-ended", &[&self.session_id]);
        emit_by_name::<bool>("end-session", &[&self.session_id]);
        }
    }

    pub fn get_reachy(&mut self) -> Reachy {
        debug!("Get reachy");
        let request = tonic::Request::new(());
        self.rt
            .block_on(self.reachy_stub.get_reachy(request))
            .unwrap()
            .into_inner()
    }

    pub fn get_reachy_state(
        &mut self,
        reachy: Option<ReachyId>,
        freq: f32,
        tx: std::sync::mpsc::Sender<ReachyState>,
    ) {
        let request = tonic::Request::new(ReachyStreamStateRequest {
            id: reachy,
            publish_frequency: freq,
        });

        let mut stream = self
            .rt
            .block_on(self.reachy_stub.stream_reachy_state(request))
            .unwrap()
            .into_inner();

        let stop_flag = self.stop_flag.clone();

        self.rt.spawn(async move {
            loop {
                match stream.message().await {
                    Ok(Some(state)) => {
                        tx.send(state).unwrap();
                        if stop_flag.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    Ok(None) => {
                        warn!("Stream ended");
                        break;
                    }
                    Err(e) => {
                        error!("Failed to receive message from state stream: {}", e);
                        break;
                    }
                }
            }
            debug!("end of stream");
            drop(tx);
        });
    }

    pub fn get_reachy_audit_status(
        &mut self,
        reachy: Option<ReachyId>,
        freq: f32,
        tx: std::sync::mpsc::Sender<ReachyStatus>,
    ) {
        let request = tonic::Request::new(ReachyStreamAuditRequest {
            id: reachy,
            publish_frequency: freq,
        });

        let mut stream = self
            .rt
            .block_on(self.reachy_stub.stream_audit(request))
            .unwrap()
            .into_inner();

        let stop_flag = self.stop_flag.clone();

        self.rt.spawn(async move {
            loop {
                match stream.message().await {
                    Ok(Some(status)) => {
                        tx.send(status).unwrap();
                        if stop_flag.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    Ok(None) => {
                        warn!("Stream ended");
                        break;
                    }
                    Err(e) => {
                        error!("Failed to receive message from audit stream: {}", e);
                        break;
                    }
                }
            }
            drop(tx);
        });
    }

    pub fn handle_commands(&mut self, commands: AnyCommands) -> Result<(), String> {
        trace!("Received message: {:?}", commands);

        for cmd in commands.commands {
            match cmd.command {
                Some(any_command::Command::ArmCommand(arm_cmd)) => {
                    if self.handle_arm_command(arm_cmd).is_err() {
                        self.lost_connection();
                        return Err("Connection lost".into());
                    }
                }
                Some(any_command::Command::HandCommand(hand_cmd)) => {
                    if self.handle_hand_command(hand_cmd).is_err() {
                        self.lost_connection();
                        return Err("Connection lost".into());
                    }
                }
                Some(any_command::Command::NeckCommand(neck_cmd)) => {
                    if self.handle_neck_command(neck_cmd).is_err() {
                        self.lost_connection();
                        return Err("Connection lost".into());
                    }
                }
                Some(any_command::Command::MobileBaseCommand(mobile_base_cmd)) => {
                    if self.handle_mobile_base_command(mobile_base_cmd).is_err() {
                        self.lost_connection();
                        return Err("Connection lost".into());
                    }
                }
                _ => warn!("Unknown command: {:?}", cmd),
            }
        }
        Ok(())
    }

    fn handle_arm_command(&mut self, cmd: ArmCommand) -> Result<(), tonic::Status> {
        trace!("Handling arm command: {:?}", cmd);

        if let Some(arm_cartesian_goal) = cmd.arm_cartesian_goal {
            trace!("SendArmCartesianGoal");
            self.rt
                .block_on(self.arm_stub.send_arm_cartesian_goal(arm_cartesian_goal))?;
        } else if let Some(turn_on) = cmd.turn_on {
            trace!("arm_turn_on");
            self.rt.block_on(self.arm_stub.turn_on(turn_on))?;
        } else if let Some(turn_off) = cmd.turn_off {
            trace!("arm_turn_off");
            self.rt.block_on(self.arm_stub.turn_off(turn_off))?;
        } else if let Some(speed_limit) = cmd.speed_limit {
            trace!("arm_speed_limit");
            self.rt
                .block_on(self.arm_stub.set_speed_limit(speed_limit))?;
        } else if let Some(torque_limit) = cmd.torque_limit {
            trace!("arm_torque_limit");
            self.rt
                .block_on(self.arm_stub.set_torque_limit(torque_limit))?;
        } else {
            warn!("Unknown arm command: {:?}", cmd);
        }
        Ok(())
    }
    fn handle_hand_command(&mut self, cmd: HandCommand) -> Result<(), tonic::Status> {
        trace!("Handling hand command: {:?}", cmd);

        if let Some(hand_goal) = cmd.hand_goal {
            trace!("hand_goal");
            self.rt
                .block_on(self.hand_stub.set_hand_position(hand_goal))?;
        } else if let Some(turn_on) = cmd.turn_on {
            trace!("hand_turn_on");
            self.rt.block_on(self.hand_stub.turn_on(turn_on))?;
        } else if let Some(turn_off) = cmd.turn_off {
            trace!("hand_turn_off");
            self.rt.block_on(self.hand_stub.turn_off(turn_off))?;
        } else {
            warn!("Unknown hand command: {:?}", cmd);
        }
        Ok(())
    }

    fn handle_neck_command(&mut self, cmd: NeckCommand) -> Result<(), tonic::Status> {
        trace!("Handling neck command: {:?}", cmd);

        if let Some(neck_goal) = cmd.neck_goal {
            trace!("neck_goal");
            self.rt
                .block_on(self.head_stub.send_neck_joint_goal(neck_goal))?;
        } else if let Some(turn_on) = cmd.turn_on {
            trace!("neck_turn_on");
            self.rt.block_on(self.head_stub.turn_on(turn_on))?;
        } else if let Some(turn_off) = cmd.turn_off {
            trace!("neck_turn_off");
            self.rt.block_on(self.head_stub.turn_off(turn_off))?;
        } else if let Some(speed_limit) = cmd.speed_limit {
            trace!("neck_speed_limit");
            self.rt
                .block_on(self.head_stub.set_speed_limit(speed_limit))?;
        } else if let Some(torque_limit) = cmd.torque_limit {
            trace!("neck_torque_limit");
            self.rt
                .block_on(self.head_stub.set_torque_limit(torque_limit))?;
        } else {
            warn!("Unknown neck command: {:?}", cmd);
        }
        Ok(())
    }

    fn handle_mobile_base_command(&mut self, cmd: MobileBaseCommand) -> Result<(), tonic::Status> {
        trace!("Handling mobile base command: {:?}", cmd);

        if let Some(target_direction) = cmd.target_direction {
            trace!("mobile_base_target_direction");
            self.rt.block_on(
                self.mobilebase_mobility_stub
                    .send_direction(target_direction),
            )?;
        } else if let Some(mobile_base_mode) = cmd.mobile_base_mode {
            trace!("mobile_base_mode");
            self.rt
                .block_on(self.mobilebase_utility_stub.set_zuuu_mode(mobile_base_mode))?;
        } else {
            warn!("Unknown mobile base command: {:?}", cmd);
        }
        Ok(())
    }
}

#[test]
fn test_get_reachy() {
    env_logger::init();

    let grpc_address = format!("http://localhost:50051");

    let mut grpc_client = GrpcClient::new(grpc_address, None, None, None).unwrap();

    let reachy: Reachy = grpc_client.get_reachy();
    let reachy_id = reachy.id.unwrap();

    assert!(
        !reachy_id.name.is_empty(),
        "Reachy name should not be empty"
    );
    assert!(reachy_id.id > 0, "Reachy id should be greater than 0");
}
