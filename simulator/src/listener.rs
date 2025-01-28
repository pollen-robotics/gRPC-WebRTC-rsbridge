use gst::glib;
use gst::prelude::*;
use gstrswebrtc::signaller::SignallableExt;
use gstrswebrtc::signaller::Signaller;
use gstrswebrtc::signaller::WebRTCSignallerRole;
use log::{error, info};
use std::sync::mpsc::channel;

pub struct Listener {
    signaller: Signaller,
    rx_peer_id: std::sync::mpsc::Receiver<Option<String>>,
}

impl Listener {
    pub fn new(uri: &String, peer_name: String) -> Self {
        let (tx_peer_id, rx_peer_id) = channel::<Option<String>>();
        let tx_peer_id_clone = tx_peer_id.clone();
        let signaller = Signaller::new(WebRTCSignallerRole::Listener);
        signaller.set_property("uri", uri);
        signaller.connect("producer-added", false, move |args| {
            let meta_structure = args[2].get::<gst::Structure>();
            match meta_structure {
                Ok(meta_structure) => {
                    let name = meta_structure.get::<String>("name");
                    match name {
                        Ok(name) => {
                            info!("Producer added: name {:?}", name);
                            if name == peer_name {
                                let peer_id = args[1].get::<String>().unwrap();
                                tx_peer_id.send(Some(peer_id)).unwrap();
                            }
                        }
                        Err(_) => {
                            error!("No name field in meta structure associated with producer");
                            tx_peer_id.send(None).unwrap();
                        }
                    }
                }
                Err(_) => {
                    error!("No meta structure associated with producer");
                    tx_peer_id.send(None).unwrap();
                }
            };
            None
        });
        signaller.connect("producer-removed", false, move |args| {
            let peer_id = args[1].get::<String>().unwrap();
            info!("Producer removed: {}", peer_id);
            None
        });

        signaller.connect_closure(
            "error",
            false,
            glib::closure!(move |_signaler: glib::Object, error: String| {
                error!("Signalling error: {}. Shutting down", error);
                tx_peer_id_clone.send(None).unwrap();
            }),
        );

        Self {
            signaller,
            rx_peer_id,
        }
    }

    pub fn look_for_peer_id(&self) -> Option<String> {
        self.signaller.start();
        info!("Listener started");
        let peer_id = self.rx_peer_id.recv().unwrap();
        info!("Listener stopped");
        self.signaller.stop();
        peer_id
    }
}
