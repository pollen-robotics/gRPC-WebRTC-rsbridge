use gst::glib;
use gst::prelude::*;
use gstrswebrtc::signaller::Signallable;
use gstrswebrtc::signaller::SignallableExt;
use gstwebrtc::WebRTCDataChannel;
use log::{debug, error, info, warn};
use std::sync::{Arc, Mutex};

use prost::Message;

use crate::grpc::reachy_api::bridge::service_request::Request;
use crate::grpc::reachy_api::bridge::ServiceRequest;

use crate::grpc::grpc_client::GrpcClient;

pub struct Session {
    peer_id: String,
    pipeline: gst::Pipeline,
    webrtcbin: Arc<Mutex<gst::Element>>,
    //grpc_client: Arc<Mutex<GrpcClient>>,
}

impl Session {
    pub fn new(peer_id: String, signaller: Arc<Mutex<Signallable>>, session_id: String) -> Self {
        debug!("Constructor Session with peer {}", peer_id);

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
                Session::create_data_channels(webrtcbin_arc.clone());
                Session::create_offer(webrtcbin_arc.clone(), signaller, session_id);
            }
            Ok(gst::StateChangeSuccess::NoPreroll) => {
                error!("Failed to transition pipeline to PLAYING: No preroll data available");
            }
            Err(err) => {
                error!("Failed to transition pipeline to PLAYING: {:?}", err);
            }
        }

        Self {
            peer_id: peer_id,
            pipeline: pipeline,
            webrtcbin: webrtcbin_arc,
        }
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
    }

    fn create_data_channels(webrtcbin: Arc<Mutex<gst::Element>>) {
        let channel = webrtcbin.lock().unwrap().emit_by_name::<WebRTCDataChannel>(
            "create-data-channel",
            &[
                &"service",
                &gst::Structure::builder("config")
                    .field("ordered", true)
                    .build(),
            ],
        );

        channel.connect_closure(
            "on-message-data",
            false,
            glib::closure!(|_channel: &WebRTCDataChannel, msg: &glib::Bytes| {
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
                        /*let resp = self.handle_get_reachy_request(grpc_client);
                        debug!("Sending service response message: {:?}", resp);
                        let byte_data = resp.encode_to_vec();
                        let gbyte_data = glib::Bytes::from(&byte_data);
                        data_channel.send_data(gbyte_data);*/
                    }
                    Some(Request::Connect(connect_request)) => {
                        info!("Received Connect request");
                        //let resp = self.handle_connect_request(connect_request, grpc_client, pc).await;
                        // Handle the response if needed
                    }
                    Some(Request::Disconnect(_)) => {
                        info!("Received Disconnect request");
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
                //#[weak]
                //element,
                //#[strong]
                //peer_id,
                #[strong]
                session_id,
                move |webrtcbin, _pspec| {
                    let state = webrtcbin
                        .property::<gstwebrtc::WebRTCPeerConnectionState>("connection-state");

                    match state {
                        gstwebrtc::WebRTCPeerConnectionState::Failed => {
                            /*let this = element.imp();
                            gst::warning!(
                                CAT,
                                obj = element,
                                "Connection state for in session {} (peer {}) failed",
                                session_id,
                                peer_id
                            );
                            let _ = this.remove_session(&session_id, true);*/
                            warn!("Connection state for in session {} failed", session_id);
                        }
                        _ => {
                            /*gst::log!(
                                CAT,
                                obj = element,
                                "Connection state in session {} (peer {}) changed: {:?}",
                                session_id,
                                peer_id,
                                state
                            );*/
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
