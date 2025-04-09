//based on gst-plugins-rs/net/webrtc/examples/webrtcsink-stats-server.rs

use async_tungstenite::tungstenite::Message as WsMessage;
use futures::channel::mpsc;
use futures::SinkExt;
use futures::StreamExt;
use gst::glib::Type;
use gst::glib::{self, WeakRef};
use gst::prelude::*;
use log::{debug, info, trace};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;
use tokio::task;
use tokio::time;

#[allow(dead_code)]
#[derive(Clone)]
struct Listener {
    id: uuid::Uuid,
    sender: mpsc::Sender<WsMessage>,
}

#[allow(dead_code)]
struct State {
    listeners: Vec<Listener>,
}

#[allow(dead_code)]
pub struct Stats {
    rt: Runtime, // see https://tokio.rs/tokio/topics/bridging
    state: Arc<Mutex<State>>,
    listener: TcpListener,
}

#[allow(dead_code)]
impl Stats {
    pub fn new(webrtcbin: WeakRef<gst::Element>) -> Self {
        let state = Arc::new(Mutex::new(State { listeners: vec![] }));

        let addr = "127.0.0.1:8484".to_string();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        // Create the event loop and TCP listener we'll accept connections on.
        let try_socket = rt.block_on(TcpListener::bind(&addr));
        let listener = try_socket.expect("Failed to bind");
        info!("Listening on: {}", addr);

        let state_clone = state.clone();
        //let ws_clone = webrtcbin.lock().unwrap().downgrade();

        //task::spawn(async move {
        rt.spawn(async move {
            let mut interval = time::interval(std::time::Duration::from_millis(100));

            loop {
                interval.tick().await;
                if let Some(ws) = webrtcbin.upgrade() {
                    let (tx_stats, rx_stats) = std::sync::mpsc::channel();
                    Stats::process_stats(ws, tx_stats);
                    let stats = rx_stats.recv().unwrap();
                    //let stats = ws.property::<gst::Structure>("stats");
                    //debug!("Stats: {:?}", stats);
                    let stats = Stats::serialize_value(&stats.to_value()).unwrap();
                    debug!("Stats: {}", serde_json::to_string_pretty(&stats).unwrap());
                    let msg = WsMessage::Text(serde_json::to_string(&stats).unwrap().into());

                    let listeners = state_clone.lock().unwrap().listeners.clone();

                    for mut listener in listeners {
                        if listener.sender.send(msg.clone()).await.is_err() {
                            let mut state = state_clone.lock().unwrap();
                            let index = state
                                .listeners
                                .iter()
                                .position(|l| l.id == listener.id)
                                .unwrap();
                            state.listeners.remove(index);
                        }
                    }
                } else {
                    break;
                }
            }
        });

        Self {
            rt,
            state,
            listener,
        }
    }

    fn process_stats(webrtcbin: gst::Element, tx_stats: std::sync::mpsc::Sender<gst::Structure>) {
        let promise = gst::Promise::with_change_func(glib::clone!(move |reply| {
            if let Ok(Some(stats)) = reply {
                tx_stats.send(stats.to_owned()).unwrap();
            }
        }));

        webrtcbin.emit_by_name::<()>("get-stats", &[&None::<gst::Pad>, &promise]);
    }

    pub fn run(self) {
        info!("Running stats server");
        while let Ok((stream, _)) = self.rt.block_on(self.listener.accept()) {
            info!("Accepted connection");
            //task::spawn(Stats::accept_connection(state.clone(), stream));
            self.rt
                .spawn(Stats::accept_connection(self.state.clone(), stream));
        }
    }

    fn serialize_value(val: &gst::glib::Value) -> Option<serde_json::Value> {
        match val.type_() {
            Type::STRING => Some(val.get::<String>().unwrap().into()),
            Type::BOOL => Some(val.get::<bool>().unwrap().into()),
            Type::I32 => Some(val.get::<i32>().unwrap().into()),
            Type::U32 => Some(val.get::<u32>().unwrap().into()),
            Type::I_LONG | Type::I64 => Some(val.get::<i64>().unwrap().into()),
            Type::U_LONG | Type::U64 => Some(val.get::<u64>().unwrap().into()),
            Type::F32 => Some(val.get::<f32>().unwrap().into()),
            Type::F64 => Some(val.get::<f64>().unwrap().into()),
            _ => {
                if let Ok(s) = val.get::<gst::Structure>() {
                    serde_json::to_value(
                        s.iter()
                            .filter_map(|(name, value)| {
                                Stats::serialize_value(value).map(|value| (name.to_string(), value))
                            })
                            .collect::<HashMap<String, serde_json::Value>>(),
                    )
                    .ok()
                } else if let Ok(a) = val.get::<gst::Array>() {
                    serde_json::to_value(
                        a.iter()
                            .filter_map(|value| Stats::serialize_value(value))
                            .collect::<Vec<serde_json::Value>>(),
                    )
                    .ok()
                } else if let Some((_klass, values)) = gst::glib::FlagsValue::from_value(val) {
                    Some(
                        values
                            .iter()
                            .map(|value| value.nick())
                            .collect::<Vec<&str>>()
                            .join("+")
                            .into(),
                    )
                } else if let Ok(value) = val.serialize() {
                    Some(value.as_str().into())
                } else {
                    None
                }
            }
        }
    }

    async fn accept_connection(state: Arc<Mutex<State>>, stream: TcpStream) {
        let addr = stream
            .peer_addr()
            .expect("connected streams should have a peer address");
        info!("Peer address: {}", addr);

        let mut ws_stream = async_tungstenite::tokio::accept_async(stream)
            .await
            .expect("Error during the websocket handshake occurred");

        info!("New WebSocket connection: {}", addr);

        let mut state = state.lock().unwrap();
        let (sender, mut receiver) = mpsc::channel::<WsMessage>(1000);
        state.listeners.push(Listener {
            id: uuid::Uuid::new_v4(),
            sender,
        });
        drop(state);

        task::spawn(async move {
            while let Some(msg) = receiver.next().await {
                trace!("Sending to one listener!");
                if ws_stream.send(msg).await.is_err() {
                    info!("Listener errored out");
                    receiver.close();
                }
            }
        });
    }
}
