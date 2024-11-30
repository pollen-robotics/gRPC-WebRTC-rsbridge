use gst::glib;
use gst::prelude::*;
use gstrswebrtc::signaller::Signallable;
use gstrswebrtc::signaller::SignallableExt;
use gstrswebrtc::signaller::Signaller;
use gstrswebrtc::signaller::WebRTCSignallerRole;

use log::{debug, error, info, warn};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::webrtc::session::Session;
//use std::sync::mpsc::channel;

pub struct WebRTCBridge {
    // pipeline: gst::Pipeline,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    signaller: Signaller,
    //rx: std::sync::mpsc::Receiver<bool>,
    main_loop: Arc<glib::MainLoop>,
}

impl WebRTCBridge {
    pub fn new(
        uri: String,
        name: String,
        grpc_address: String,
        rx_stop_signal: std::sync::mpsc::Receiver<bool>,
    ) -> Self {
        debug!("Constructor GstWebRTCServer");

        gstrswebrtc::plugin_register_static().unwrap();

        let main_loop = Arc::new(glib::MainLoop::new(None, false));
        let main_loop_clone = main_loop.clone();

        std::thread::spawn(move || {
            let _ = rx_stop_signal.recv();
            main_loop_clone.quit();
        });

        let sessions = Arc::new(Mutex::new(HashMap::<String, Session>::new()));

        //let (tx, rx) = channel::<bool>();
        let signaller = Signaller::new(WebRTCSignallerRole::Producer);
        signaller.set_property("uri", &uri);

        signaller.connect_closure(
            "request-meta",
            false,
            glib::closure!(move |_signaler: glib::Object| -> Option<gst::Structure> {
                let meta_structure =
                    Some(gst::Structure::builder("meta").field("name", &name).build());
                meta_structure
            }),
        );

        signaller.connect_closure(
            //"end-session",
            "session-ended",
            false,
            glib::closure!(
                #[strong]
                sessions,
                move |_signaler: glib::Object, session_id: &str| {
                    info!("session-ended {}", session_id);
                    sessions.lock().unwrap().remove(session_id);
                    false
                }
            ),
        );

        signaller.connect_closure(
            "shutdown",
            false,
            glib::closure!(
                #[strong]
                main_loop,
                move |_signaler: glib::Object| {
                    info!("Shutting down");
                    main_loop.quit();
                }
            ),
        );

        signaller.connect_closure(
            "error",
            false,
            glib::closure!(
                #[strong]
                main_loop,
                move |_signaler: glib::Object, error: String| {
                    error!("Signalling error: {}. Shutting down", error);
                    //tx.send(true).unwrap();
                    main_loop.quit();
                }
            ),
        );

        signaller.connect_closure(
            "session-requested",
            false,
            glib::closure!(
                #[strong]
                sessions,
                #[strong]
                main_loop,
                move |signaler: glib::Object,
                      session_id: &str,
                      peer_id: &str,
                      offer: Option<&gstwebrtc::WebRTCSessionDescription>| {
                    info!("Session requested id: {} peer_id: {} ", session_id, peer_id);

                    if let Some(_offer) = offer {
                        warn!("Discarding received offer");
                    }

                    let signaler_arc =
                        Arc::new(Mutex::new(signaler.downcast::<Signallable>().unwrap()));

                    match Session::new(
                        peer_id.to_string(),
                        signaler_arc.clone(),
                        session_id.to_string(),
                        grpc_address.clone(),
                        main_loop.clone(),
                    ) {
                        Ok(session) => sessions
                            .lock()
                            .unwrap()
                            .insert(session_id.to_string(), session),
                        Err(e) => {
                            error!("{}. Make sure that the SDK server is up.", e);
                            signaler_arc
                                .lock()
                                .unwrap()
                                .emit_by_name::<bool>("session-ended", &[&session_id]);
                            None
                        }
                    };
                }
            ),
        );

        signaller.connect_closure(
            "session-description",
            false,
            glib::closure!(
                #[strong]
                sessions,
                move |_signaler: glib::Object,
                      session_id: &str,
                      session_description: &gstwebrtc::WebRTCSessionDescription| {
                    if session_description.type_() == gstwebrtc::WebRTCSDPType::Answer {
                        sessions
                            .lock()
                            .unwrap()
                            .get(session_id)
                            .unwrap()
                            .handle_sdp_answer(session_description);
                    } else {
                        error!("Unsupported SDP Type");
                    }
                }
            ),
        );

        signaller.connect_closure(
            "session-started",
            false,
            glib::closure!(
                |_signaller: glib::Object, session_id: &str, peer_id: &str| {
                    info! {"session started {} {}", session_id, peer_id};
                }
            ),
        );

        signaller.connect_closure(
            "handle-ice",
            false,
            glib::closure!(
                #[strong]
                sessions,
                move |_signaler: glib::Object,
                      session_id: &str,
                      sdp_m_line_index: u32,
                      sdp_mid: Option<String>,
                      candidate: &str| {
                    sessions
                        .lock()
                        .unwrap()
                        .get(session_id)
                        .unwrap()
                        .handle_ice(Some(sdp_m_line_index), sdp_mid, candidate);
                }
            ),
        );

        Self {
            sessions,
            signaller,
            main_loop,
        }
    }

    pub fn run(&self) {
        self.signaller.start();
        info!("Bridge started");
        self.main_loop.run();
        info!("Bridge stopped");
        self.signaller.stop();
    }
}

impl Drop for WebRTCBridge {
    fn drop(&mut self) {
        self.sessions.lock().unwrap().clear();
    }
}
