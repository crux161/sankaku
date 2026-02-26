use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sankaku_core::{
    extract_sao_parameters, nal_unit_type, parse_psk_hex, split_annex_b, SankakuReceiver,
    SankakuSender, SaoParameters, VideoFrame, VideoPayloadKind,
};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "sankaku-cli", about = "Sankaku realtime frame transport CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Send {
        #[arg(long, default_value = "127.0.0.1:8080")]
        dest: String,
        #[arg(long)]
        psk: Option<String>,
        #[arg(long)]
        ticket_in: Option<String>,
        #[arg(long)]
        ticket_out: Option<String>,
        #[arg(long, default_value_t = 120)]
        frames: u32,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        #[arg(long, default_value_t = 1200)]
        payload_bytes: usize,
        #[arg(long, default_value_t = false)]
        keyframe_every_30: bool,
        #[arg(long, default_value_t = false)]
        sao: bool,
    },
    Recv {
        #[arg(long, default_value = "0.0.0.0:8080")]
        bind: String,
        #[arg(long)]
        psk: Option<String>,
        #[arg(long)]
        ticket_key: Option<String>,
        #[arg(long, default_value_t = 0)]
        max_frames: u64,
    },
    InspectHevc {
        #[arg(long)]
        file: String,
    },
    DumpSaoDataset {
        #[arg(long)]
        input: String,
        #[arg(long)]
        output: String,
    },
}

fn resolve_psk(explicit: Option<String>) -> Result<[u8; 32]> {
    if let Some(value) = explicit {
        return parse_psk_hex(&value);
    }
    if let Ok(value) = std::env::var("SANKAKU_PSK") {
        return parse_psk_hex(&value);
    }
    if let Ok(value) = std::env::var("KYU2_PSK") {
        return parse_psk_hex(&value);
    }
    anyhow::bail!("Missing PSK: pass --psk or set SANKAKU_PSK");
}

fn resolve_ticket_key(explicit: Option<String>, psk: [u8; 32]) -> Result<[u8; 32]> {
    if let Some(value) = explicit {
        return parse_psk_hex(&value);
    }
    if let Ok(value) = std::env::var("SANKAKU_TICKET_KEY") {
        return parse_psk_hex(&value);
    }
    if let Ok(value) = std::env::var("KYU2_TICKET_KEY") {
        return parse_psk_hex(&value);
    }
    Ok(psk)
}

fn unix_us_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

fn hevc_nal_type_name(nal_type: u8) -> &'static str {
    match nal_type {
        0..=31 => "VCL",
        32 => "VPS",
        33 => "SPS",
        34 => "PPS",
        35 => "AUD",
        36 => "EOS",
        37 => "EOB",
        38 => "FD",
        39 => "PREFIX_SEI",
        40 => "SUFFIX_SEI",
        41..=47 => "RESERVED_NVCL",
        48..=63 => "UNSPECIFIED",
        _ => "UNKNOWN",
    }
}

fn sao_as_bytes(sao: &SaoParameters) -> &[u8] {
    // SAFETY: `SaoParameters` is `#[repr(C)]`, and we only create a read-only
    // byte view over its initialized stack value for immediate file writing.
    unsafe {
        std::slice::from_raw_parts(
            (sao as *const SaoParameters).cast::<u8>(),
            std::mem::size_of::<SaoParameters>(),
        )
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Send {
            dest,
            psk,
            ticket_in,
            ticket_out,
            frames,
            fps,
            payload_bytes,
            keyframe_every_30,
            sao,
        } => {
            let psk = resolve_psk(psk)?;
            let mut sender = SankakuSender::new_with_psk(&dest, psk).await?;
            if let Some(path) = ticket_in {
                let blob =
                    fs::read(&path).with_context(|| format!("Failed to read ticket {path}"))?;
                sender.import_resumption_ticket(&blob)?;
            }

            let stream_id = sender.open_stream()?;
            let interval_ms = (1000u64 / fps.max(1) as u64).max(1);
            let payload_seed = if sao { 0x5Au8 } else { 0x3Cu8 };

            let mut total_bytes = 0u64;
            for index in 0..frames {
                let mut payload = vec![payload_seed; payload_bytes.max(1)];
                if !payload.is_empty() {
                    payload[0] = (index & 0xFF) as u8;
                }

                let keyframe = keyframe_every_30 && index % 30 == 0;
                let frame = VideoFrame {
                    timestamp_us: unix_us_now(),
                    keyframe,
                    kind: if sao {
                        VideoPayloadKind::SaoParameters
                    } else {
                        VideoPayloadKind::NalUnit
                    },
                    payload,
                };
                let frame_index = sender.send_frame(stream_id, frame).await?;
                println!("sent stream={stream_id:x} frame={frame_index}");
                total_bytes = total_bytes.saturating_add(payload_bytes as u64);
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
            }

            sender
                .send_stream_fin(stream_id, total_bytes, frames as u64)
                .await?;

            if let Some(path) = ticket_out {
                if let Some(blob) = sender.export_resumption_ticket()? {
                    fs::write(&path, blob)
                        .with_context(|| format!("Failed to write ticket {path}"))?;
                }
            }
        }
        Commands::Recv {
            bind,
            psk,
            ticket_key,
            max_frames,
        } => {
            let psk = resolve_psk(psk)?;
            let ticket_key = resolve_ticket_key(ticket_key, psk)?;
            let receiver = SankakuReceiver::new_with_psk_and_ticket_key(&bind, psk, ticket_key)
                .await
                .with_context(|| format!("Failed to bind receiver on {bind}"))?;
            println!("listening on {}", receiver.local_addr()?);

            let mut frames_seen = 0u64;
            let mut inbound = receiver.spawn_frame_channel();
            while let Some(frame) = inbound.recv().await {
                frames_seen = frames_seen.saturating_add(1);
                println!(
                    "recv session={} stream={:x} frame={} kind={:?} bytes={} keyframe={}",
                    frame.session_id,
                    frame.stream_id,
                    frame.frame_index,
                    frame.kind,
                    frame.payload.len(),
                    frame.keyframe
                );
                if max_frames > 0 && frames_seen >= max_frames {
                    break;
                }
            }
        }
        Commands::InspectHevc { file } => {
            let bytes =
                fs::read(&file).with_context(|| format!("Failed to read HEVC file {file}"))?;

            let mut nal_type_counts: HashMap<u8, usize> = HashMap::new();
            let mut nal_count = 0usize;

            for (index, nal_unit) in split_annex_b(&bytes).enumerate() {
                nal_count = nal_count.saturating_add(1);

                if let Some(unit_type) = nal_unit_type(nal_unit) {
                    *nal_type_counts.entry(unit_type).or_insert(0) += 1;

                    if index < 50 {
                        println!(
                            "[Index {index}] NAL Unit Type: {unit_type} ({}) - Length: {} bytes",
                            hevc_nal_type_name(unit_type),
                            nal_unit.len()
                        );
                    }

                    if unit_type <= 31 {
                        if let Some(sao) = extract_sao_parameters(nal_unit) {
                            println!(
                                "[Index {index}] VCL Unit - SAO Extracted: CTU({},{}), Type: {}, Band: {}, Offsets: [{}, {}, {}, {}]",
                                sao.ctu_x,
                                sao.ctu_y,
                                sao.sao_type_idx,
                                sao.band_position,
                                sao.offset[0],
                                sao.offset[1],
                                sao.offset[2],
                                sao.offset[3],
                            );
                        }
                    }
                } else if index < 50 {
                    println!(
                        "[Index {index}] NAL Unit Type: Unknown (MALFORMED) - Length: {} bytes",
                        nal_unit.len()
                    );
                }
            }

            let mut sorted_counts: Vec<(u8, usize)> = nal_type_counts
                .iter()
                .map(|(ty, count)| (*ty, *count))
                .collect();
            sorted_counts.sort_by_key(|(ty, _)| *ty);

            let vcl_count: usize = sorted_counts
                .iter()
                .filter(|(ty, _)| *ty <= 31)
                .map(|(_, count)| *count)
                .sum();

            println!("\nHEVC NAL Summary");
            println!("Total NAL units parsed: {nal_count}");
            for (unit_type, count) in sorted_counts {
                println!(
                    "Type {:>2} ({:<13}) -> {}",
                    unit_type,
                    hevc_nal_type_name(unit_type),
                    count
                );
            }
            println!("Total VCL units (0-31): {vcl_count}");
        }
        Commands::DumpSaoDataset { input, output } => {
            let bytes =
                fs::read(&input).with_context(|| format!("Failed to read HEVC input {input}"))?;
            let mut output_file = fs::File::create(&output)
                .with_context(|| format!("Failed to create output dataset {output}"))?;

            let mut structs_written = 0usize;
            for nal_unit in split_annex_b(&bytes) {
                if let Some(sao) = extract_sao_parameters(nal_unit) {
                    output_file
                        .write_all(sao_as_bytes(&sao))
                        .with_context(|| format!("Failed to write dataset struct to {output}"))?;
                    structs_written = structs_written.saturating_add(1);
                }
            }

            output_file
                .flush()
                .with_context(|| format!("Failed to flush dataset output {output}"))?;

            println!(
                "Successfully wrote {structs_written} SaoParameters structs to dataset file: {output}"
            );
        }
    }
    Ok(())
}
