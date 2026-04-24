use webrtc::api::APIBuilder;
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
    
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };
    
    let api = APIBuilder::new().build();
    let peer_connection = Arc::new(api.new_peer_connection(config).await?);
    
    let data_channel = peer_connection.create_data_channel("http-tunnel", None).await?;
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
    
    let offer = peer_connection.create_offer(None).await?;
    peer_connection.set_local_description(offer).await?;
    
    // === ЖДЁМ СБОРА ICE-КАНДИДАТОВ ===
    let (ice_tx, mut ice_rx) = mpsc::unbounded_channel::<()>();
    let ice_tx2 = ice_tx.clone();
        
    peer_connection.on_ice_candidate(Box::new(move |candidate| {
        let tx = ice_tx2.clone();
        Box::pin(async move {
            match candidate {
                Some(c) => {
                    println!("ICE candidate: {}", c.to_string());
                }
                None => {
                    println!("ICE gathering complete!");
                    let _ = tx.send(());
                }
            }
        })
    }));
    
    // Таймаут на сбор ICE (5 секунд)
    tokio::select! {
        _ = ice_rx.recv() => {},
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            eprintln!("ICE gathering timeout, proceeding with partial candidates");
        }
    }
    
    let local_desc = peer_connection.local_description().await.unwrap();
    let sdp = serde_json::to_string(&local_desc)?;
    let sdp_bytes = sdp.as_bytes();
    
    println!("\n=== COPY TO BROWSER (JSON) ===\n{}\n", sdp);
    
    let formatted = format_hex(sdp_bytes, 30);
    println!("=== COPY TO BROWSER (HEX) ===\n\n{}\n", formatted);
    
    println!("Paste answer here and press Enter:");
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let answer_json = lines.next_line().await?.unwrap();
    let answer: RTCSessionDescription = serde_json::from_str(&answer_json)?;
    
    peer_connection.set_remote_description(answer).await?;
    
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