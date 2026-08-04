use super::WebOsEndpoint;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use tungstenite::{accept, Message, WebSocket};

mod observed_tv;

pub(super) use observed_tv::{ObservedWebOsInput, ObservedWebOsTvServer};

pub(super) struct ScriptedWebOsServer {
    endpoint: WebOsEndpoint,
    handle: JoinHandle<()>,
}

impl ScriptedWebOsServer {
    pub(super) fn spawn<F>(script: F) -> Self
    where
        F: FnOnce(&mut ScriptedWebOsPeer<TcpStream>) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted webOS server");
        let endpoint = WebOsEndpoint::ws_at(listener.local_addr().expect("server address"));
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept webOS client");
            let socket = accept(stream).expect("accept webOS websocket");
            let mut peer = ScriptedWebOsPeer::from_socket(socket);
            script(&mut peer);
        });

        Self { endpoint, handle }
    }

    pub(super) fn endpoint(&self) -> WebOsEndpoint {
        self.endpoint
    }

    pub(super) fn finish(self) {
        self.handle.join().expect("scripted webOS server thread");
    }
}

pub(super) struct ScriptedWebOsPeer<S> {
    socket: WebSocket<S>,
}

impl<S> ScriptedWebOsPeer<S> {
    pub(super) fn from_socket(socket: WebSocket<S>) -> Self {
        Self { socket }
    }
}

impl<S: Read + Write> ScriptedWebOsPeer<S> {
    pub(super) fn receive_json(&mut self) -> Value {
        match self.socket.read().expect("read webOS client request") {
            Message::Text(text) => {
                serde_json::from_str(text.as_str()).expect("webOS client request JSON")
            }
            other => panic!("expected webOS text request, got {other:?}"),
        }
    }

    pub(super) fn send_json(&mut self, value: Value) {
        self.socket
            .send(Message::text(value.to_string()))
            .expect("send scripted webOS response");
    }

    pub(super) fn send_text(&mut self, value: &str) {
        self.socket
            .send(Message::text(value))
            .expect("send scripted webOS text");
    }

    pub(super) fn send_close(&mut self) {
        self.socket
            .send(Message::Close(None))
            .expect("send scripted webOS close");
    }
}
