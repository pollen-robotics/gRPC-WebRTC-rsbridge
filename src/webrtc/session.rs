use crate::grpc::grpc_client::GrpcClient;
use crate::grpc::reachy_api::bridge::service_request::Request;
use crate::grpc::reachy_api::bridge::Connect;
use crate::grpc::reachy_api::bridge::{any_command, AnyCommands, ServiceRequest};
use crate::grpc::reachy_api::bridge::{service_response, ConnectionStatus, ServiceResponse};
use crate::grpc::reachy_api::reachy::{ReachyId, ReachyState, ReachyStatus};

use gst::glib;

use gst::prelude::*;
use gstrswebrtc::signaller::Signallable;
use gstrswebrtc::signaller::SignallableExt;
use gstwebrtc::WebRTCDataChannel;
use log::{debug, error, info, warn};
use prost::Message;

use std::sync::mpsc::channel;

use std::sync::{Arc, Mutex};
use std::thread;

pub struct Session {
    peer_id: String,
    pipeline: gst::Pipeline,
    webrtcbin: Arc<Mutex<gst::Element>>,
    tx_stop_thread: std::sync::mpsc::Sender<bool>,
}

impl Session {
    pub fn new(
        peer_id: String,
        signaller: Arc<Mutex<Signallable>>,
        session_id: String,
        grpc_address: String,
    ) -> Self {
        debug!("Constructor Session with peer {}", peer_id);

        let (grpc_client, tx_stop_thread) = Session::spawn_grpc_client(grpc_address);

        let (pipeline, webrtcbin) =
            Session::setup_webrtc(&peer_id, signaller, session_id, grpc_client);

        Self {
            peer_id: peer_id,
            pipeline: pipeline,
            webrtcbin: webrtcbin,
            tx_stop_thread: tx_stop_thread,
        }
    }

    fn spawn_grpc_client(
        grpc_address: String,
    ) -> (Arc<Mutex<GrpcClient>>, std::sync::mpsc::Sender<bool>) {
        let (tx, rx) = channel();
        let (tx_stop_thread, rx_stop_thread) = channel::<bool>();
        //grpc client uses the tonic async lib that can't run in the glib:closure where this Session is created
        thread::spawn(move || {
            let grpc_client = Arc::new(Mutex::new(GrpcClient::new(grpc_address)));
            tx.send(grpc_client.clone()).unwrap();
            rx_stop_thread.recv().unwrap();
            grpc_client.lock().unwrap().stop();
            debug!("exit grpc thread");
        });
        let grpc_client = rx.recv().unwrap();
        (grpc_client, tx_stop_thread)
    }

    fn setup_webrtc(
        peer_id: &String,
        signaller: Arc<Mutex<Signallable>>,
        session_id: String,
        grpc_client: Arc<Mutex<GrpcClient>>,
    ) -> (gst::Pipeline, Arc<Mutex<gst::Element>>) {
        let pipeline = gst::Pipeline::builder()
            .name(format!("session-pipeline-{peer_id}"))
            .build();

        let webrtcbin = Session::create_webrtcbin(signaller.clone(), &session_id);

        pipeline.add(&webrtcbin).unwrap();

        let webrtcbin_arc = Arc::new(Mutex::new(webrtcbin));

        let ret = pipeline.set_state(gst::State::Playing);
        match ret {
            Ok(gst::StateChangeSuccess::Success) | Ok(gst::StateChangeSuccess::Async) => {
                // Pipeline state changed successfully
                Session::create_data_channels(webrtcbin_arc.clone(), grpc_client);
                Session::create_offer(webrtcbin_arc.clone(), signaller, session_id);
            }
            Ok(gst::StateChangeSuccess::NoPreroll) => {
                error!("Failed to transition pipeline to PLAYING: No preroll data available");
            }
            Err(err) => {
                error!("Failed to transition pipeline to PLAYING: {:?}", err);
            }
        }
        (pipeline, webrtcbin_arc)
    }

    fn stop(&self) {
        let ret = self.pipeline.set_state(gst::State::Null);
        match ret {
            Ok(gst::StateChangeSuccess::Success) | Ok(gst::StateChangeSuccess::Async) => {
                // Pipeline state changed successfully
            }
            Ok(gst::StateChangeSuccess::NoPreroll) => {
                error!("Failed to transition pipeline to NULL: No preroll data available");
            }
            Err(err) => {
                error!("Failed to transition pipeline to NULL: {:?}", err);
            }
        }
        let _ = self.tx_stop_thread.send(true);
    }

    fn create_data_channels(
        webrtcbin: Arc<Mutex<gst::Element>>,
        grpc_client: Arc<Mutex<GrpcClient>>,
    ) {
        let channel = webrtcbin.lock().unwrap().emit_by_name::<WebRTCDataChannel>(
            "create-data-channel",
            &[
                &"service",
                &gst::Structure::builder("config")
                    .field("ordered", true)
                    .build(),
            ],
        );

        // there is a deadlock that prevents from creating data channel from the glib closure
        let (tx, rx) = std::sync::mpsc::channel();
        Session::handle_connect_request(rx, webrtcbin, grpc_client.clone());

        channel.connect_closure(
            "on-message-data",
            false,
            glib::closure!(move |_channel: &WebRTCDataChannel, msg: &glib::Bytes| {
                let request: ServiceRequest = match Message::decode(msg.as_ref()) {
                    Ok(request) => request,
                    Err(e) => {
                        error!("Failed to decode message: {}", e);
                        return;
                    }
                };

                match request.request {
                    Some(Request::GetReachy(_)) => {
                        info!("Received GetReachy request");
                        let reachy = grpc_client.lock().unwrap().get_reachy();
                        let id = reachy.id.clone().unwrap().id;

                        let service_response = ServiceResponse {
                            response: Some(service_response::Response::ConnectionStatus(
                                ConnectionStatus {
                                    connected: true,
                                    state_channel: format!("reachy_state_{}", id,),
                                    command_channel: format!("reachy_command_{}", id),
                                    reachy: Some(reachy),
                                },
                            )),
                        };

                        let data = glib::Bytes::from_owned(service_response.encode_to_vec());
                        _channel.send_data(Some(&data));
                    }
                    Some(Request::Connect(connect_request)) => {
                        let _ = tx.send(connect_request);
                    }
                    Some(Request::Disconnect(_)) => {
                        warn!("Received Disconnect request. no handled!");
                        //let resp = self.handle_disconnect_request().await;
                        // Handle the response if needed
                    }
                    None => {
                        error!("No request field set in ServiceRequest");
                    }
                }
            }),
        );
    }

    fn handle_connect_request(
        rx: std::sync::mpsc::Receiver<Connect>,
        webrtcbin: Arc<Mutex<gst::Element>>,
        grpc_client: Arc<Mutex<GrpcClient>>,
    ) {
        thread::spawn(move || {
            let connect = rx.recv().unwrap();
            let reachyid = connect.reachy_id;
            let id = reachyid.clone().unwrap().id;

            let (tx_state, rx_state) = std::sync::mpsc::channel();
            let (tx_audit, rx_audit) = std::sync::mpsc::channel();
            let (tx_reliable_command, rx_reliable_command) = std::sync::mpsc::channel();
            let (tx_lossy_command, rx_lossy_command) = std::sync::mpsc::channel();

            //configure channels before any data is sent
            webrtcbin.lock().unwrap().connect_closure(
                "prepare-data-channel",
                false,
                glib::closure!(move |_webrtcbin: &gst::Element,
                                     channel: &WebRTCDataChannel,
                                     _islocal: bool| {
                    let name: String = channel.property("label");
                    debug!("prepare data channel {}", name);

                    channel.connect_on_error(|channel: &WebRTCDataChannel, error| {
                        error!(
                            "Data channel {} error {}",
                            channel.property::<String>("label"),
                            error
                        );
                    });

                    let tx_state_clone = tx_state.clone();
                    let tx_audit_clone = tx_audit.clone();
                    let tx_reliable_command_clone = tx_reliable_command.clone();
                    let tx_lossy_command_clone = tx_lossy_command.clone();
                    if name == format!("reachy_state_{}", id) {
                        channel.connect_on_open(move |_| {
                            info!("Data channel state opened");
                            let _ = tx_state_clone.send(true);
                        });
                    } else if name == format!("reachy_audit_{}", id) {
                        channel.connect_on_open(move |_| {
                            info!("Data channel audit opened");
                            let _ = tx_audit_clone.send(true);
                        });
                    } else if name == format!("reachy_command_reliable_{}", id) {
                        channel.connect_on_open(move |channel_command: &WebRTCDataChannel| {
                            info!("Data channel command reliable opened");
                            channel_command.connect_closure(
                                "on-message-data",
                                false,
                                glib::closure!(
                                    #[strong]
                                    tx_reliable_command_clone,
                                    move |_channel: &WebRTCDataChannel, msg: &glib::Bytes| {
                                        let commands: AnyCommands =
                                            match Message::decode(msg.as_ref()) {
                                                Ok(commands) => commands,
                                                Err(e) => {
                                                    error!("Failed to decode message: {}", e);
                                                    return;
                                                }
                                            };

                                        //debug!("received reliable commands {:?}", commands);
                                        let _ = tx_reliable_command_clone.send(commands);
                                        //let important_commmands =
                                        //    Session::create_important_commands(commands);
                                    }
                                ),
                            );
                        });
                    } else if name == format!("reachy_command_lossy_{}", id) {
                        channel.connect_on_open(move |channel_command: &WebRTCDataChannel| {
                            info!("Data channel command lossy opened");
                            //let _ = tx_command_clone.send(true);
                            channel_command.connect_closure(
                                "on-message-data",
                                false,
                                glib::closure!(
                                    #[strong]
                                    tx_lossy_command_clone,
                                    move |_channel: &WebRTCDataChannel, msg: &glib::Bytes| {
                                        let commands: AnyCommands =
                                            match Message::decode(msg.as_ref()) {
                                                Ok(commands) => commands,
                                                Err(e) => {
                                                    error!("Failed to decode message: {}", e);
                                                    return;
                                                }
                                            };

                                        //debug!("received lossy commands {:?}", commands);
                                        let _ = tx_lossy_command_clone.send(commands);
                                        //let important_commmands =
                                        //    Session::create_important_commands(commands);
                                    }
                                ),
                            );
                        });
                    }
                }),
            );

            Session::configure_state_channel(
                id,
                webrtcbin.clone(),
                grpc_client.clone(),
                reachyid.clone(),
                connect.update_frequency,
                rx_state,
            );

            Session::configure_audit_channel(
                id,
                webrtcbin.clone(),
                grpc_client.clone(),
                reachyid,
                connect.audit_frequency,
                rx_audit,
            );

            Session::configure_command_channel_reliable(
                id,
                webrtcbin.clone(),
                grpc_client.clone(),
                rx_reliable_command,
            );

            Session::configure_command_channel_lossy(id, webrtcbin, grpc_client, rx_lossy_command);

            debug!("exit create channels");
        });
    }

    fn configure_command_channel_reliable(
        id: u32,
        webrtcbin: Arc<Mutex<gst::Element>>,
        grpc_client: Arc<Mutex<GrpcClient>>,
        rx_command: std::sync::mpsc::Receiver<AnyCommands>,
    ) {
        let _channel_command = webrtcbin.lock().unwrap().emit_by_name::<WebRTCDataChannel>(
            "create-data-channel",
            &[
                &format!("reachy_command_reliable_{}", id),
                &gst::Structure::builder("config")
                    .field("ordered", true)
                    .build(),
            ],
        );

        thread::spawn(move || {
            while let Ok(commands) = rx_command.recv() {
                debug!("received reliable commands {:?}", commands);
                grpc_client.lock().unwrap().handle_commands(commands);
            }
            debug!("exit stream reliable command channel");
        });

        /*channel_command.connect_closure(
            "on-message-data",
            false,
            glib::closure!(move |_channel: &WebRTCDataChannel, msg: &glib::Bytes| {
                let commands: AnyCommands = match Message::decode(msg.as_ref()) {
                    Ok(commands) => commands,
                    Err(e) => {
                        error!("Failed to decode message: {}", e);
                        return;
                    }
                };

                //debug!("received commands {:?}", commands);
                let important_commmands = Session::create_important_commands(commands);
            }),
        );*/
    }

    fn configure_command_channel_lossy(
        id: u32,
        webrtcbin: Arc<Mutex<gst::Element>>,
        grpc_client: Arc<Mutex<GrpcClient>>,
        rx_command: std::sync::mpsc::Receiver<AnyCommands>,
    ) {
        let _channel_command = webrtcbin.lock().unwrap().emit_by_name::<WebRTCDataChannel>(
            "create-data-channel",
            &[
                &format!("reachy_command_lossy_{}", id),
                &gst::Structure::builder("config")
                    .field("ordered", true)
                    .field("max-retransmits", 0)
                    .build(),
            ],
        );

        thread::spawn(move || {
            while let Ok(commands) = rx_command.recv() {
                //debug!("received lossy commands {:?}", commands);
                grpc_client.lock().unwrap().handle_commands(commands);
            }
            debug!("exit stream lossy command channel");
        });
    }

    fn configure_audit_channel(
        id: u32,
        webrtcbin: Arc<Mutex<gst::Element>>,
        grpc_client: Arc<Mutex<GrpcClient>>,
        reachy: Option<ReachyId>,
        update_frequency: f32,
        rx_audit: std::sync::mpsc::Receiver<bool>,
    ) {
        let max_packet_lifetime = if update_frequency > 1000f32 {
            1
        } else {
            (1000f32 / update_frequency) as u32
        };
        let channel = webrtcbin.lock().unwrap().emit_by_name::<WebRTCDataChannel>(
            "create-data-channel",
            &[
                &format!("reachy_audit_{}", id),
                &gst::Structure::builder("config")
                    .field("ordered", true)
                    .field("max-packet-lifetime", max_packet_lifetime)
                    .build(),
            ],
        );

        thread::spawn(move || {
            let _ = rx_audit.recv();
            let (tx, rx) = std::sync::mpsc::channel::<ReachyStatus>();

            grpc_client
                .lock()
                .unwrap()
                .get_reachy_audit_status(reachy, update_frequency, tx);

            while let Ok(status) = rx.recv() {
                //info!("{:?} {}", status, channel.property::<String>("label"));
                let data = glib::Bytes::from_owned(status.encode_to_vec());
                channel.send_data(Some(&data));
            }
            debug!("exit stream status channel");
        });
    }

    fn configure_state_channel(
        id: u32,
        webrtcbin: Arc<Mutex<gst::Element>>,
        grpc_client: Arc<Mutex<GrpcClient>>,
        reachy: Option<ReachyId>,
        update_frequency: f32,
        rx_state: std::sync::mpsc::Receiver<bool>,
    ) {
        let max_packet_lifetime = if update_frequency > 1000f32 {
            1
        } else {
            (1000f32 / update_frequency) as u32
        };
        let channel = webrtcbin.lock().unwrap().emit_by_name::<WebRTCDataChannel>(
            "create-data-channel",
            &[
                &format!("reachy_state_{}", id),
                &gst::Structure::builder("config")
                    .field("ordered", true)
                    .field("max-packet-lifetime", max_packet_lifetime)
                    .build(),
            ],
        );

        thread::spawn(move || {
            let _ = rx_state.recv();
            let (tx, rx) = std::sync::mpsc::channel::<ReachyState>();

            grpc_client
                .lock()
                .unwrap()
                .get_reachy_state(reachy, update_frequency, tx);

            while let Ok(state) = rx.recv() {
                //info!("{:?} {}", state, channel.property::<String>("label"));
                let data = glib::Bytes::from_owned(state.encode_to_vec());
                channel.send_data(Some(&data));
            }
            debug!("exit stream state channel");
        });
    }

    fn create_important_commands(commands: AnyCommands) -> AnyCommands {
        let mut commands_important = AnyCommands {
            commands: Vec::new(),
        };

        for mut cmd in commands.commands {
            let mut cmd_important = cmd.clone();

            match cmd_important.command {
                Some(any_command::Command::ArmCommand(ref mut arm_cmd)) => {
                    arm_cmd.arm_cartesian_goal = None;
                }
                Some(any_command::Command::HandCommand(ref mut hand_cmd)) => {
                    hand_cmd.hand_goal = None;
                }
                Some(any_command::Command::NeckCommand(ref mut neck_cmd)) => {
                    neck_cmd.neck_goal = None;
                }
                Some(any_command::Command::MobileBaseCommand(ref mut mobile_base_cmd)) => {
                    mobile_base_cmd.target_direction = None;
                }
                None => {}
            }

            commands_important.commands.push(cmd_important);

            match cmd.command {
                Some(any_command::Command::ArmCommand(ref mut arm_cmd)) => {
                    arm_cmd.speed_limit = None;
                    arm_cmd.torque_limit = None;
                    arm_cmd.turn_off = None;
                    arm_cmd.turn_on = None;
                }
                Some(any_command::Command::HandCommand(ref mut hand_cmd)) => {
                    hand_cmd.hand_goal = None;
                }
                Some(any_command::Command::NeckCommand(ref mut neck_cmd)) => {
                    neck_cmd.neck_goal = None;
                }
                Some(any_command::Command::MobileBaseCommand(ref mut mobile_base_cmd)) => {
                    mobile_base_cmd.target_direction = None;
                }
                None => {}
            }
        }

        commands_important
    }

    fn create_webrtcbin(signaller: Arc<Mutex<Signallable>>, session_id: &String) -> gst::Element {
        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .build()
            .expect("Failed to create webrtcbin");

        webrtcbin.connect_closure(
            "on-ice-candidate",
            false,
            glib::closure!(
                #[strong]
                session_id,
                move |_webrtcbin: &gst::Element, sdp_m_line_index: u32, candidate: String| {
                    debug!("adding ice candidate {} {} ", sdp_m_line_index, candidate);
                    signaller.lock().unwrap().add_ice(
                        &session_id,
                        &candidate,
                        sdp_m_line_index,
                        None,
                    )
                }
            ),
        );

        webrtcbin.connect_notify(
            Some("connection-state"),
            glib::clone!(
                #[strong]
                session_id,
                move |webrtcbin, _pspec| {
                    let state = webrtcbin
                        .property::<gstwebrtc::WebRTCPeerConnectionState>("connection-state");

                    match state {
                        gstwebrtc::WebRTCPeerConnectionState::Failed => {
                            warn!("Connection state for in session {} failed", session_id);
                        }
                        _ => {
                            info!(
                                "Connection state in session {}  changed: {:?}",
                                session_id, state
                            );
                        }
                    }
                }
            ),
        );

        webrtcbin.connect_notify(
            Some("ice-connection-state"),
            glib::clone!(
                #[strong]
                session_id,
                move |webrtcbin, _pspec| {
                    let state = webrtcbin
                        .property::<gstwebrtc::WebRTCICEConnectionState>("ice-connection-state");

                    match state {
                        gstwebrtc::WebRTCICEConnectionState::Failed => {
                            error!("Ice connection state in session {} failed", session_id);
                        }
                        _ => {
                            debug!(
                                "Ice connection state in session {} changed: {:?}",
                                session_id, state
                            );
                        }
                    }

                    if state == gstwebrtc::WebRTCICEConnectionState::Completed {
                        debug!("Ice connection state in session {} completed", session_id);
                    }
                }
            ),
        );

        webrtcbin
    }

    fn create_offer(
        webrtcbin: Arc<Mutex<gst::Element>>,
        signaller: Arc<Mutex<Signallable>>,
        session_id: String,
    ) {
        debug!("Creating offer for session");

        let webrtcbin_clone = webrtcbin.clone();

        let promise = gst::Promise::with_change_func(glib::clone!(move |reply| {
            let reply = match reply {
                Ok(Some(reply)) => reply,
                Ok(None) => {
                    debug!("Promise returned without a reply for");
                    return;
                }
                Err(err) => {
                    debug!("Promise returned with an error for: {:?}", err);
                    return;
                }
            };

            if let Ok(offer) = reply
                .value("offer")
                .map(|offer| offer.get::<gstwebrtc::WebRTCSessionDescription>().unwrap())
            {
                Session::on_offer_created(webrtcbin_clone, offer, signaller, session_id);
            } else {
                debug!("Reply without an offer for session: {:?}", reply);
            }
        }));
        webrtcbin
            .lock()
            .unwrap()
            .emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
    }

    fn on_offer_created(
        webrtcbin: Arc<Mutex<gst::Element>>,
        offer: gstwebrtc::WebRTCSessionDescription,
        signaller_arc: Arc<Mutex<Signallable>>,
        session_id: String,
    ) {
        debug!("Set local description");
        webrtcbin
            .lock()
            .unwrap()
            .emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);

        let signaller = signaller_arc.lock().unwrap();

        let maybe_munged_offer = if signaller
            .has_property("manual-sdp-munging", Some(bool::static_type()))
            && signaller.property("manual-sdp-munging")
        {
            // Don't munge, signaller will manage this
            offer
        } else {
            // Use the default munging mechanism (signal registered by user)
            signaller.munge_sdp(&session_id, &offer)
        };
        signaller.send_sdp(&session_id, &maybe_munged_offer);
    }

    pub fn handle_sdp_answer(&self, desc: &gstwebrtc::WebRTCSessionDescription) {
        debug!("Set remote description");

        self.webrtcbin
            .lock()
            .unwrap()
            .emit_by_name::<()>("set-remote-description", &[desc, &None::<gst::Promise>]);
    }

    pub fn handle_ice(
        &self,
        sdp_m_line_index: Option<u32>,
        _sdp_mid: Option<String>,
        candidate: &str,
    ) {
        let sdp_m_line_index = match sdp_m_line_index {
            Some(sdp_m_line_index) => sdp_m_line_index,
            None => {
                warn!("No mandatory SDP m-line index");
                return;
            }
        };

        debug!("handle ice {} {}", sdp_m_line_index, candidate);

        self.webrtcbin
            .lock()
            .unwrap()
            .emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        debug!("Drop Session with peer {}", self.peer_id);
        self.stop();
    }
}
