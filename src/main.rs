use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock};
use tokio_tungstenite::tungstenite::Message;
use webrtc::api::media_engine::MediaEngine;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::offer_answer_options::RTCAnswerOptions;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp::packet::Packet;

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
struct OfferSignalMessage {
    msg_type: String,
    content: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct AnswerSignalMessage {
    msg_type: String,
    content: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct IceCandidateSignalMessage {
    msg_type: String,
    content: String,
    sdp_mid: Option<String>,
    sdp_mline_index: Option<u16>,
    username_fragment: Option<String>,
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

type Clients = Arc<RwLock<HashMap<SocketAddr, futures::channel::mpsc::UnboundedSender<Message>>>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Sunucuyu localhost:8080'de başlat
    //let addr = "0.0.0.0:8080";
    let addr = "localhost:8080";
    let listener = TcpListener::bind(&addr).await?;
    println!("WebSocket sunucusu şurada çalışıyor: {}", addr);

    // Bağlı istemcilerin listesini tut
    let clients: Clients = Arc::new(RwLock::new(HashMap::new()));

    // Yeni bağlantıları kabul et
    while let Ok((stream, addr)) = listener.accept().await {
        println!("Yeni bağlantı: {}", addr);

        tokio::spawn(handle_connection(stream, addr, clients.clone()));
    }

    Ok(())
}

async fn handle_connection(
    stream: TcpStream,
    addr: SocketAddr,
    clients: Arc<RwLock<HashMap<SocketAddr, futures::channel::mpsc::UnboundedSender<Message>>>>,
) {
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
    clients.write().await.insert(addr, tx);

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
    let peer_connection: RTCPeerConnection = match create_peer_connection().await {
        Ok(pc) => pc,
        Err(e) => {
            println!("Peer bağlantısı oluşturulamadı: {}", e);
            return;
        }
    };

    peer_connection.on_track(Box::new(move |track, _, _| {
        Box::pin(async move {
            println!("Track id: {}", track.id());
            let track_clone = track.clone();
            loop {
                match track_clone.read_rtp().await {
                    Ok((rtp_packet, _)) => {
                        // RTP paketini işleyin
                        println!("RTP Paketi Alındı");
                        handle_rtp_packet(&rtp_packet);
                    }
                    Err(e) => {
                        // Hata durumu loglanıyor
                        eprintln!("RTP Okuma Hatası: {:?}", e);
                        break; // Hata durumunda döngüyü sonlandırıyoruz
                    }
                }
            }
        })
    }));

    let clients_arc = Arc::clone(&clients);
    peer_connection.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
        let clients = clients_arc.clone();
        Box::pin(async move {
            if let Some(candidate) = candidate {
                let candidate = IceCandidateSignalMessage {
                    msg_type: "candidate".to_string(),
                    content: candidate.to_json().unwrap().candidate,
                    sdp_mid: candidate.to_json().unwrap().sdp_mid,
                    sdp_mline_index: candidate.to_json().unwrap().sdp_mline_index,
                    username_fragment: candidate.to_json().unwrap().username_fragment,
                };
                
                let clients = clients.read().await;
                let client_tx = clients.get(&addr).unwrap();
                client_tx
                    .unbounded_send(Message::Text(serde_json::to_string(&candidate).unwrap()))
                    .unwrap();
            }
        })
    }));

    peer_connection.on_ice_connection_state_change(Box::new(move |ice_connection_state| {
        Box::pin(async move {
            println!("ICE Connection State: {:?}", ice_connection_state);
        })
    }));

    peer_connection.on_ice_gathering_state_change(Box::new(move |ice_gatherer_state| {
        Box::pin(async move {
            println!("ICE Gatherer State: {:?}", ice_gatherer_state);
        })
    }));

    let peer_connection_arc = Arc::new(RwLock::new(peer_connection));

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
                                if let Some(msg_type) =
                                    value.get("msg_type").and_then(|v| v.as_str())
                                {
                                    match msg_type {
                                        "ping" => {
                                            println!("{} adresinden ping alındı", addr);
                                            let mut ws_sender = ws_sender_clone.lock().await;
                                            if let Err(e) = ws_sender
                                                .send(Message::Text(String::from(
                                                    "{ 'msg_type': 'pong', 'content': [] }",
                                                )))
                                                .await
                                            {
                                                println!("Pong gönderme hatası: {}", e);
                                                break;
                                            }
                                        }
                                        "chat" => {
                                            // Chat mesajı olarak işleyin
                                            if let Ok(chat_msg) =
                                                serde_json::from_value::<ChatMessage>(value.clone())
                                            {
                                                println!(
                                                    "İstemciden alınan chat mesajı: {:?}",
                                                    chat_msg
                                                );

                                                // Mesajı diğer istemcilere gönder
                                                for (client_addr, client_tx) in
                                                    clients.read().await.iter()
                                                {
                                                    if *client_addr != addr {
                                                        let json_msg =
                                                            serde_json::to_string(&chat_msg)
                                                                .unwrap();
                                                        if let Err(e) = client_tx
                                                            .unbounded_send(Message::Text(json_msg))
                                                        {
                                                            println!(
                                                                "Broadcast hatası {}: {}",
                                                                client_addr, e
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        "offer" => {
                                            if let Ok(message) =
                                                serde_json::from_value::<OfferSignalMessage>(
                                                    value.clone(),
                                                )
                                            {
                                                let remote_sdp_string = message.content;
                                                let pc = peer_connection_arc.write().await;
                                                let remote_sdp: RTCSessionDescription =
                                                    RTCSessionDescription::offer(remote_sdp_string)
                                                        .unwrap();
                                                if Ok(())
                                                    == pc.set_remote_description(remote_sdp).await
                                                {
                                                    println!("set_remote_description");
                                                    if let Ok(local_sdp) = pc
                                                        .create_answer(Some(RTCAnswerOptions {
                                                            ..Default::default()
                                                        }))
                                                        .await
                                                    {
                                                        println!("create_answer");
                                                        if Ok(())
                                                            == pc
                                                                .set_local_description(
                                                                    local_sdp.clone(),
                                                                )
                                                                .await
                                                        {
                                                            println!("set_local_description");
                                                            if let Some(client_tx) =
                                                                clients.read().await.get(&addr)
                                                            {
                                                                println!("unbounded_send");
                                                                let local_sdp_answer =
                                                                    AnswerSignalMessage {
                                                                        msg_type: "answer"
                                                                            .to_string(),
                                                                        content: local_sdp.sdp,
                                                                    };
                                                                client_tx
                                                                    .unbounded_send(Message::Text(
                                                                        serde_json::to_string(
                                                                            &local_sdp_answer,
                                                                        )
                                                                        .expect(
                                                                            "SDP oluşturulamadı",
                                                                        ),
                                                                    ))
                                                                    .expect("SDP gönderilemedi");
                                                            } else {
                                                                println!("İstemci bulunamadı");
                                                            }
                                                        } else {
                                                            println!(
                                                                "Local description ayarlanamadı"
                                                            );
                                                        }
                                                    } else {
                                                        println!(
                                                            "Local description oluşturulamadı"
                                                        );
                                                    }
                                                } else {
                                                    println!("Remote description ayarlanamadı");
                                                }
                                            } else {
                                                println!("Offer ayrıştırılamadı");
                                            }
                                        }
                                        "candidate" => {
                                            if let Ok(message) =
                                                serde_json::from_value::<IceCandidateSignalMessage>(
                                                    value.clone(),
                                                )
                                            {
                                                let candidatee = RTCIceCandidateInit {
                                                    candidate: message.content.clone(),
                                                    sdp_mid: message.sdp_mid.clone(),
                                                    sdp_mline_index: message.sdp_mline_index,
                                                    username_fragment: message.username_fragment,
                                                };
                                                if Ok(())
                                                    == peer_connection_arc
                                                        .write()
                                                        .await
                                                        .add_ice_candidate(candidatee)
                                                        .await
                                                {
                                                    println!("ICE adayı eklendi");
                                                } else {
                                                    println!("ICE adayı eklenemedi");
                                                }
                                            } else {
                                                println!("ICE ayrıştırılamadı");
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
                        peer_connection_arc.read().await.close().await.unwrap();
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
    clients.write().await.remove(&addr);
    peer_connection_arc.write().await.close().await.unwrap();
    broadcast_task.abort();
}

use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
//use webrtc::data_channel::data_channel_message::DataChannelMessage;

async fn create_peer_connection(/*clients: Clients, addr: SocketAddr*/
) -> Result<RTCPeerConnection, Box<dyn std::error::Error>> {
    //let config = RTCConfiguration::default();

    let stun_server_1 = RTCIceServer {
        urls: vec!["stun:stun.l.google.com:19302".to_string()],
        ..Default::default()
    };

    let config = RTCConfiguration {
        ice_servers: vec![stun_server_1],
        ..Default::default()
    };

    let mut media_engine = MediaEngine::default();

    media_engine.register_default_codecs()?;

    let api = APIBuilder::new().with_media_engine(media_engine).build();

    // PeerConnection oluştur
    let peer_connection = api.new_peer_connection(config).await.unwrap();

    // Add audio transceiver
    /*peer_connection
        .add_transceiver_from_kind(
            RTPCodecType::Audio,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                send_encodings: vec![]
            }),
        )
        .await?;

    // Add video transceiver
    peer_connection
        .add_transceiver_from_kind(
            RTPCodecType::Video,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                send_encodings: vec![],
            }),
        )
        .await?;*/

    // Data Channel oluştur
    //let data_channel = peer_connection.create_data_channel("chat", None).await?;

    // Data Channel mesajlarını işleyin
    /*data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
        Box::pin(async move {
            println!("DataChannel mesajı alındı: {:?}", msg);
        })
    }));*/

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

    Ok(peer_connection)
}

// RTP paketlerini işlemek için bir fonksiyon tanımlayın
fn handle_rtp_packet(packet: &Packet) {
    // Burada RTP paketini işleyin
    // Örneğin, payload'u veya timestamp'ı kontrol edebilir veya ek bilgi ekleyebilirsiniz
    println!("RTP Paketi Alındı: {}", packet);
}
