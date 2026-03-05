// 下点心 (XiaDianxin) — Tauri v2 Backend
//
// TCP signaling relay for WebRTC, mDNS peer discovery (IPv4+IPv6),
// local profile storage, 9-digit calling codes.
// Sankaku/RT H.265 transport is stubbed at marked integration points.

use base64::Engine;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MDNS_SERVICE_TYPE: &str = "_xiadianxin._udp.local.";

// ---------------------------------------------------------------------------
// Shared types (mirrored 1:1 in src/types/call.ts)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub display_name: String,
    pub calling_code: String,
    pub avatar_id: String,
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredPeer {
    pub calling_code: String,
    pub display_name: String,
    pub addresses: Vec<String>,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingCallPayload {
    pub peer_id: String,
    pub peer_name: String,
    pub peer_avatar: Option<String>,
    pub audio_only: bool,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallResult {
    pub success: bool,
    pub message: String,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicemailResult {
    pub success: bool,
    pub message: String,
    pub recording_id: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Friend {
    pub calling_code: String,
    pub display_name: String,
    pub avatar_id: String,
    pub public_key: Option<String>,
    pub approved: bool,
    pub added_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: String,
    pub from: String,
    pub text: Option<String>,
    pub sticker: Option<String>,
    pub file_name: Option<String>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceivedFileEntry {
    pub file_name: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified_at: u64,
    pub sha256: String,
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum CoreState {
    Uninitialized,
    Ready,
    InCall,
    Recording,
}

struct AppState {
    core_state: CoreState,
    profile: UserProfile,
    profile_path: PathBuf,
    friends_path: PathBuf,
    conversations_dir: PathBuf,
    received_files_dir: PathBuf,
    download_dir_config_path: PathBuf,
    instance_id: String,
    #[allow(dead_code)]
    signaling_port: u16,
    discovered_peers: HashMap<String, DiscoveredPeer>,
    peer_instances: HashMap<String, String>,
    peer_services: HashMap<String, String>,
    active_peer: Option<String>,
    active_connection: Option<Arc<Mutex<TcpStream>>>,
    session_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn generate_calling_code() -> String {
    let mut rng = rand::thread_rng();
    format!(
        "{:03}-{:03}-{:03}",
        rng.gen_range(0..1000u32),
        rng.gen_range(0..1000u32),
        rng.gen_range(0..1000u32),
    )
}

fn generate_instance_id() -> String {
    let mut rng = rand::thread_rng();
    format!("{:016x}", rng.gen::<u64>())
}

fn hostname_or_default() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "User".into())
}

fn default_profile() -> UserProfile {
    UserProfile {
        display_name: hostname_or_default(),
        calling_code: generate_calling_code(),
        avatar_id: "kyu-kun".into(),
        language: "en".into(),
    }
}

fn profile_dir(app: &tauri::App) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn load_profile(path: &PathBuf) -> UserProfile {
    match fs::read_to_string(path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_else(|_| default_profile()),
        Err(_) => default_profile(),
    }
}

fn save_profile(path: &PathBuf, profile: &UserProfile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(profile).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

fn load_download_directory(path: &PathBuf) -> Option<PathBuf> {
    let configured = fs::read_to_string(path).ok()?;
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn save_download_directory(path: &PathBuf, directory: &PathBuf) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(path, directory.to_string_lossy().as_bytes()).map_err(|e| e.to_string())
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn stub_session_id() -> String {
    format!("snk-{:x}", epoch_secs())
}

fn format_socket_addr(ip: &str, port: u16) -> String {
    if ip.contains(':') {
        format!("[{}]:{}", ip, port)
    } else {
        format!("{}:{}", ip, port)
    }
}

// ---------------------------------------------------------------------------
// Friends storage
// ---------------------------------------------------------------------------

fn load_friends(path: &PathBuf) -> Vec<Friend> {
    match fs::read_to_string(path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_friends_to_disk(path: &PathBuf, friends: &[Friend]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(friends).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Conversation storage
// ---------------------------------------------------------------------------

fn load_messages_from_disk(dir: &PathBuf, peer_code: &str) -> Vec<StoredMessage> {
    let safe = peer_code.replace(|c: char| !c.is_alphanumeric() && c != '-', "_");
    let path = dir.join(format!("{}.json", safe));
    match fs::read_to_string(&path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_messages_to_disk(
    dir: &PathBuf,
    peer_code: &str,
    messages: &[StoredMessage],
) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let safe = peer_code.replace(|c: char| !c.is_alphanumeric() && c != '-', "_");
    let path = dir.join(format!("{}.json", safe));
    let json = serde_json::to_string_pretty(messages).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())
}

fn get_conversation_peer_codes(dir: &PathBuf) -> Vec<String> {
    let mut codes = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.path().file_stem().and_then(|s| s.to_str()) {
                let code = name.replace('_', "-");
                codes.push(code);
            }
        }
    }
    codes
}

// ---------------------------------------------------------------------------
// TCP signaling relay
// ---------------------------------------------------------------------------

fn spawn_reader_thread(
    stream: TcpStream,
    handle: AppHandle,
    state: Arc<Mutex<AppState>>,
) {
    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            match line {
                Ok(msg) if !msg.is_empty() => {
                    let _ = handle.emit("signaling-message", &msg);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if let Ok(mut s) = state.lock() {
            s.active_connection = None;
            s.active_peer = None;
            if s.core_state == CoreState::InCall {
                s.core_state = CoreState::Ready;
            }
        }
        let _ = handle.emit("signaling-disconnected", ());
    });
}

fn start_signaling_loop(
    handle: AppHandle,
    state: Arc<Mutex<AppState>>,
    listener: TcpListener,
) {
    thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };

            let writer_stream = match stream.try_clone() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let writer = Arc::new(Mutex::new(writer_stream));

            if let Ok(mut s) = state.lock() {
                s.active_connection = Some(Arc::clone(&writer));
            }

            let _ = handle.emit("signaling-connected", ());
            spawn_reader_thread(stream, handle.clone(), Arc::clone(&state));
        }
    });
}

// ---------------------------------------------------------------------------
// mDNS service registration & discovery
// ---------------------------------------------------------------------------

fn start_mdns(handle: AppHandle, state: Arc<Mutex<AppState>>, port: u16) {
    let (profile, instance_id) = {
        let s = state.lock().unwrap();
        (s.profile.clone(), s.instance_id.clone())
    };
    let local_calling_code = profile.calling_code.clone();

    thread::spawn(move || {
        let daemon = match ServiceDaemon::new() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[mDNS] failed to start daemon: {e}");
                return;
            }
        };

        let props = [
            ("code", profile.calling_code.as_str()),
            ("name", profile.display_name.as_str()),
            ("inst", instance_id.as_str()),
        ];

        let host = format!("xdx-{}.local.", &instance_id[..8]);

        match ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            &instance_id,
            &host,
            "",
            port,
            &props[..],
        ) {
            Ok(service) => {
                let service = service.enable_addr_auto();
                if let Err(e) = daemon.register(service) {
                    eprintln!("[mDNS] register failed: {e}");
                }
            }
            Err(e) => {
                eprintln!("[mDNS] ServiceInfo build failed: {e}");
            }
        }

        let receiver = match daemon.browse(MDNS_SERVICE_TYPE) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[mDNS] browse failed: {e}");
                return;
            }
        };

        loop {
            match receiver.recv() {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    let service_fullname = info.get_fullname().to_string();
                    let remote_inst = info
                        .get_properties()
                        .get("inst")
                        .map(|v| v.val_str().to_string())
                        .unwrap_or_default();

                    if !remote_inst.is_empty() && remote_inst == instance_id {
                        continue;
                    }

                    let code = info
                        .get_properties()
                        .get("code")
                        .map(|v| v.val_str().to_string())
                        .unwrap_or_default();

                    if code.is_empty() {
                        continue;
                    }
                    if code == local_calling_code {
                        continue;
                    }

                    let name = info
                        .get_properties()
                        .get("name")
                        .map(|v| v.val_str().to_string())
                        .unwrap_or_else(|| "Unknown".into());

                    let addresses: Vec<String> =
                        info.get_addresses().iter().map(|a| a.to_string()).collect();

                    let peer = DiscoveredPeer {
                        calling_code: code.clone(),
                        display_name: name,
                        addresses,
                        port: info.get_port(),
                    };

                    if let Ok(mut s) = state.lock() {
                        s.discovered_peers.insert(code.clone(), peer.clone());
                        if !remote_inst.is_empty() {
                            s.peer_instances
                                .insert(remote_inst.clone(), code.clone());
                        }
                        s.peer_services
                            .insert(service_fullname.clone(), code.clone());
                    }
                    let _ = handle.emit("peer-discovered", &peer);
                }
                Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                    let removed_code: Option<String> = {
                        if let Ok(s) = state.lock() {
                            s.peer_services
                                .get(&fullname)
                                .cloned()
                                .or_else(|| {
                                    s.peer_instances
                                .iter()
                                .find(|(inst, _)| fullname.contains(inst.as_str()))
                                .map(|(_, code)| code.clone())
                                })
                        } else {
                            None
                        }
                    };
                    if let Some(code) = removed_code {
                        if let Ok(mut s) = state.lock() {
                            s.discovered_peers.remove(&code);
                            s.peer_instances.retain(|_, v| v != &code);
                            s.peer_services.retain(|_, v| v != &code);
                        }
                        let _ = handle.emit("peer-lost", &code);
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "camelCase")]
fn init_sankaku_core(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<CallResult, String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.core_state = CoreState::Ready;
    Ok(CallResult {
        success: true,
        message: "Sankaku/RT core initialised".into(),
        session_id: None,
    })
}

#[tauri::command(rename_all = "camelCase")]
fn get_profile(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<UserProfile, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(s.profile.clone())
}

#[tauri::command(rename_all = "camelCase")]
fn update_profile(
    display_name: Option<String>,
    avatar_id: Option<String>,
    language: Option<String>,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<UserProfile, String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    if let Some(name) = display_name {
        s.profile.display_name = name;
    }
    if let Some(avatar) = avatar_id {
        s.profile.avatar_id = avatar;
    }
    if let Some(lang) = language {
        s.profile.language = lang;
    }
    save_profile(&s.profile_path, &s.profile)?;
    Ok(s.profile.clone())
}

#[tauri::command(rename_all = "camelCase")]
fn get_discovered_peers(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<Vec<DiscoveredPeer>, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(s.discovered_peers.values().cloned().collect())
}

#[tauri::command(rename_all = "camelCase")]
fn connect_to_peer(
    code: String,
    handle: AppHandle,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<CallResult, String> {
    let peer = {
        let s = state.lock().map_err(|e| e.to_string())?;
        s.discovered_peers
            .get(&code)
            .cloned()
            .ok_or_else(|| format!("Peer {} not found on local network", code))?
    };

    if peer.addresses.is_empty() {
        return Err("No addresses available for peer".into());
    }

    // Sort addresses: prefer IPv4 over IPv6, skip link-local IPv6
    let mut candidates: Vec<String> = peer
        .addresses
        .iter()
        .filter(|a| {
            if let Ok(ip) = a.parse::<IpAddr>() {
                if let IpAddr::V6(v6) = ip {
                    return !v6.to_string().starts_with("fe80");
                }
            }
            true
        })
        .cloned()
        .collect();
    candidates.sort_by_key(|a| if a.contains(':') { 1u8 } else { 0u8 });

    let mut last_err = String::from("No routable addresses");
    for addr in &candidates {
        let connect_addr = format_socket_addr(addr, peer.port);
        let sock_addr = match connect_addr.parse::<std::net::SocketAddr>() {
            Ok(a) => a,
            Err(_) => continue,
        };
        match TcpStream::connect_timeout(&sock_addr, Duration::from_secs(4)) {
            Ok(stream) => {
                let writer = Arc::new(Mutex::new(
                    stream.try_clone().map_err(|e| e.to_string())?,
                ));
                let sid = stub_session_id();
                {
                    let mut s = state.lock().map_err(|e| e.to_string())?;
                    s.active_connection = Some(Arc::clone(&writer));
                    s.active_peer = Some(code.clone());
                    s.session_id = Some(sid.clone());
                }
                spawn_reader_thread(stream, handle, Arc::clone(state.inner()));
                return Ok(CallResult {
                    success: true,
                    message: format!("{} ({})", peer.display_name, code),
                    session_id: Some(sid),
                });
            }
            Err(e) => {
                last_err = format!("{} -> {}", connect_addr, e);
                eprintln!("[XDX] connect attempt failed: {}", last_err);
            }
        }
    }

    Err(format!("All connection attempts failed: {}", last_err))
}

#[tauri::command(rename_all = "camelCase")]
fn send_signal(
    message: String,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<(), String> {
    let conn = {
        let s = state.lock().map_err(|e| e.to_string())?;
        s.active_connection.clone()
    };
    if let Some(conn) = conn {
        let mut stream = conn.lock().map_err(|e| e.to_string())?;
        writeln!(stream, "{}", message).map_err(|e| e.to_string())?;
        stream.flush().map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("No active signaling connection".into())
    }
}

#[tauri::command(rename_all = "camelCase")]
fn disconnect_signal(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<(), String> {
    let conn = {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        s.active_connection.take()
    };
    if let Some(conn) = conn {
        if let Ok(stream) = conn.lock() {
            stream.shutdown(Shutdown::Both).ok();
        }
    }
    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.active_peer = None;
    if s.core_state == CoreState::InCall {
        s.core_state = CoreState::Ready;
    }
    s.session_id = None;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
fn dial_code(
    code: String,
    _audio_only: bool,
    handle: AppHandle,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<CallResult, String> {
    connect_to_peer(code, handle, state)
}

#[tauri::command(rename_all = "camelCase")]
fn start_call(
    peer_id: String,
    _audio_only: bool,
    handle: AppHandle,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<CallResult, String> {
    connect_to_peer(peer_id, handle, state)
}

#[tauri::command(rename_all = "camelCase")]
fn accept_call(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<CallResult, String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    let sid = stub_session_id();
    s.core_state = CoreState::InCall;
    s.session_id = Some(sid.clone());
    Ok(CallResult {
        success: true,
        message: "Call accepted".into(),
        session_id: Some(sid),
    })
}

#[tauri::command(rename_all = "camelCase")]
fn end_call(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<CallResult, String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    let old = s.session_id.take();
    s.core_state = CoreState::Ready;
    Ok(CallResult {
        success: true,
        message: format!("Session {old:?} ended"),
        session_id: old,
    })
}

#[tauri::command(rename_all = "camelCase")]
fn record_voicemail(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<VoicemailResult, String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.core_state = CoreState::Recording;
    Ok(VoicemailResult {
        success: true,
        message: "Voicemail recording started".into(),
        recording_id: Some(format!("vm-{:x}", epoch_secs())),
        duration_ms: 0,
    })
}

#[tauri::command(rename_all = "camelCase")]
fn stop_voicemail(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<VoicemailResult, String> {
    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.core_state = CoreState::Ready;
    Ok(VoicemailResult {
        success: true,
        message: "Voicemail saved".into(),
        recording_id: None,
        duration_ms: 0,
    })
}

// ---------------------------------------------------------------------------
// Friends commands
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "camelCase")]
fn get_friends(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<Vec<Friend>, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(load_friends(&s.friends_path))
}

#[tauri::command(rename_all = "camelCase")]
fn add_friend(
    calling_code: String,
    display_name: String,
    avatar_id: Option<String>,
    public_key: Option<String>,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<Friend, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    let mut friends = load_friends(&s.friends_path);
    if let Some(existing) = friends.iter_mut().find(|f| f.calling_code == calling_code) {
        existing.approved = true;
        if !display_name.is_empty() {
            existing.display_name = display_name.clone();
        }
        if let Some(avatar) = avatar_id {
            existing.avatar_id = avatar;
        }
        if let Some(key) = public_key {
            existing.public_key = Some(key);
        }
        let friend = existing.clone();
        save_friends_to_disk(&s.friends_path, &friends)?;
        return Ok(friend);
    }
    let friend = Friend {
        calling_code: calling_code.clone(),
        display_name,
        avatar_id: avatar_id.unwrap_or_else(|| "default".into()),
        public_key,
        approved: true,
        added_at: epoch_secs(),
    };
    friends.push(friend.clone());
    save_friends_to_disk(&s.friends_path, &friends)?;
    Ok(friend)
}

#[tauri::command(rename_all = "camelCase")]
fn remove_friend(
    calling_code: String,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<(), String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    let mut friends = load_friends(&s.friends_path);
    friends.retain(|f| f.calling_code != calling_code);
    save_friends_to_disk(&s.friends_path, &friends)
}

#[tauri::command(rename_all = "camelCase")]
fn is_friend(
    calling_code: String,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<bool, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    let friends = load_friends(&s.friends_path);
    Ok(friends.iter().any(|f| f.calling_code == calling_code && f.approved))
}

// ---------------------------------------------------------------------------
// Conversation commands
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "camelCase")]
fn save_messages(
    peer_code: String,
    messages: Vec<StoredMessage>,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<(), String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    save_messages_to_disk(&s.conversations_dir, &peer_code, &messages)
}

#[tauri::command(rename_all = "camelCase")]
fn load_messages(
    peer_code: String,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<Vec<StoredMessage>, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(load_messages_from_disk(&s.conversations_dir, &peer_code))
}

#[tauri::command(rename_all = "camelCase")]
fn list_conversations(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<Vec<String>, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    Ok(get_conversation_peer_codes(&s.conversations_dir))
}

// ---------------------------------------------------------------------------
// File receive command
// ---------------------------------------------------------------------------

fn sanitize_file_name(raw: &str) -> String {
    let mut safe = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.is_empty() {
        safe = "received_file".into();
    }
    safe
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{:02x}", b);
    }
    out
}

#[tauri::command(rename_all = "camelCase")]
fn save_received_file(
    filename: String,
    data_b64: String,
    expected_sha256: Option<String>,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<ReceivedFileEntry, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    fs::create_dir_all(&s.received_files_dir).map_err(|e| e.to_string())?;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data_b64)
        .map_err(|e| format!("base64 decode: {e}"))?;
    let actual_sha256 = sha256_hex(&bytes);
    if let Some(expected) = expected_sha256 {
        if actual_sha256.to_lowercase() != expected.to_lowercase() {
            return Err(format!(
                "SHA-256 mismatch. expected={} actual={}",
                expected, actual_sha256
            ));
        }
    }

    let safe_name = sanitize_file_name(&filename);
    let mut dest = s.received_files_dir.join(&safe_name);
    if dest.exists() {
        let suffix = epoch_secs();
        let stem = dest
            .file_stem()
            .and_then(|v| v.to_str())
            .unwrap_or("received_file");
        let ext = dest.extension().and_then(|v| v.to_str()).unwrap_or("");
        let renamed = if ext.is_empty() {
            format!("{stem}_{suffix}")
        } else {
            format!("{stem}_{suffix}.{ext}")
        };
        dest = s.received_files_dir.join(renamed);
    }
    fs::write(&dest, bytes).map_err(|e| e.to_string())?;

    let metadata = fs::metadata(&dest).map_err(|e| e.to_string())?;
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or_else(epoch_secs);

    Ok(ReceivedFileEntry {
        file_name: dest
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or(&safe_name)
            .to_string(),
        path: dest.to_string_lossy().into_owned(),
        size_bytes: metadata.len(),
        modified_at,
        sha256: actual_sha256,
    })
}

#[tauri::command(rename_all = "camelCase")]
fn list_received_files(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<Vec<ReceivedFileEntry>, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    fs::create_dir_all(&s.received_files_dir).map_err(|e| e.to_string())?;

    let mut entries = Vec::new();
    let read_dir = fs::read_dir(&s.received_files_dir).map_err(|e| e.to_string())?;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let metadata = match fs::metadata(&path) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        entries.push(ReceivedFileEntry {
            file_name: path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("received_file")
                .to_string(),
            path: path.to_string_lossy().into_owned(),
            size_bytes: metadata.len(),
            modified_at,
            sha256: sha256_hex(&bytes),
        });
    }

    entries.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
    Ok(entries)
}

#[tauri::command(rename_all = "camelCase")]
fn set_download_directory(
    path: String,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Download directory path cannot be empty".into());
    }

    let mut directory = PathBuf::from(trimmed);
    if !directory.is_absolute() {
        directory = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(directory);
    }
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;

    let mut s = state.lock().map_err(|e| e.to_string())?;
    s.received_files_dir = directory.clone();
    save_download_directory(&s.download_dir_config_path, &directory)
}

// ---------------------------------------------------------------------------
// Custom avatar
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "camelCase")]
fn save_custom_avatar(
    data_b64: String,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<(), String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    let dir = s.profile_path.parent().unwrap_or(std::path::Path::new("."));
    let path = dir.join("custom_avatar.b64");
    fs::write(&path, &data_b64).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn load_custom_avatar(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<Option<String>, String> {
    let s = state.lock().map_err(|e| e.to_string())?;
    let dir = s.profile_path.parent().unwrap_or(std::path::Path::new("."));
    let path = dir.join("custom_avatar.b64");
    match fs::read_to_string(&path) {
        Ok(data) if !data.is_empty() => Ok(Some(data)),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let data_dir = profile_dir(app);
            let profile_path = data_dir.join("profile.json");
            let friends_path = data_dir.join("friends.json");
            let conversations_dir = data_dir.join("conversations");
            let download_dir_config_path = data_dir.join("download_directory.txt");
            let received_files_dir = load_download_directory(&download_dir_config_path)
                .unwrap_or_else(|| data_dir.join("received_files"));
            let profile = load_profile(&profile_path);
            save_profile(&profile_path, &profile).ok();
            fs::create_dir_all(&conversations_dir).ok();
            fs::create_dir_all(&received_files_dir).ok();

            let instance_id = generate_instance_id();

            // Bind TCP signaling — prefer dual-stack IPv6 (accepts v4+v6 on macOS)
            let listener = TcpListener::bind("[::]:0")
                .or_else(|_| TcpListener::bind("0.0.0.0:0"))
                .expect("Failed to bind TCP signaling listener");
            let signaling_port = listener.local_addr().map(|a| a.port()).unwrap_or(0);

            eprintln!(
                "[XDX] instance={} code={} signal_port={}",
                &instance_id[..8],
                profile.calling_code,
                signaling_port
            );

            let app_state = Arc::new(Mutex::new(AppState {
                core_state: CoreState::Uninitialized,
                profile,
                profile_path,
                friends_path,
                conversations_dir,
                received_files_dir,
                download_dir_config_path,
                instance_id,
                signaling_port,
                discovered_peers: HashMap::new(),
                peer_instances: HashMap::new(),
                peer_services: HashMap::new(),
                active_peer: None,
                active_connection: None,
                session_id: None,
            }));

            start_signaling_loop(
                app.handle().clone(),
                Arc::clone(&app_state),
                listener,
            );
            start_mdns(
                app.handle().clone(),
                Arc::clone(&app_state),
                signaling_port,
            );

            app.manage(app_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            init_sankaku_core,
            get_profile,
            update_profile,
            get_discovered_peers,
            connect_to_peer,
            send_signal,
            disconnect_signal,
            dial_code,
            start_call,
            accept_call,
            end_call,
            record_voicemail,
            stop_voicemail,
            get_friends,
            add_friend,
            remove_friend,
            is_friend,
            save_messages,
            load_messages,
            list_conversations,
            save_received_file,
            list_received_files,
            set_download_directory,
            save_custom_avatar,
            load_custom_avatar,
        ])
        .run(tauri::generate_context!())
        .expect("fatal: 下点心 failed to start");
}
