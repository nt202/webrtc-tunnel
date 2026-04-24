use webrtc::api::APIBuilder;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::ice::network_type::NetworkType;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::io::{AsyncBufReadExt, BufReader};

fn format_hex(data: &[u8], pairs_per_line: usize) -> String {
    let hex_pairs: Vec<String> = data.iter()
        .map(|b| format!("{:02x}", b))
        .map(|it| it.to_uppercase())
        .collect();

    hex_pairs
        .chunks(pairs_per_line)
        .map(|chunk| chunk.join("-"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let http_client = reqwest::Client::new();
    
    let mut setting_engine = SettingEngine::default();
    setting_engine.set_network_types(vec![NetworkType::Udp4]);
    
    let api = APIBuilder::new()
        .with_setting_engine(setting_engine)
        .build();
    
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec![
                "stun:stun.sipgate.net:10000".to_owned(),
                "stun:stun.ekiga.net:3478".to_owned(),
                "stun:stun.nextcloud.com:3478".to_owned(),
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    
    let peer_connection = Arc::new(api.new_peer_connection(config).await?);
    
    // Ждём data channel от браузера
    let (dc_tx, mut dc_rx) = mpsc::unbounded_channel();
    peer_connection.on_data_channel(Box::new(move |dc| {
        let tx = dc_tx.clone();
        Box::pin(async move {
            let _ = tx.send(dc);
        })
    }));
    
    // Читаем offer из stdin
    println!("Paste offer from browser and press Enter:");
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let offer_json = lines.next_line().await?.unwrap();
    let offer: RTCSessionDescription = serde_json::from_str(&offer_json)?;
    
    peer_connection.set_remote_description(offer).await?;
    
    let answer = peer_connection.create_answer(None).await?;
    peer_connection.set_local_description(answer).await?;
    
    // Ждём ICE gathering
    let (ice_tx, mut ice_rx) = mpsc::unbounded_channel::<()>();
    peer_connection.on_ice_candidate(Box::new(move |c| {
        let tx = ice_tx.clone();
        Box::pin(async move {
            if c.is_none() { let _ = tx.send(()); }
        })
    }));
    
    tokio::select! {
        _ = ice_rx.recv() => {},
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {},
    }
    
    let local_desc = peer_connection.local_description().await.unwrap();
    let sdp = serde_json::to_string(&local_desc)?;
    let sdp_bytes = sdp.as_bytes();
    
    // JSON
    println!("\n=== COPY TO BROWSER (JSON) ===\n{}\n", sdp);
    
    // HEX
    let formatted = format_hex(sdp_bytes, 30);
    println!("=== COPY TO BROWSER (HEX) ===\n\n{}\n", formatted);
    
    // Ждём data channel
    let data_channel = dc_rx.recv().await.unwrap();
    
    // Настраиваем HTTP-прокси
    let dc_clone = data_channel.clone();
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
    
    // Ждём подключения
    let (tx, mut rx) = mpsc::unbounded_channel();
    peer_connection.on_peer_connection_state_change(Box::new(move |state| {
        let tx = tx.clone();
        Box::pin(async move {
            println!("State: {:?}", state);
            if state == RTCPeerConnectionState::Connected {
                let _ = tx.send(());
            }
        })
    }));
    
    rx.recv().await;
    println!("Tunnel ready!");
    tokio::signal::ctrl_c().await?;
    
    Ok(())
}
