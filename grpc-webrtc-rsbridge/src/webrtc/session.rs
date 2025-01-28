use crate::grpc::grpc_client::GrpcClient;
use reachy_api::bridge::service_request::Request;
use reachy_api::bridge::Connect;
use reachy_api::bridge::{service_response, ConnectionStatus, ServiceResponse};
use reachy_api::bridge::{AnyCommands, ServiceRequest};
use reachy_api::reachy::{ReachyId, ReachyState, ReachyStatus};

use gst::glib::{self, WeakRef};

use gst::prelude::*;
use gstrswebrtc::signaller::Signallable;
use gstrswebrtc::signaller::SignallableExt;
use gstwebrtc::WebRTCDataChannel;
use log::{debug, error, info, warn};
use prost::Message;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use std::collections::VecDeque;
use std::sync::mpsc::channel;
use std::time::Duration;

use std::sync::{Arc, Mutex};
use std::thread;

//use crate::webrtc::stats::Stats;

pub struct Session {
    peer_id: String,
    pipeline: gst::Pipeline,
    webrtcbin: gst::Element,
    tx_stop_thread: std::sync::mpsc::Sender<bool>,
    running: Arc<AtomicBool>,
}

impl Session {
    pub fn new(
        peer_id: String,
        signaller: WeakRef<Signallable>,
        session_id: String,
        grpc_address: String,
        main_loop: Arc<glib::MainLoop>,
    ) -> Result<Self, String> {
        debug!("Constructor Session with peer {}", peer_id);

        let (grpc_client, tx_stop_thread) =
            Session::spawn_grpc_client(grpc_address, signaller.clone(), session_id.clone());
        let Some(grpc_client) = grpc_client else {
            return Err("Cannot create grpc client".into());
        };

        let running = Arc::new(AtomicBool::new(true));

        let (pipeline, webrtcbin) = Session::setup_webrtc(
            &peer_id,
            signaller,
            session_id,
            grpc_client,
            running.clone(),
        );

        Session::setup_bus_watch(&pipeline, main_loop);

        Ok(Self {
            peer_id,
            pipeline,
            webrtcbin,
            tx_stop_thread: tx_stop_thread.unwrap(),
            running,
        })
    }

    fn spawn_grpc_client(
        grpc_address: String,
        signaller: WeakRef<Signallable>,
        session_id: String,
    ) -> (
        Option<Arc<Mutex<GrpcClient>>>,
        Option<std::sync::mpsc::Sender<bool>>,
    ) {
        let (tx, rx) = channel::<Option<Arc<Mutex<GrpcClient>>>>();
        let (tx_stop_thread, rx_stop_thread) = channel::<bool>();

        let tx_stop_thread_clone = tx_stop_thread.clone();
        //grpc client uses the tonic async lib that can't run in the glib:closure where this Session is created
        thread::spawn(move || {
            match GrpcClient::new(
                grpc_address,
                signaller,
                Some(session_id),
                Some(tx_stop_thread_clone),
            ) {
                Ok(client) => {
                    let grpc_client = Arc::new(Mutex::new(client));
                    tx.send(Some(grpc_client.clone())).unwrap();
                    rx_stop_thread.recv().unwrap();
                    grpc_client.lock().unwrap().stop();
                    debug!("exit grpc thread");
                }
                Err(e) => {
                    error!("Grpc client cannot be created: {}", e);
                    tx.send(None).unwrap()
                }
            };
        });
        let grpc_client = rx.recv().unwrap();
        (grpc_client, Some(tx_stop_thread))
    }

    fn setup_webrtc(
        peer_id: &String,
        signaller: WeakRef<Signallable>,
        session_id: String,
        grpc_client: Arc<Mutex<GrpcClient>>,
        running: Arc<AtomicBool>,
    ) -> (gst::Pipeline, gst::Element) {
        let pipeline = gst::Pipeline::builder()
            .name(format!("session-pipeline-{peer_id}"))
            .build();

        let webrtcbin = Session::create_webrtcbin(signaller.clone(), &session_id);

        pipeline.add(&webrtcbin).unwrap();

        let webrtcbin_ref = webrtcbin.downgrade();

        let ret = pipeline.set_state(gst::State::Playing);
        match ret {
            Ok(gst::StateChangeSuccess::Success) | Ok(gst::StateChangeSuccess::Async) => {
                // Pipeline state changed successfully
                Session::create_data_channels(webrtcbin_ref.clone(), grpc_client, running);
                info!("data channel");
                Session::create_offer(webrtcbin_ref.clone(), signaller, session_id);
                //Session::setup_stats(webrtcbin_ref);
            }
            Ok(gst::StateChangeSuccess::NoPreroll) => {
                error!("Failed to transition pipeline to PLAYING: No preroll data available");
            }
            Err(err) => {
                error!("Failed to transition pipeline to PLAYING: {:?}", err);
            }
        }
        (pipeline, webrtcbin)
    }

    /*fn setup_stats(webrtcbin: WeakRef<gst::Element>) {
        thread::spawn(move || {
            let stats = Stats::new(webrtcbin);
            stats.run();
        });
    }*/

    fn setup_bus_watch(pipeline: &gst::Pipeline, main_loop: Arc<glib::MainLoop>) {
        let bus = pipeline.bus().unwrap();
        let _bus_watch = bus
            .add_watch(move |_bus, message| {
                use gst::MessageView;
                match message.view() {
                    MessageView::Error(err) => {
                        error!(
                            "Error received from element {:?} {}",
                            err.src().map(|s| s.path_string()),
                            err.error()
                        );
                        error!("Debugging information: {:?}", err.debug());
                        main_loop.quit();
                        glib::ControlFlow::Break
                    }
                    MessageView::Eos(..) => {
                        info!("Reached end of stream");
                        main_loop.quit();
                        glib::ControlFlow::Break
                    }
                    _ => glib::ControlFlow::Continue,
                }
            })
            .unwrap();
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    fn stop(&self) {
        debug!("stop session");
        Session::stop_pipeline(&self.pipeline);
        let _ = self.tx_stop_thread.send(true);
    }

    fn stop_pipeline(pipeline: &gst::Pipeline) {
        debug!("stop pipeline");
        let ret = pipeline.set_state(gst::State::Null);
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
    }

    fn create_data_channels(
        webrtcbin: WeakRef<gst::Element>,
        grpc_client: Arc<Mutex<GrpcClient>>,
        running: Arc<AtomicBool>,
    ) {
        let channel = webrtcbin
            .upgrade()
            .unwrap()
            .emit_by_name::<WebRTCDataChannel>(
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
        Session::handle_connect_request(rx, webrtcbin, grpc_client.clone(), running);

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
                        info!("Received Connect Request");
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
        webrtcbin: WeakRef<gst::Element>,
        grpc_client: Arc<Mutex<GrpcClient>>,
        running: Arc<AtomicBool>,
    ) {
        thread::spawn(move || {
            let connect = rx.recv().unwrap();
            let reachyid = connect.reachy_id;
            let id = reachyid.clone().unwrap().id;

            let (tx_state, rx_state) = std::sync::mpsc::channel();
            let (tx_audit, rx_audit) = std::sync::mpsc::channel();
            let (tx_reliable_command, rx_reliable_command) =
                std::sync::mpsc::channel::<AnyCommands>();
            let (tx_lossy_command, rx_lossy_command) = std::sync::mpsc::channel::<AnyCommands>();

            //configure channels before any data is sent
            webrtcbin.upgrade().unwrap().connect_closure(
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
                            tx_state_clone.send(true).unwrap();
                        });
                    } else if name == format!("reachy_audit_{}", id) {
                        channel.connect_on_open(move |_| {
                            info!("Data channel audit opened");
                            tx_audit_clone.send(true).unwrap();
                        });
                    } else if name == format!("reachy_command_reliable_{}", id) {
                        channel.connect_on_open(move |channel_command: &WebRTCDataChannel| {
                            info!("Data channel command reliable opened");
                            let tx_reliable_command_clone2 = tx_reliable_command_clone.clone();
                            channel_command.connect_on_message_data(
                                move |_: &WebRTCDataChannel, msg: Option<&glib::Bytes>| {
                                    let Some(data) = msg else {
                                        warn!("reliable message is None");
                                        return;
                                    };
                                    let commands = match Message::decode(data.as_ref()) {
                                        Ok(commands) => commands,

                                        Err(e) => {
                                            error!("Failed to decode message: {}", e);
                                            return;
                                        }
                                    };
                                    tx_reliable_command_clone2.send(commands).unwrap();
                                },
                            );
                        });
                    } else if name == format!("reachy_command_lossy_{}", id) {
                        channel.connect_on_open(move |channel_command: &WebRTCDataChannel| {
                            info!("Data channel command lossy opened");
                            let tx_lossy_command_clone2 = tx_lossy_command_clone.clone();
                            channel_command.connect_on_message_data(
                                move |_: &WebRTCDataChannel, msg: Option<&glib::Bytes>| {
                                    let Some(data) = msg else {
                                        warn!("lossy message is None");
                                        return;
                                    };
                                    let commands = match Message::decode(data.as_ref()) {
                                        Ok(commands) => commands,

                                        Err(e) => {
                                            error!("Failed to decode message: {}", e);
                                            return;
                                        }
                                    };
                                    tx_lossy_command_clone2.send(commands).unwrap();
                                },
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
                running.clone(),
            );

            Session::configure_audit_channel(
                id,
                webrtcbin.clone(),
                grpc_client.clone(),
                reachyid,
                connect.audit_frequency,
                rx_audit,
                running.clone(),
            );

            Session::configure_command_channel_reliable(
                id,
                webrtcbin.clone(),
                grpc_client.clone(),
                rx_reliable_command,
                running.clone(),
            );

            Session::configure_command_channel_lossy(
                id,
                webrtcbin,
                grpc_client,
                rx_lossy_command,
                running,
            );

            debug!("exit create channels");
        });
    }

    fn configure_command_channel_reliable(
        id: u32,
        webrtcbin: WeakRef<gst::Element>,
        grpc_client: Arc<Mutex<GrpcClient>>,
        rx_command: std::sync::mpsc::Receiver<AnyCommands>,
        running: Arc<AtomicBool>,
    ) {
        let _channel_command = webrtcbin
            .upgrade()
            .unwrap()
            .emit_by_name::<WebRTCDataChannel>(
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
                if grpc_client
                    .lock()
                    .unwrap()
                    .handle_commands(commands)
                    .is_err()
                {
                    running.store(false, Ordering::Relaxed);
                    break;
                };
            }
            debug!("exit stream reliable command channel");
        });
    }

    fn configure_command_channel_lossy(
        id: u32,
        webrtcbin: WeakRef<gst::Element>,
        grpc_client: Arc<Mutex<GrpcClient>>,
        rx_command: std::sync::mpsc::Receiver<AnyCommands>,
        running: Arc<AtomicBool>,
    ) {
        let _channel_command = webrtcbin
            .upgrade()
            .unwrap()
            .emit_by_name::<WebRTCDataChannel>(
                "create-data-channel",
                &[
                    &format!("reachy_command_lossy_{}", id),
                    &gst::Structure::builder("config")
                        .field("ordered", true)
                        .field("max-retransmits", 0)
                        .build(),
                ],
            );

        let command_counter = Arc::new(AtomicU64::new(0));
        let command_counter_clone = command_counter.clone();
        let drop_counter = Arc::new(AtomicU64::new(0));
        let drop_counter_clone = drop_counter.clone();
        let running_clone = running.clone();

        thread::spawn(move || {
            let mut command_counter_old = 0u64;
            let mut drop_counter_old = 0u64;
            let display_frequency = 1u64;
            while running_clone.load(Ordering::Relaxed) {
                let current_counter_command = command_counter_clone.load(Ordering::Relaxed);
                let current_drop_counter = drop_counter_clone.load(Ordering::Relaxed);
                let freq_command =
                    (current_counter_command - command_counter_old) / display_frequency;
                let freq_drop = (current_drop_counter - drop_counter_old) / display_frequency;
                info!("Lossy Command freq: {freq_command} Hz - Drop frequency: {freq_drop} Hz");
                command_counter_old = current_counter_command;
                drop_counter_old = current_drop_counter;
                std::thread::sleep(Duration::from_secs(display_frequency));
            }
        });

        let queue_commands = Arc::new(Mutex::new(VecDeque::new()));
        let queue_commands_clone = queue_commands.clone();
        let running_clone = running.clone();

        thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let mut queue_commands_lock = queue_commands_clone.try_lock();
                if let Ok(ref mut queue) = queue_commands_lock {
                    if let Some(commands) = queue.pop_front() {
                        //debug!("play");
                        if grpc_client
                            .lock()
                            .unwrap()
                            .handle_commands(commands)
                            .is_err()
                        {
                            running.store(false, Ordering::Relaxed);
                            break;
                        };
                    }
                }
                drop(queue_commands_lock);
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        thread::spawn(move || {
            while let Ok(commands) = rx_command.recv() {
                //debug!("received lossy commands {:?}", commands);
                let mut queue_commands_lock = queue_commands.try_lock(); //.push_back(commands);
                if let Ok(ref mut queue) = queue_commands_lock {
                    queue.push_back(commands);

                    let counter = command_counter.load(Ordering::Relaxed) + 1;
                    command_counter.store(counter, Ordering::Relaxed);
                } else {
                    let counter = drop_counter.load(Ordering::Relaxed) + 1;
                    drop_counter.store(counter, Ordering::Relaxed);
                }
                drop(queue_commands_lock);
                std::thread::sleep(Duration::from_millis(1));
            }
            running_clone.store(false, Ordering::Relaxed);
            debug!("exit stream lossy command channel");
        });
    }

    fn configure_audit_channel(
        id: u32,
        webrtcbin: WeakRef<gst::Element>,
        grpc_client: Arc<Mutex<GrpcClient>>,
        reachy: Option<ReachyId>,
        update_frequency: f32,
        rx_audit: std::sync::mpsc::Receiver<bool>,
        running: Arc<AtomicBool>,
    ) {
        let max_packet_lifetime = if update_frequency > 1000f32 {
            1
        } else {
            (1000f32 / update_frequency) as u32
        };
        let channel = webrtcbin
            .upgrade()
            .unwrap()
            .emit_by_name::<WebRTCDataChannel>(
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
            running.store(false, Ordering::Relaxed);
            debug!("exit stream status channel");
        });
    }

    fn configure_state_channel(
        id: u32,
        webrtcbin: WeakRef<gst::Element>,
        grpc_client: Arc<Mutex<GrpcClient>>,
        reachy: Option<ReachyId>,
        update_frequency: f32,
        rx_state: std::sync::mpsc::Receiver<bool>,
        running: Arc<AtomicBool>,
    ) {
        let max_packet_lifetime = if update_frequency > 1000f32 {
            1
        } else {
            (1000f32 / update_frequency) as u32
        };
        let channel = webrtcbin
            .upgrade()
            .unwrap()
            .emit_by_name::<WebRTCDataChannel>(
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
            running.store(false, Ordering::Relaxed);
            debug!("exit stream state channel");
        });
    }

    fn create_webrtcbin(signaller: WeakRef<Signallable>, session_id: &str) -> gst::Element {
        let webrtcbin = gst::ElementFactory::make("webrtcbin")
            .build()
            .expect("Failed to create webrtcbin");

        webrtcbin.connect_closure(
            "on-ice-candidate",
            false,
            glib::closure!(
                #[to_owned]
                session_id,
                move |_webrtcbin: &gst::Element, sdp_m_line_index: u32, candidate: String| {
                    debug!("adding ice candidate {} {} ", sdp_m_line_index, candidate);
                    signaller /*.lock()*/
                        .upgrade()
                        .unwrap()
                        .add_ice(&session_id, &candidate, sdp_m_line_index, None)
                }
            ),
        );

        webrtcbin.connect_notify(
            Some("connection-state"),
            glib::clone!(
                #[to_owned]
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
                #[to_owned]
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
        webrtcbin: WeakRef<gst::Element>,
        signaller: WeakRef<Signallable>,
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
            .upgrade()
            .unwrap()
            .emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
    }

    fn on_offer_created(
        webrtcbin: WeakRef<gst::Element>,
        offer: gstwebrtc::WebRTCSessionDescription,
        signaller: WeakRef<Signallable>,
        session_id: String,
    ) {
        debug!("Set local description");
        webrtcbin
            .upgrade()
            .unwrap()
            .emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);

        let signaller = signaller.upgrade().unwrap(); //signaller_arc.lock().unwrap();

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
            .emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        debug!("Drop Session with peer {}", self.peer_id);
        self.stop();
    }
}
