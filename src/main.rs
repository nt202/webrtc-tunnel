use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let http_client = reqwest::Client::new();
    
    // STUN от Google
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };
    
    let api = APIBuilder::new().build();
    let peer_connection = Arc::new(api.new_peer_connection(config).await?);
    
    // Создаем data channel
    let data_channel = peer_connection.create_data_channel("http-tunnel", None).await?;
    let dc_clone = data_channel.clone();
    
    // Обработка сообщений от клиента
    data_channel.on_message(Box::new(move |msg| {
        let http_client = http_client.clone();
        let dc = dc_clone.clone();
        Box::pin(async move {
            let url = String::from_utf8_lossy(&msg.data);
            if let Ok(resp) = http_client.get(format!("http://localhost:9090{}", url)).send().await {
                if let Ok(text) = resp.text().await {
                    let _ = dc.send_text(text).await;
                }
            }
        })
    }));
    
    // Создаем offer
    let offer = peer_connection.create_offer(None).await?;
    peer_connection.set_local_description(offer).await?;
    
    // Ждем ICE gathering
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    
    // Печатаем SDP offer (копировать в браузер)
    let local_desc = peer_connection.local_description().await.unwrap();
    let sdp = serde_json::to_string(&local_desc)?;
    println!("\n=== COPY TO BROWSER ===\n{}\n", sdp);
    
    // Ждем answer из stdin
    println!("Paste aswer here and press Enter:");
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let answer_json = lines.next_line().await?.unwrap();
    let answer: RTCSessionDescription = serde_json::from_str(&answer_json)?;
    
    peer_connection.set_remote_description(answer).await?;
    
    // Ждем соединения через канал
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pc = peer_connection.clone();
    pc.on_peer_connection_state_change(Box::new(move |state| {
        let tx = tx.clone();
        Box::pin(async move {
            if state == RTCPeerConnectionState::Connected {
                let _ = tx.send(());
            }
        })
    }));
    
    rx.recv().await;
    println!("Tunnel ready. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    
    Ok(())
}
