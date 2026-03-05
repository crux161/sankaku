import React, { useRef, useEffect, useState, useCallback } from "react";
import {
  Typography,
  Tag,
  Tooltip,
  Avatar,
  Space,
  Button,
} from "@douyinfe/semi-ui";
import {
  IconCamera,
  IconDesktop,
  IconClose,
  IconMicrophone,
  IconStop,
  IconComment,
  IconSetting,
} from "@douyinfe/semi-icons";
import { useI18n } from "../i18n/index";
import {
  CallState,
  type ActiveCallInfo,
  type CallNetworkMetrics,
} from "../types/call";

const { Title, Text } = Typography;

interface Props {
  callState: CallState;
  activeCall: ActiveCallInfo;
  localStream: MediaStream | null;
  remoteStream: MediaStream | null;
  cameraOn: boolean;
  micOn: boolean;
  metrics: CallNetworkMetrics | null;
  chatOpen: boolean;
  unreadChat: number;
  voicemailRecording: boolean;
  voicemailEnabled: boolean;
  onToggleCamera: () => void;
  onToggleMic: () => void;
  onEndCall: () => void;
  onRecordVoicemail: () => Promise<boolean>;
  onStopVoicemail: () => Promise<void>;
  onToggleChat: () => void;
}

function formatDuration(seconds: number): string {
  const m = Math.floor(seconds / 60)
    .toString()
    .padStart(2, "0");
  const s = (seconds % 60).toString().padStart(2, "0");
  return `${m}:${s}`;
}

const TallyLight: React.FC<{
  label: string;
  live: boolean;
  kind: "camera" | "mic";
}> = ({ label, live, kind }) => (
  <div className={`xdx-tally ${live ? "live" : "off"} xdx-tally-${kind}`}>
    <span className="xdx-tally-dot" />
    <Text
      size="small"
      style={{ color: live ? "#fff" : "rgba(255,255,255,0.35)" }}
    >
      {label}
    </Text>
  </div>
);

const CallView: React.FC<Props> = ({
  callState,
  activeCall,
  localStream,
  remoteStream,
  cameraOn,
  micOn,
  metrics,
  chatOpen,
  unreadChat,
  voicemailRecording,
  voicemailEnabled,
  onToggleCamera,
  onToggleMic,
  onEndCall,
  onRecordVoicemail,
  onStopVoicemail,
  onToggleChat,
}) => {
  const { t } = useI18n();
  const remoteVideoRef = useRef<HTMLVideoElement>(null);
  const remoteAudioRef = useRef<HTMLAudioElement>(null);
  const localVideoRef = useRef<HTMLVideoElement>(null);
  const [callDuration, setCallDuration] = useState("00:00");
  const [recordingDuration, setRecordingDuration] = useState(0);
  const [showMetrics, setShowMetrics] = useState(false);

  useEffect(() => {
    if (localVideoRef.current && localStream) {
      localVideoRef.current.srcObject = localStream;
    }
  }, [localStream, callState]);

  useEffect(() => {
    if (remoteVideoRef.current && remoteStream) {
      remoteVideoRef.current.srcObject = remoteStream;
    }
  }, [remoteStream]);

  // Always attach remote stream to a hidden audio element for audio playback
  useEffect(() => {
    if (remoteAudioRef.current && remoteStream) {
      remoteAudioRef.current.srcObject = remoteStream;
    }
  }, [remoteStream]);

  useEffect(() => {
    if (
      callState !== CallState.InCallVideo &&
      callState !== CallState.InCallAudio
    )
      return;
    const start = activeCall.startTime;
    const tick = setInterval(() => {
      setCallDuration(
        formatDuration(Math.floor((Date.now() - start) / 1000)),
      );
    }, 1000);
    return () => clearInterval(tick);
  }, [callState, activeCall.startTime]);

  useEffect(() => {
    if (!voicemailRecording) return;
    const tick = setInterval(() => setRecordingDuration((d) => d + 1), 1000);
    return () => clearInterval(tick);
  }, [voicemailRecording]);

  const handleStartRecording = useCallback(() => {
    onRecordVoicemail()
      .then((started) => {
        if (started) {
          setRecordingDuration(0);
        }
      })
      .catch(() => {});
  }, [onRecordVoicemail]);

  const handleStopRecording = useCallback(() => {
    onStopVoicemail().catch(() => {});
  }, [onStopVoicemail]);

  // ===== Voicemail UI =====
  if (callState === CallState.Voicemail) {
    return (
      <div className="xdx-voicemail">
        <div className="xdx-voicemail-content">
          <Avatar
            size="extra-large"
            className="xdx-voicemail-avatar"
            style={{
              background:
                "linear-gradient(135deg, #667eea 0%, #764ba2 100%)",
            }}
          >
            {activeCall.peerName.charAt(0)}
          </Avatar>
          <Title heading={4} style={{ color: "#fff", marginTop: 24 }}>
            {activeCall.peerName} {t("voicemail.unanswered")}
          </Title>
          <Text style={{ color: "rgba(255,255,255,0.45)", marginTop: 4 }}>
            {t("voicemail.leaveMessage")}
          </Text>
          {voicemailRecording ? (
            <div className="xdx-recording-active">
              <div className="xdx-recording-indicator">
                <span className="xdx-rec-dot" />
                <Text
                  style={{
                    color: "#ff4757",
                    fontWeight: 700,
                    letterSpacing: 1,
                  }}
                >
                  {t("voicemail.rec")}
                </Text>
              </div>
              <Title
                heading={2}
                style={{
                  color: "#fff",
                  fontVariantNumeric: "tabular-nums",
                  margin: "16px 0",
                }}
              >
                {formatDuration(recordingDuration)}
              </Title>
              <div className="xdx-waveform">
                {Array.from({ length: 32 }).map((_, i) => (
                  <div
                    key={i}
                    className="xdx-waveform-bar"
                    style={{ animationDelay: `${i * 0.06}s` }}
                  />
                ))}
              </div>
              <div className="xdx-voicemail-preview">
                <video
                  ref={localVideoRef}
                  className="xdx-video-element"
                  autoPlay
                  playsInline
                  muted
                />
                {!localStream && (
                  <div className="xdx-video-placeholder-small">
                    <IconCamera
                      size="large"
                      style={{ color: "rgba(255,255,255,0.2)" }}
                    />
                  </div>
                )}
              </div>
              <button
                className="xdx-btn-stop-record"
                onClick={handleStopRecording}
              >
                <IconStop style={{ marginRight: 6 }} />
                {t("voicemail.stopRecording")}
              </button>
            </div>
          ) : (
            <div className="xdx-voicemail-start">
              <Space vertical align="center" spacing={16} style={{ marginTop: 32 }}>
                <button
                  className="xdx-btn-record"
                  onClick={handleStartRecording}
                  disabled={!voicemailEnabled}
                >
                  <span className="xdx-record-circle" />
                </button>
                <Text
                  size="small"
                  style={{ color: "rgba(255,255,255,0.5)" }}
                >
                  {voicemailEnabled
                    ? t("voicemail.startRecording")
                    : t("voicemail.onlyFriends")}
                </Text>
                <button className="xdx-link-btn" onClick={onEndCall}>
                  {t("voicemail.back")}
                </button>
              </Space>
            </div>
          )}
        </div>
      </div>
    );
  }

  // ===== In-Call UI =====
  const isAudioOnlyCall = callState === CallState.InCallAudio;
  const hasRemoteVideo = !!remoteStream;
  const bitrateMbps = ((metrics?.bitrateBps ?? 0) / 1_000_000).toFixed(2);
  const lossPercent = (metrics?.packetLossPercent ?? 0).toFixed(2);

  return (
    <div className="xdx-call-view">
      {/* Hidden audio element — ensures remote audio always plays (especially audio-only calls) */}
      <audio ref={remoteAudioRef} autoPlay playsInline style={{ display: "none" }} />

      {/* Tally bar */}
      <div className="xdx-tally-bar">
        <TallyLight
          label={cameraOn ? t("call.camLive") : t("call.camOff")}
          live={cameraOn}
          kind="camera"
        />
        <div className="xdx-call-info-bar">
          <Tag
            color={callState === CallState.InCallVideo ? "green" : "blue"}
            size="small"
          >
            {callState === CallState.InCallVideo
              ? t("call.videoCall")
              : t("call.audioCall")}
          </Tag>
          <Text
            style={{
              color: "rgba(255,255,255,0.6)",
              fontVariantNumeric: "tabular-nums",
              margin: "0 8px",
              fontSize: 13,
            }}
          >
            {callDuration}
          </Text>
          <Text size="small" style={{ color: "rgba(255,255,255,0.3)" }}>
            {activeCall.peerName}
          </Text>
          <Tooltip
            content={showMetrics ? "Hide metrics" : "Show metrics"}
            position="top"
          >
            <Button
              aria-label={showMetrics ? "Hide metrics" : "Show metrics"}
              theme="borderless"
              type="tertiary"
              icon={<IconSetting size="small" />}
              size="small"
              className={`xdx-metrics-toggle ${showMetrics ? "active" : ""}`}
              onClick={() => setShowMetrics((prev) => !prev)}
            />
          </Tooltip>
        </div>
        <TallyLight
          label={micOn ? t("call.micLive") : t("call.micOff")}
          live={micOn}
          kind="mic"
        />
      </div>

      {/* Video grid */}
      <div className="xdx-video-grid">
        <div className="xdx-remote-video">
          {isAudioOnlyCall ? (
            <div className="xdx-audio-only-display">
              <div className="xdx-audio-center">
                <Avatar
                  size="extra-large"
                  style={{
                    background:
                      "linear-gradient(135deg, #667eea 0%, #764ba2 100%)",
                    width: 96,
                    height: 96,
                    fontSize: 36,
                    boxShadow: "0 14px 34px rgba(0, 0, 0, 0.42)",
                  }}
                >
                  {activeCall.peerName.charAt(0)}
                </Avatar>
                <Title heading={3} style={{ color: "#fff", marginTop: 18 }}>
                  {activeCall.peerName}
                </Title>
                <Text className="xdx-audio-call-duration">{callDuration}</Text>
                <Text style={{ color: "rgba(255,255,255,0.55)" }}>
                  {t("call.audioInProgress")}
                </Text>
              </div>
            </div>
          ) : (
            <>
              <video
                ref={remoteVideoRef}
                className={`xdx-video-element ${hasRemoteVideo ? "active" : ""}`}
                autoPlay
                playsInline
              />
              {!hasRemoteVideo && (
                <div className="xdx-video-placeholder">
                  <Avatar
                    size="extra-large"
                    style={{
                      background:
                        "linear-gradient(135deg, #667eea 0%, #764ba2 100%)",
                      width: 80,
                      height: 80,
                      fontSize: 32,
                    }}
                  >
                    {activeCall.peerName.charAt(0)}
                  </Avatar>
                  <Text
                    style={{
                      color: "rgba(255,255,255,0.35)",
                      marginTop: 12,
                      fontSize: 13,
                    }}
                  >
                    {t("call.waitingRemote")}
                  </Text>
                </div>
              )}
            </>
          )}
          {metrics && showMetrics && (
            <div className="xdx-metrics-overlay">
              <div className="xdx-metrics-title">Sankaku/RT Telemetry</div>
              <div className="xdx-metrics-row">
                RTT: {metrics.rttMs} ms
              </div>
              <div className="xdx-metrics-row">
                Bitrate: {bitrateMbps} Mbps
              </div>
              <div className="xdx-metrics-row">
                Loss: {lossPercent}%
              </div>
              <div className="xdx-metrics-row">
                Dropped: {metrics.packetsDropped}
              </div>
              <div className="xdx-metrics-row">
                FPS: {metrics.fps.toFixed(1)}
              </div>
            </div>
          )}
          {!isAudioOnlyCall && (
            <div className="xdx-remote-name-overlay">
              <Text
                size="small"
                style={{
                  color: "#fff",
                  textShadow: "0 1px 4px rgba(0,0,0,0.6)",
                }}
              >
                {activeCall.peerName}
              </Text>
            </div>
          )}
        </div>

        {/* Local PiP with real webcam feed */}
        {callState === CallState.InCallVideo && (
          <div
            className={`xdx-local-video ${!cameraOn ? "camera-off" : ""}`}
          >
            <video
              ref={localVideoRef}
              className="xdx-video-element xdx-local"
              autoPlay
              playsInline
              muted
            />
            {!cameraOn && (
              <div className="xdx-camera-off-overlay">
                <IconCamera
                  size="extra-large"
                  style={{ color: "rgba(255,255,255,0.3)" }}
                />
              </div>
            )}
            <div className="xdx-local-label">
              <Text size="small" style={{ color: "#fff" }}>
                {t("call.local")}
              </Text>
            </div>
          </div>
        )}
      </div>

      {/* Control bar */}
      {isAudioOnlyCall ? (
        <div className="xdx-control-bar xdx-control-bar-audio">
          <div className="xdx-control-bar-inner xdx-control-bar-inner-audio">
            <Tooltip
              content={micOn ? t("call.mute") : t("call.unmute")}
              position="top"
            >
              <button
                className={`xdx-ctrl-btn xdx-ctrl-btn-audio ${!micOn ? "toggled-off" : ""}`}
                onClick={onToggleMic}
              >
                <IconMicrophone size="extra-large" />
                {!micOn && <span className="xdx-slash-overlay" />}
              </button>
            </Tooltip>
            <Tooltip content={t("call.endCall")} position="top">
              <button
                className="xdx-ctrl-btn xdx-btn-end xdx-ctrl-btn-audio xdx-ctrl-btn-audio-end"
                onClick={onEndCall}
              >
                <IconClose size="extra-large" />
              </button>
            </Tooltip>
          </div>
        </div>
      ) : (
        <div className="xdx-control-bar">
          <div className="xdx-control-bar-inner">
            <Tooltip
              content={micOn ? t("call.mute") : t("call.unmute")}
              position="top"
            >
              <button
                className={`xdx-ctrl-btn ${!micOn ? "toggled-off" : ""}`}
                onClick={onToggleMic}
              >
                <IconMicrophone size="extra-large" />
                {!micOn && <span className="xdx-slash-overlay" />}
              </button>
            </Tooltip>
            <Tooltip
              content={cameraOn ? t("call.cameraOff") : t("call.cameraOn")}
              position="top"
            >
              <button
                className={`xdx-ctrl-btn ${!cameraOn ? "toggled-off" : ""}`}
                onClick={onToggleCamera}
              >
                <IconCamera size="extra-large" />
                {!cameraOn && <span className="xdx-slash-overlay" />}
              </button>
            </Tooltip>
            <Tooltip content={t("chat.title")} position="top">
              <button
                className={`xdx-ctrl-btn ${chatOpen ? "active" : ""}`}
                onClick={onToggleChat}
              >
                <IconComment size="extra-large" />
                {unreadChat > 0 && (
                  <span className="xdx-unread-badge">{unreadChat}</span>
                )}
              </button>
            </Tooltip>
            <Tooltip content={t("call.shareScreen")} position="top">
              <button className="xdx-ctrl-btn">
                <IconDesktop size="extra-large" />
              </button>
            </Tooltip>
            <Tooltip content={t("call.endCall")} position="top">
              <button className="xdx-ctrl-btn xdx-btn-end" onClick={onEndCall}>
                <IconClose size="extra-large" />
              </button>
            </Tooltip>
          </div>
        </div>
      )}
    </div>
  );
};

export default CallView;
