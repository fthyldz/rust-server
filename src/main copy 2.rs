use futures::{SinkExt, StreamExt};
use serde_json::Value;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp::packet::Packet;
//use webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver;
//use webrtc::rtp_transceiver::RTCRtpTransceiver;
//use webrtc::track::track_remote::TrackRemote;
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

#[derive(Serialize, Deserialize, Debug)]
struct SignalMessage {
    msg_type: String,
    content: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct OfferAnswerMessageJson {
    msg_type: String,
    content: OfferAnswerContentMessageJson,
}

#[derive(Serialize, Deserialize, Debug)]
struct OfferAnswerContentMessageJson {
    r#type: String,
    sdp: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct OfferAnswerMessageRust {
    msg_type: String,
    content: OfferAnswerContentMessageRust,
}

#[derive(Serialize, Deserialize, Debug)]
struct OfferAnswerContentMessageRust {
    sdp_type: String,
    sdp: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct IceCandidateMessage {
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
            //println!("{} adresine mesaj gönderiliyor: {:?}", addr, msg);
            let mut ws_sender = ws_sender_clone.lock().await;
            ws_sender
                .send(msg)
                .await
                .unwrap_or_else(|e| println!("Mesaj gönderme hatası: {}", e));
        }
    });

    // WebRTC Peer Connection oluşturma
    let peer_connection = match create_peer_connection(/*clients.clone(), addr*/).await {
        Ok(pc) => pc,
        Err(e) => {
            println!("Peer bağlantısı oluşturulamadı: {}", e);
            return;
        }
    };
    
    let peer_connection = Arc::new(Mutex::new(peer_connection));

    let peer_connection_clone = Arc::clone(&peer_connection);
    tokio::spawn(async move {
        peer_connection_clone.lock().await.lock().await.on_track(Box::new(move |track, _, _| {
            println!("Track id: {}", track.id());
            let track_clone = track.clone();
            tokio::spawn(async move {
                while let Ok((rtp_packet, _)) = track_clone.read_rtp().await {
                    // RTP paketini işleyin
                    println!("RTP Paketi Alındı");
                    handle_rtp_packet(&rtp_packet);
                }
            });
    
            Box::pin(async move {})
        }));
    });

    let peer_connection_clone = Arc::clone(&peer_connection);
    let clients_clone = Arc::clone(&clients);
    tokio::spawn(async move {
        let clients_clone = clients_clone.clone();
        peer_connection_clone.lock().await.lock().await.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
            let clients_clone = clients_clone.clone();
            tokio::spawn(async move {    
                if let Some(candidate) = candidate {
                    // Burada, ICE adayını JSON olarak istemciye gönderin
                    let clients_lock = clients_clone.lock().await;
                    let client_tx = clients_lock.get(&addr).unwrap();
                    let candidate = IceCandidateMessage {
                        msg_type: "candidate".to_string(),
                        content: candidate.to_json().unwrap().candidate
                    };
                    client_tx.unbounded_send(Message::Text(serde_json::to_string(&candidate).unwrap())).unwrap();
                }
            });

            Box::pin(async move {})
        }));
    });
    

    // İstemciden gelen mesajları dinle
    let ws_sender_clone = Arc::clone(&ws_sender);
    let peer_connection_clone = Arc::clone(&peer_connection);
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
                                        "ping" => {
                                            println!("{} adresinden ping alındı", addr);
                                            let mut ws_sender = ws_sender_clone.lock().await;
                                            if let Err(e) = ws_sender.send(Message::Text(String::from("{ 'msg_type': 'pong', 'content': [] }"))).await {
                                                println!("Pong gönderme hatası: {}", e);
                                                break;
                                            }
                                        }
                                        "chat" => {
                                            println!("Bilinmeyen msg_type: {}", msg_type);
                                        }
                                        "offer" => {
                                            let content = serde_json::from_value::<OfferAnswerMessageJson>(value.clone()).unwrap().content;

                                            let peer_connection = peer_connection_clone.lock().await;
                                            
                                            let sdp: RTCSessionDescription = RTCSessionDescription::offer(content.sdp).unwrap();
                                            println!("İstemciden SDP alındı - {:?}", sdp);
                                            peer_connection.lock().await.set_remote_description(sdp).await.unwrap();
                                            let answer = peer_connection.lock().await.create_answer(None).await.unwrap();
                                            let answerrr = OfferAnswerMessageJson
                                            {
                                                msg_type: String::from("answer"),
                                                content: OfferAnswerContentMessageJson
                                                {
                                                    r#type: String::from("answer"),
                                                    sdp: answer.sdp.clone()
                                                }
                                            };
                                            peer_connection.lock().await.set_local_description(answer.clone()).await.unwrap();
                                            //let answerr = { msg_type: "answer", content: { type: "answer", sdp: ".to_owned() + &answer.sdp + " } };
                                            let clients_lock = clients.lock().await;
                                            let client_tx = clients_lock.get(&addr).unwrap();
                                            client_tx.unbounded_send(Message::Text(serde_json::to_string(&answerrr).unwrap())).unwrap();
                                        }
                                        "candidate" => {
                                            let candidate = serde_json::from_value::<IceCandidateMessage>(value.clone()).unwrap();
                                            let peer_connection = peer_connection_clone.lock().await;
                                            //let candidate_parts: Vec<&str> = candidate.content.split_whitespace().collect();
                                            //let username = candidate_parts[11];
                                            let candidatee = RTCIceCandidateInit { candidate: candidate.content.clone(), sdp_mid: Some("0".to_string()), sdp_mline_index: Some(0), ..Default::default()/*, username_fragment: Some(username.to_string())*/ };
                                            if Ok(()) == peer_connection.lock().await.add_ice_candidate(candidatee).await {
                                                println!("ICE adayı eklendi");
                                            } else {
                                                println!("ICE adayı eklenemedi");
                                            }
                                        }
                                        _ => {
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
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
//use webrtc::data_channel::data_channel_message::DataChannelMessage;

async fn create_peer_connection(/*clients: Clients, addr: SocketAddr*/) -> Result<Arc<Mutex<RTCPeerConnection>>, Box<dyn std::error::Error>> {
    //let config = RTCConfiguration::default();

    let mut config = RTCConfiguration::default();
    config.ice_servers.push(RTCIceServer {
        urls: vec![String::from("stun:stun.l.google.com:19302")],
        ..Default::default()
    });

    let api = APIBuilder::new().build();
    
    // PeerConnection oluştur
    let peer_connection = api.new_peer_connection(config).await.unwrap();

    // Data Channel oluştur
    //let data_channel = peer_connection.create_data_channel("chat", None).await?;

    // Data Channel mesajlarını işleyin
    /*data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
        Box::pin(async move {
            println!("DataChannel mesajı alındı: {:?}", msg);
        })
    }));*/

    let peer_connection_arc= Arc::new(Mutex::new(peer_connection));
    

    /*peer_connection.on_track(Box::new(move |track: Arc<TrackRemote>, receiver: Arc<RTCRtpReceiver>, _: Arc<RTCRtpTransceiver>| {
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
    }));*/

    // ICE adayları ekleme
    /*peer_connection.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
        println!("ICE adayı alındı: {:?}", candidate);
        let ws_sender_clone = Arc::clone(&ws_sender_clone);
        Box::pin(async move {
            if let Some(candidate) = candidate {
                // Burada, ICE adayını JSON olarak istemciye gönderin
                println!("ICE adayı alındı: {:?}", candidate);
                let mut ws_sender = ws_sender_clone.lock().await;
                let candidate = "{ msg_type: \"candidate\", content: \"{ ".to_owned() + &candidate.to_string() + "}\" }";
                ws_sender.send(Message::Text(serde_json::to_string(&candidate).unwrap())).await.unwrap();
            }
        })
    }));*/

    Ok(peer_connection_arc)
}

// RTP paketlerini işlemek için bir fonksiyon tanımlayın
fn handle_rtp_packet(packet: &Packet) {
    // Burada RTP paketini işleyin
    // Örneğin, payload'u veya timestamp'ı kontrol edebilir veya ek bilgi ekleyebilirsiniz
    println!("RTP Paketi Alındı: {}", packet);
}