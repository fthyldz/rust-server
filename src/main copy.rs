use futures::{SinkExt, StreamExt};
use serde_json::Value;
use webrtc::rtp::packet::Packet;
use webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver;
use webrtc::rtp_transceiver::RTCRtpTransceiver;
use webrtc::track::track_remote::TrackRemote;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct ChatMessage {
    msg_type: String,
    content: String,
}

type Clients = Arc<Mutex<HashMap<SocketAddr, futures::channel::mpsc::UnboundedSender<Message>>>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Sunucuyu localhost:8080'de başlat
    //let addr = "0.0.0.0:8080";
    let addr = "127.0.0.1:8080";
    let listener = TcpListener::bind(&addr).await?;
    println!("WebSocket sunucusu şurada çalışıyor: {}", addr);

    // Bağlı istemcilerin listesini tut
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));

    // Yeni bağlantıları kabul et
    while let Ok((stream, addr)) = listener.accept().await {
        println!("Yeni bağlantı: {}", addr);
        
        let clients = clients.clone();
        tokio::spawn(handle_connection(stream, addr, clients));
    }

    Ok(())
}

async fn handle_connection(stream: TcpStream, addr: SocketAddr, clients: Clients) {
    // TCP stream'i WebSocket'e yükselt
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            println!("WebSocket yükseltme hatası: {}", e);
            return;
        }
    };

    // WebSocket stream'ini gelen ve giden mesajlara ayır
    let (ws_sender, mut ws_receiver) = ws_stream.split();

    let ws_sender = Arc::new(Mutex::new(ws_sender));

    // Broadcast kanalı oluştur
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    clients.lock().await.insert(addr, tx);

    // Broadcast mesajlarını dinle ve gönder
    let ws_sender_clone = Arc::clone(&ws_sender);
    let broadcast_task = tokio::spawn(async move {
        while let Some(msg) = rx.next().await {
            println!("{} adresine mesaj gönderiliyor: {:?}", addr, msg);
            let mut ws_sender = ws_sender_clone.lock().await;
            ws_sender
                .send(msg)
                .await
                .unwrap_or_else(|e| println!("Mesaj gönderme hatası: {}", e));
        }
    });

    // WebRTC Peer Connection oluşturma
    let peer_connection = match create_peer_connection().await {
        Ok(pc) => pc,
        Err(e) => {
            println!("Peer bağlantısı oluşturulamadı: {}", e);
            return;
        }
    };

    // İstemciden gelen mesajları dinle
    let ws_sender_clone = Arc::clone(&ws_sender);
    while let Some(result) = ws_receiver.next().await {
        match result {
            Ok(msg) => {
                //println!("{} adresinden mesaj alındı: {:?}", addr, msg);
                
                match msg {
                    Message::Text(text) => {
                        // Mesajı serde_json::Value olarak ayrıştır
                        match serde_json::from_str::<Value>(&text) {
                            Ok(value) => {
                                // Mesajın msg_type alanına bak
                                if let Some(msg_type) = value.get("msg_type").and_then(|v| v.as_str()) {
                                    match msg_type {
                                        "offer" | "answer" | "candidate" => {
                                            // WebRTC ile ilgili SignalMessage olarak işleyin
                                            if let Ok(signal_msg) = serde_json::from_value::<SignalMessage>(value.clone()) {
                                                 //println!("Signaling mesajı alındı: {:?}", signal_msg);

                                                if signal_msg.msg_type == "offer" || signal_msg.msg_type == "answer" {
                                                    if let Some(sdp) = signal_msg.sdp {
                                                        let sdp = if signal_msg.msg_type == "offer" {
                                                            let sdp = serde_json::from_str::<Value>(&sdp).unwrap().get("sdp").and_then(|v| v.as_str()).unwrap().to_string();
                                                            RTCSessionDescription::offer(sdp).unwrap()
                                                        } else {
                                                            let sdp = serde_json::from_str::<Value>(&sdp).unwrap().get("sdp").and_then(|v| v.as_str()).unwrap().to_string();
                                                            RTCSessionDescription::answer(sdp).unwrap()
                                                        };
                                                        peer_connection.set_remote_description(sdp).await.unwrap();
                                                    }
                                                }

                                                if signal_msg.msg_type == "candidate" {
                                                    if let Some(candidate) = signal_msg.candidate {
                                                        println!("ICE adayı alındı: {:?}", candidate);
                                                        if let Err(err) = peer_connection
                                                                .add_ice_candidate(RTCIceCandidateInit {
                                                                    candidate: candidate,
                                                                    sdp_mid: Some("0".to_string()), // Example value
                                                                    sdp_mline_index: Some(0), // Example value
                                                                    username_fragment: None,
                                                                })
                                                                .await {
                                                            println!("ICE adayı eklenirken hata: {:?}", err);
                                                        }
                                                        else {
                                                            println!("ICE adayı başarıyla eklendi");
                                                        }

                                                    }
                                                }
                                            }
                                        }
                                        "chat" => {
                                            // Chat mesajı olarak işleyin
                                            if let Ok(chat_msg) = serde_json::from_value::<ChatMessage>(value.clone()) {
                                                println!("İstemciden alınan chat mesajı: {:?}", chat_msg);

                                                // Mesajı diğer istemcilere gönder
                                                let clients_lock = clients.lock().await;
                                                for (client_addr, client_tx) in clients_lock.iter() {
                                                    if *client_addr != addr {
                                                        let json_msg = serde_json::to_string(&chat_msg).unwrap();
                                                        if let Err(e) = client_tx.unbounded_send(Message::Text(json_msg)) {
                                                            println!("Broadcast hatası {}: {}", client_addr, e);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        "ping" => {
                                            println!("{} adresinden ping alındı", addr);
                                            let mut ws_sender = ws_sender_clone.lock().await;
                                            if let Err(e) = ws_sender.send(Message::Text(String::from("{ 'msg_type': 'pong', 'content': [] }"))).await {
                                                println!("Pong gönderme hatası: {}", e);
                                                break;
                                            }
                                        }
                                        _ => {
                                            println!("Bilinmeyen msg_type: {}", msg_type);
                                        }
                                    }
                                } else {
                                    println!("msg_type bulunamadı");
                                }
                            }
                            Err(e) => {
                                println!("JSON ayrıştırma hatası: {}", e);
                            }
                        }
                    }
                    Message::Close(_) => {
                        println!("{} bağlantıyı kapatıyor", addr);
                        break;
                    }
                    _ => {}
                }
            }
            Err(e) => {
                println!("Mesaj alımında hata: {}", e);
                break;
            }
        }
    }

    // Bağlantı koptuğunda temizlik yap
    println!("{} bağlantısı koptu", addr);
    clients.lock().await.remove(&addr);
    broadcast_task.abort();
}

use webrtc::api::APIBuilder;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::data_channel::data_channel_message::DataChannelMessage;

#[derive(Serialize, Deserialize, Debug)]
struct SignalMessage {
    msg_type: String,
    sdp: Option<String>,
    candidate: Option<String>,
}

async fn create_peer_connection() -> Result<RTCPeerConnection, Box<dyn std::error::Error>> {
    let config = RTCConfiguration::default();
    let api = APIBuilder::new().build();
    
    // PeerConnection oluştur
    let peer_connection = api.new_peer_connection(config).await?;

    // Data Channel oluştur
    let data_channel = peer_connection.create_data_channel("chat", None).await?;

    // Data Channel mesajlarını işleyin
    data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
        Box::pin(async move {
            println!("DataChannel mesajı alındı: {:?}", msg);
        })
    }));

    peer_connection.on_track(Box::new(move |track: Arc<TrackRemote>, receiver: Arc<RTCRtpReceiver>, _: Arc<RTCRtpTransceiver>| {
        // Tüm closure'ı 'static hale getirmek için move anahtar kelimesi kullanılıyor.
        Box::pin(async move {
            // Track ve Receiver'dan gerekli bilgileri alabilirsiniz
            println!("Track id: {}, Receiver id: {}", track.id(), receiver.tracks().await[0].id());
    
            // RTP paketlerini okuyun
            while let Ok((rtp_packet, _)) = track.read_rtp().await {
                // RTP paketini işleyin
                handle_rtp_packet(&rtp_packet);
            }
        })
    }));

    // ICE adayları ekleme
    peer_connection.on_ice_candidate(Box::new(|candidate: Option<RTCIceCandidate>| {
        Box::pin(async move {
            if let Some(candidate) = candidate {
                // Burada, ICE adayını JSON olarak istemciye gönderin
                println!("ICE adayı alındı: {:?}", candidate);
            }
        })
    }));

    Ok(peer_connection)
}

// RTP paketlerini işlemek için bir fonksiyon tanımlayın
fn handle_rtp_packet(packet: &Packet) {
    // Burada RTP paketini işleyin
    // Örneğin, payload'u veya timestamp'ı kontrol edebilir veya ek bilgi ekleyebilirsiniz
    println!("RTP Paketi Alındı: {}", packet);
}