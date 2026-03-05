import React, { useState, useEffect, useCallback, useRef } from "react";
import {
  Layout,
  Avatar,
  Modal,
  Typography,
  Tag,
  Tooltip,
  Toast,
  Spin,
} from "@douyinfe/semi-ui";
import {
  IconCamera,
  IconSearch,
  IconSetting,
  IconUser,
  IconComment,
  IconClose,
  IconMicrophone,
  IconPhone,
  IconDownload,
  IconUserAdd,
} from "@douyinfe/semi-icons";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { I18nProvider, useI18n } from "./i18n/index";
import SankakuBridge from "./services/SankakuBridge";
import { WebRTCService } from "./services/WebRTCService";
import { useMediaDevices } from "./hooks/useMediaDevices";
import CallView from "./components/CallView";
import ChatPanel from "./components/ChatPanel";
import SettingsPanel, { AVATAR_MAP } from "./components/SettingsPanel";
import DialPad from "./components/DialPad";
import EventCard from "./components/EventCard";
import {
  CallState,
  type ActiveCallInfo,
  type ChatMessage,
  type CallNetworkMetrics,
  type DiscoveredPeer,
  type DownloadedFileEntry,
  type FileTransferProgress,
  type Friend,
  type SignalMessage,
  type StoredMessage,
  type UserProfile,
  type LocaleCode,
} from "./types/call";

import foxOk from "../reference/images/kyu-kun/fox-maru-green-OK.jpeg";
import foxSleeping from "../reference/images/kyu-kun/fox-sleeping.jpeg";
import foxCelebrate from "../reference/images/kyu-kun/fox-celebrate-recovery.jpeg";
import foxHanabi from "../reference/images/kyu-kun/fox-hanabi-summer.jpeg";
import foxHanami from "../reference/images/kyu-kun/fox-hanami.jpeg";
import foxLunar from "../reference/images/kyu-kun/fox-lunar-new-year.jpeg";
import foxBandana from "../reference/images/kyu-kun/fox-bandana.jpeg";
import kenRed from "../reference/images/ken-chan/koken-CNY-red.jpeg";
import pentaro from "../reference/images/pentaro-san/OIG2.AB3fp4AoIltcenw1pKtq.jpeg";
import izakaya from "../reference/images/izakaya/OIG1.926SucZQi9p5mEil9SD4.jpeg";

import "./App.css";

const { Sider, Content } = Layout;
const { Title, Text } = Typography;

type SidebarTab = "peers" | "voicemail" | "downloads";

const IDLE_ART = [
  { src: foxOk, alt: "Kyu-kun" },
  { src: foxHanabi, alt: "Summer" },
  { src: foxHanami, alt: "Hanami" },
  { src: kenRed, alt: "Ken-chan" },
  { src: pentaro, alt: "Pentaro" },
  { src: izakaya, alt: "Izakaya" },
];

let chatIdCounter = 0;
const MOBILE_BREAKPOINT = 900;

function nextChatId(): string {
  return `msg-${Date.now()}-${++chatIdCounter}`;
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatShortTime(epochSecs: number): string {
  if (!epochSecs) return "--";
  return new Date(epochSecs * 1000).toLocaleString();
}

function base64ToBytes(dataB64: string): Uint8Array {
  const raw = atob(dataB64);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i += 1) {
    bytes[i] = raw.charCodeAt(i);
  }
  return bytes;
}

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const input = new Uint8Array(bytes.byteLength);
  input.set(bytes);
  const digest = await crypto.subtle.digest(
    "SHA-256",
    input.buffer as ArrayBuffer,
  );
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function bytesToBase64(bytes: Uint8Array): string {
  let raw = "";
  const BATCH = 8192;
  for (let i = 0; i < bytes.length; i += BATCH) {
    const chunk = bytes.subarray(i, i + BATCH);
    raw += String.fromCharCode(...chunk);
  }
  return btoa(raw);
}

function generateFriendKey(): string {
  const rand = new Uint8Array(16);
  crypto.getRandomValues(rand);
  return Array.from(rand)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

// ---------------------------------------------------------------------------
// Inner app (wrapped by I18nProvider)
// ---------------------------------------------------------------------------

const AppInner: React.FC = () => {
  const { t } = useI18n();
  const bridge = useRef(SankakuBridge.getInstance());
  const media = useMediaDevices();
  const rtcRef = useRef<WebRTCService | null>(null);

  const [callState, setCallState] = useState<CallState>(CallState.Idle);
  const [activeCall, setActiveCall] = useState<ActiveCallInfo | null>(null);
  const [remoteStream, setRemoteStream] = useState<MediaStream | null>(null);
  const [pendingOffer, setPendingOffer] = useState<
    (SignalMessage & { type: "offer" }) | null
  >(null);

  const [profile, setProfile] = useState<UserProfile | null>(null);
  const [peers, setPeers] = useState<DiscoveredPeer[]>([]);
  const [coreReady, setCoreReady] = useState(false);
  const [sidebarTab, setSidebarTab] = useState<SidebarTab>("peers");
  const [dialPadOpen, setDialPadOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [isMobile, setIsMobile] = useState<boolean>(
    () => window.innerWidth <= MOBILE_BREAKPOINT,
  );
  const [uiScale, setUiScale] = useState<number>(() => {
    const stored = Number(window.localStorage.getItem("xdx-ui-scale"));
    if (Number.isFinite(stored) && stored >= 0.9 && stored <= 1.2) {
      return stored;
    }
    return window.innerWidth <= MOBILE_BREAKPOINT ? 1.08 : 1;
  });

  const [chatMessages, setChatMessages] = useState<ChatMessage[]>([]);
  const [chatOpen, setChatOpen] = useState(false);
  const [unreadChat, setUnreadChat] = useState(0);

  const [connectedPeer, setConnectedPeer] = useState<{
    name: string;
    code: string;
  } | null>(null);
  const [remoteTyping, setRemoteTyping] = useState<string | null>(null);
  const typingTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const typingThrottleRef = useRef<number>(0);

  const [friends, setFriends] = useState<Friend[]>([]);
  const [pendingFriendRequest, setPendingFriendRequest] = useState<{
    callingCode: string;
    displayName: string;
    avatarId?: string;
    publicKey?: string;
  } | null>(null);
  const [customAvatarUrl, setCustomAvatarUrl] = useState<string | null>(null);
  const [fileTransfers, setFileTransfers] = useState<
    Map<string, FileTransferProgress>
  >(new Map());
  const fileTransfersRef = useRef<Map<string, FileTransferProgress>>(
    new Map(),
  );
  const fileChunksRef = useRef<Map<string, string[]>>(new Map());
  const [downloads, setDownloads] = useState<DownloadedFileEntry[]>([]);
  const [callMetrics, setCallMetrics] = useState<CallNetworkMetrics | null>(
    null,
  );
  const metricsTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const metricsLastBytesRef = useRef<{ bytes: number; ts: number } | null>(
    null,
  );
  const connectedPeerRef = useRef<{ name: string; code: string } | null>(null);
  const voicemailRecorderRef = useRef<MediaRecorder | null>(null);
  const voicemailChunksRef = useRef<BlobPart[]>([]);
  const [voicemailRecording, setVoicemailRecording] = useState(false);

  useEffect(() => {
    fileTransfersRef.current = fileTransfers;
  }, [fileTransfers]);

  useEffect(() => {
    connectedPeerRef.current = connectedPeer;
  }, [connectedPeer]);

  const getAvatarSrc = useCallback(
    (avatarId?: string) => {
      if (!avatarId) return undefined;
      if (avatarId === "custom") {
        return customAvatarUrl ?? undefined;
      }
      return AVATAR_MAP[avatarId] ?? undefined;
    },
    [customAvatarUrl],
  );

  const getFriendByCode = useCallback(
    (code?: string | null) => {
      if (!code) return undefined;
      return friends.find((f) => f.callingCode === code && f.approved);
    },
    [friends],
  );

  useEffect(() => {
    const onResize = () => {
      setIsMobile(window.innerWidth <= MOBILE_BREAKPOINT);
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    const clamped = Math.min(1.2, Math.max(0.9, uiScale));
    document.documentElement.style.setProperty(
      "--xdx-ui-scale",
      clamped.toFixed(2),
    );
    window.localStorage.setItem("xdx-ui-scale", String(clamped));
  }, [uiScale]);

  const handleTitlebarMouseDown = useCallback(
    (e: React.MouseEvent<HTMLElement>) => {
      if (isMobile || e.button !== 0) return;
      const target = e.target as HTMLElement;
      if (
        target.closest(
          "button,input,textarea,select,a,[data-no-drag='true']",
        )
      ) {
        return;
      }
      getCurrentWindow()
        .startDragging()
        .catch(() => {});
    },
    [isMobile],
  );

  const refreshDownloads = useCallback(() => {
    bridge.current
      .listReceivedFiles()
      .then((entries) => setDownloads(entries))
      .catch((err) => {
        console.error("[XDX] list received files failed", err);
      });
  }, []);

  // Bootstrap
  useEffect(() => {
    const init = async () => {
      try {
        const prof = await bridge.current.getProfile();
        setProfile(prof);
        const customAvatar = await bridge.current.loadCustomAvatar();
        setCustomAvatarUrl(customAvatar);

        const friendsList = await bridge.current.getFriends();
        setFriends(friendsList);
        const received = await bridge.current.listReceivedFiles();
        setDownloads(received);

        const result = await bridge.current.initialize();
        if (result.success) {
          setCoreReady(true);
          Toast.success({ content: t("toast.engineReady"), duration: 2 });
          const discovered = await bridge.current.getDiscoveredPeers();
          setPeers(discovered);
        }
      } catch (err) {
        console.error("[XDX] init error", err);
        Toast.error({
          content: `${t("toast.initFailed")}: ${err}`,
          duration: 4,
        });
      }
    };

    const unsub2 = bridge.current.onPeerDiscovered((peer) => {
      setPeers((prev) => {
        const idx = prev.findIndex(
          (p) => p.callingCode === peer.callingCode,
        );
        if (idx >= 0) {
          const next = [...prev];
          next[idx] = peer;
          return next;
        }
        return [...prev, peer];
      });
    });

    const unsub3 = bridge.current.onPeerLost((code) => {
      setPeers((prev) => prev.filter((p) => p.callingCode !== code));
    });

    const unsub5 = bridge.current.onSignalingDisconnect(() => {
      setConnectedPeer(null);
      setRemoteTyping(null);
      if (
        rtcRef.current &&
        rtcRef.current.connectionState !== "closed"
      ) {
        Toast.warning({ content: t("toast.peerDisconnected") });
        cleanupCall();
      }
    });

    init();
    return () => {
      unsub2();
      unsub3();
      unsub5();
      bridge.current.destroy();
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const persistMessages = useCallback(
    (peerCode: string, msgs: ChatMessage[]) => {
      const stored: StoredMessage[] = msgs.map((m) => ({
        id: m.id,
        from: m.from,
        text: m.text,
        sticker: m.sticker,
        fileName: m.fileName,
        timestamp: m.timestamp,
      }));
      bridge.current.saveMessages(peerCode, stored).catch(() => {});
    },
    [],
  );

  useEffect(() => {
    const unsub = bridge.current.onSignalingMessage((msg: SignalMessage) => {
      switch (msg.type) {
        case "offer":
          setConnectedPeer({
            name: msg.displayName,
            code: msg.callingCode,
          });
          setPendingOffer(msg);
          setCallState(CallState.IncomingCall);
          break;
        case "answer":
          rtcRef.current?.handleAnswer(msg.sdp);
          break;
        case "ice":
          rtcRef.current?.addIceCandidate(msg.candidate);
          break;
        case "chat": {
          const chatMsg: ChatMessage = {
            id: msg.id,
            from: "remote",
            text: msg.text,
            sticker: msg.sticker,
            timestamp: msg.timestamp,
          };
          setChatMessages((prev) => {
            if (prev.some((m) => m.id === msg.id)) return prev;
            const next = [...prev, chatMsg];
            if (connectedPeer) persistMessages(connectedPeer.code, next);
            return next;
          });
          setChatOpen((open) => {
            if (!open) setUnreadChat((n) => n + 1);
            return open;
          });
          break;
        }
        case "typing": {
          setRemoteTyping(`${msg.displayName} ${t("chat.isTyping")}`);
          if (typingTimeoutRef.current)
            clearTimeout(typingTimeoutRef.current);
          typingTimeoutRef.current = setTimeout(
            () => setRemoteTyping(null),
            3000,
          );
          break;
        }
        case "hello": {
          const existingFriend = getFriendByCode(msg.callingCode);
          if (existingFriend) {
            if (
              (msg.avatarId && msg.avatarId !== existingFriend.avatarId) ||
              (msg.publicKey && msg.publicKey !== existingFriend.publicKey)
            ) {
              bridge.current
                .addFriend(
                  msg.callingCode,
                  msg.displayName,
                  msg.avatarId,
                  msg.publicKey,
                )
                .then((friend) =>
                  setFriends((prev) => [
                    ...prev.filter((f) => f.callingCode !== friend.callingCode),
                    friend,
                  ]),
                )
                .catch(() => {});
            }
            setConnectedPeer({
              name: msg.displayName,
              code: msg.callingCode,
            });
            bridge.current
              .loadMessages(msg.callingCode)
              .then((stored) => {
                const loaded: ChatMessage[] = stored.map((s) => ({
                  id: s.id,
                  from: s.from as "local" | "remote",
                  text: s.text,
                  sticker: s.sticker,
                  fileName: s.fileName,
                  timestamp: s.timestamp,
                }));
                setChatMessages(loaded);
              })
              .catch(() => {});
            setChatOpen(true);
          } else {
            setPendingFriendRequest({
              callingCode: msg.callingCode,
              displayName: msg.displayName,
              avatarId: msg.avatarId,
              publicKey: msg.publicKey,
            });
          }
          break;
        }
        case "friend-accept":
          setConnectedPeer({
            name: msg.displayName,
            code: msg.callingCode,
          });
          bridge.current
            .addFriend(
              msg.callingCode,
              msg.displayName,
              msg.avatarId,
              msg.publicKey,
            )
            .then(async (f) => {
              setFriends((prev) => [
                ...prev.filter((x) => x.callingCode !== f.callingCode),
                f,
              ]);
              const prof = profile ?? (await bridge.current.getProfile());
              bridge.current
                .sendSignal({
                  type: "hello",
                  callingCode: prof.callingCode,
                  displayName: prof.displayName,
                  avatarId: prof.avatarId,
                  publicKey: f.publicKey ?? undefined,
                })
                .catch(() => {});
            })
            .catch(() => {});
          Toast.success({ content: t("friends.approved") });
          setChatOpen(true);
          break;
        case "friend-request":
          setPendingFriendRequest({
            callingCode: msg.callingCode,
            displayName: msg.displayName,
            avatarId: msg.avatarId,
          });
          break;
        case "file-offer": {
          const kind = msg.kind ?? "file";
          const senderCode = msg.fromCode ?? connectedPeerRef.current?.code;
          if (kind === "voicemail") {
            const friend = getFriendByCode(senderCode);
            const keyOk =
              !!friend?.publicKey &&
              !!msg.voicemailAuth &&
              friend.publicKey === msg.voicemailAuth;
            if (!keyOk) {
              bridge.current
                .sendSignal({ type: "file-reject", fileId: msg.fileId })
                .catch(() => {});
              Toast.warning({ content: t("voicemail.unauthorized") });
              break;
            }
          }

          const transfer: FileTransferProgress = {
            fileId: msg.fileId,
            fileName: msg.fileName,
            byteSize: msg.fileSize,
            totalSize: msg.totalChunks ?? 1,
            received: 0,
            kind,
            sha256: msg.sha256,
            verified: false,
            direction: "receive",
            status: kind === "voicemail" ? "transferring" : "offering",
          };
          setFileTransfers((prev) => new Map(prev).set(msg.fileId, transfer));
          const offerMsg: ChatMessage = {
            id: `file-offer-${msg.fileId}`,
            from: "remote",
            text:
              kind === "voicemail"
                ? `🎙️ ${msg.fileName} (${formatFileSize(msg.fileSize)})`
                : `📎 ${msg.fileName} (${formatFileSize(msg.fileSize)})`,
            fileName: msg.fileName,
            timestamp: Date.now(),
          };
          setChatMessages((prev) => [...prev, offerMsg]);
          if (kind === "voicemail") {
            bridge.current
              .sendSignal({ type: "file-accept", fileId: msg.fileId })
              .catch(() => {});
          }
          break;
        }
        case "file-accept": {
          setFileTransfers((prev) => {
            const next = new Map(prev);
            const t = next.get(msg.fileId);
            if (t) next.set(msg.fileId, { ...t, status: "transferring" });
            return next;
          });
          break;
        }
        case "file-reject": {
          setFileTransfers((prev) => {
            const next = new Map(prev);
            const t = next.get(msg.fileId);
            if (t) next.set(msg.fileId, { ...t, status: "rejected" });
            return next;
          });
          break;
        }
        case "file-chunk": {
          const chunks = fileChunksRef.current.get(msg.fileId) ?? [];
          chunks[msg.offset] = msg.data;
          fileChunksRef.current.set(msg.fileId, chunks);
          setFileTransfers((prev) => {
            const next = new Map(prev);
            const t = next.get(msg.fileId);
            if (t) {
              const received = chunks.filter(Boolean).length;
              next.set(msg.fileId, {
                ...t,
                totalSize: Math.max(t.totalSize, msg.total || 1),
                received,
                status: "transferring",
              });
            }
            return next;
          });
          break;
        }
        case "file-complete": {
          const chunks = fileChunksRef.current.get(msg.fileId) ?? [];
          const fullB64 = chunks.join("");
          const transfer = fileTransfersRef.current.get(msg.fileId);
          if (transfer) {
            const verifyAndSave = async () => {
              const bytes = base64ToBytes(fullB64);
              const actualHash = await sha256Hex(bytes);
              const expectedHash = transfer.sha256 ?? msg.sha256;
              if (
                expectedHash &&
                actualHash.toLowerCase() !== expectedHash.toLowerCase()
              ) {
                setFileTransfers((prev) => {
                  const next = new Map(prev);
                  next.set(msg.fileId, {
                    ...transfer,
                    status: "rejected",
                    verified: false,
                    error: "sha256-mismatch",
                  });
                  return next;
                });
                Toast.error({ content: t("chat.fileHashMismatch") });
                return;
              }

              const saved = await bridge.current.saveReceivedFile(
                transfer.fileName,
                fullB64,
                expectedHash,
              );
              const receivedLabel =
                transfer.kind === "voicemail"
                  ? t("voicemail.received")
                  : t("chat.fileReceived");
              Toast.success({
                content: `${receivedLabel}: ${saved.fileName}`,
              });
              setFileTransfers((prev) => {
                const next = new Map(prev);
                next.set(msg.fileId, {
                  ...transfer,
                  fileName: saved.fileName,
                  status: "complete",
                  verified: true,
                  sha256: saved.sha256,
                });
                return next;
              });
              refreshDownloads();
              if (transfer.kind === "voicemail") {
                setSidebarTab("voicemail");
              }
            };

            verifyAndSave().catch((err) => {
              console.error("[XDX] save file failed", err);
              Toast.error({ content: t("chat.fileSaveFailed") });
              setFileTransfers((prev) => {
                const next = new Map(prev);
                next.set(msg.fileId, {
                  ...transfer,
                  status: "rejected",
                  verified: false,
                  error: "save-failed",
                });
                return next;
              });
            });
          }
          fileChunksRef.current.delete(msg.fileId);
          break;
        }
        case "hangup":
          cleanupCall();
          break;
        case "decline":
          Toast.info({ content: t("incoming.decline") });
          rtcRef.current?.close();
          rtcRef.current = null;
          media.stopCamera();
          setRemoteStream(null);
          if (getFriendByCode(connectedPeerRef.current?.code)?.publicKey) {
            setCallState(CallState.Voicemail);
          } else {
            Toast.warning({ content: t("voicemail.onlyFriends") });
            setCallState(CallState.Idle);
            setActiveCall(null);
            setPendingOffer(null);
          }
          break;
      }
    });
    return unsub;
  }, [getFriendByCode, profile, refreshDownloads, t]); // eslint-disable-line react-hooks/exhaustive-deps

  const cleanupCall = useCallback(() => {
    const recorder = voicemailRecorderRef.current;
    if (recorder && recorder.state !== "inactive") {
      try {
        recorder.stop();
      } catch {
        /* ignore */
      }
    }
    voicemailRecorderRef.current = null;
    voicemailChunksRef.current = [];
    setVoicemailRecording(false);
    rtcRef.current?.close();
    rtcRef.current = null;
    media.stopCamera();
    setRemoteStream(null);
    setCallState(CallState.Idle);
    setActiveCall(null);
    setPendingOffer(null);
  }, [media]);

  const handleProfileChanged = useCallback((p: UserProfile) => {
    setProfile(p);
    bridge.current
      .loadCustomAvatar()
      .then((avatar) => setCustomAvatarUrl(avatar))
      .catch(() => {});
    window.dispatchEvent(new CustomEvent("xdx-profile-changed"));
  }, []);

  const initiateCall = useCallback(
    async (code: string, peerName: string, audioOnly: boolean) => {
      try {
        setCallState(CallState.Connecting);

        if (connectedPeer?.code !== code) {
          const result = await bridge.current.connectToPeer(code);
          if (!result.success) {
            Toast.warning({ content: result.message });
            setCallState(CallState.Idle);
            return;
          }
        }
        setConnectedPeer({ name: peerName, code });

        const stream = await media.startCamera(!audioOnly, true);
        if (!stream) {
          Toast.error({ content: t("toast.mediaFailed") });
          setCallState(CallState.Idle);
          return;
        }

        const rtc = new WebRTCService();
        rtc.init(stream, true);
        rtcRef.current = rtc;

        rtc.onRemoteStream = (rs) => setRemoteStream(rs);
        rtc.onIceCandidate = (candidate) => {
          bridge.current
            .sendSignal({ type: "ice", candidate })
            .catch(() => {});
        };

        const offerSdp = await rtc.createOffer();
        const prof = profile ?? (await bridge.current.getProfile());
        await bridge.current.sendSignal({
          type: "offer",
          callingCode: prof.callingCode,
          displayName: prof.displayName,
          audioOnly,
          sdp: offerSdp,
        });

        setActiveCall({
          peerId: code,
          peerName,
          audioOnly,
          startTime: Date.now(),
        });
        setCallState(
          audioOnly ? CallState.InCallAudio : CallState.InCallVideo,
        );
      } catch (err) {
        Toast.error({ content: `${t("toast.callFailed")}: ${err}` });
        cleanupCall();
      }
    },
    [media, profile, t, cleanupCall, connectedPeer],
  );

  const handleAcceptCall = useCallback(
    async (audioOnly: boolean) => {
      if (!pendingOffer) return;
      try {
        const stream = await media.startCamera(!audioOnly, true);
        if (!stream) {
          Toast.error({ content: t("toast.mediaFailed") });
          return;
        }

        const rtc = new WebRTCService();
        rtc.init(stream, false);
        rtcRef.current = rtc;

        rtc.onRemoteStream = (rs) => setRemoteStream(rs);
        rtc.onIceCandidate = (candidate) => {
          bridge.current
            .sendSignal({ type: "ice", candidate })
            .catch(() => {});
        };

        const answerSdp = await rtc.handleOffer(pendingOffer.sdp);
        await bridge.current.sendSignal({ type: "answer", sdp: answerSdp });
        await bridge.current.acceptCall();

        setActiveCall({
          peerId: pendingOffer.callingCode,
          peerName: pendingOffer.displayName,
          audioOnly,
          startTime: Date.now(),
        });
        setCallState(
          audioOnly ? CallState.InCallAudio : CallState.InCallVideo,
        );
        setPendingOffer(null);
      } catch (err) {
        Toast.error({ content: `${t("toast.acceptFailed")}: ${err}` });
      }
    },
    [pendingOffer, media, t],
  );

  const handleDeclineCall = useCallback(() => {
    bridge.current.sendSignal({ type: "decline" }).catch(() => {});
    setPendingOffer(null);
    setCallState(CallState.Idle);
  }, []);

  const handleEndCall = useCallback(async () => {
    const recorder = voicemailRecorderRef.current;
    if (recorder && recorder.state !== "inactive") {
      try {
        recorder.stop();
      } catch {
        /* ignore */
      }
    }
    voicemailRecorderRef.current = null;
    voicemailChunksRef.current = [];
    setVoicemailRecording(false);
    try {
      bridge.current.sendSignal({ type: "hangup" }).catch(() => {});
      await bridge.current.endCall();
    } catch {
      /* ignore */
    }
    cleanupCall();
    if (!connectedPeer) {
      setChatMessages([]);
      setChatOpen(false);
      setUnreadChat(0);
    }
  }, [cleanupCall, connectedPeer]);

  const handleRecordVoicemail = useCallback(async (): Promise<boolean> => {
    const peerCode = connectedPeerRef.current?.code;
    const friend = getFriendByCode(peerCode);
    if (!friend?.publicKey) {
      Toast.warning({ content: t("voicemail.onlyFriends") });
      return false;
    }

    try {
      const stream = media.localStream ?? (await media.startCamera(true, true));
      if (!stream) {
        Toast.error({ content: t("toast.mediaFailed") });
        return false;
      }
      if (typeof MediaRecorder === "undefined") {
        Toast.error({ content: t("voicemail.recordUnsupported") });
        return false;
      }

      const result = await bridge.current.recordVoicemail();
      if (!result.success) {
        Toast.error({ content: t("toast.recordFailed") });
        return false;
      }

      const preferredMime = [
        "video/mp4",
        "video/webm;codecs=vp8,opus",
        "video/webm",
      ].find((m) => MediaRecorder.isTypeSupported(m));

      const recorder = preferredMime
        ? new MediaRecorder(stream, { mimeType: preferredMime })
        : new MediaRecorder(stream);
      voicemailChunksRef.current = [];
      recorder.ondataavailable = (ev: BlobEvent) => {
        if (ev.data && ev.data.size > 0) {
          voicemailChunksRef.current.push(ev.data);
        }
      };
      recorder.start(300);
      voicemailRecorderRef.current = recorder;
      setVoicemailRecording(true);
      Toast.info({ content: t("toast.recordingStarted") });
      return true;
    } catch (err) {
      Toast.error({ content: `${t("toast.recordFailed")}: ${err}` });
      return false;
    }
  }, [getFriendByCode, media, t]);

  // -- Chat (TCP signaling only) --
  const handleSendText = useCallback(
    (text: string) => {
      const msg: ChatMessage = {
        id: nextChatId(),
        from: "local",
        text,
        timestamp: Date.now(),
      };
      setChatMessages((prev) => {
        const next = [...prev, msg];
        if (connectedPeer) persistMessages(connectedPeer.code, next);
        return next;
      });
      bridge.current
        .sendSignal({
          type: "chat",
          id: msg.id,
          text,
          timestamp: msg.timestamp,
        })
        .catch(() => {});
    },
    [connectedPeer, persistMessages],
  );

  const handleSendSticker = useCallback(
    (stickerId: string) => {
      const msg: ChatMessage = {
        id: nextChatId(),
        from: "local",
        sticker: stickerId,
        timestamp: Date.now(),
      };
      setChatMessages((prev) => {
        const next = [...prev, msg];
        if (connectedPeer) persistMessages(connectedPeer.code, next);
        return next;
      });
      bridge.current
        .sendSignal({
          type: "chat",
          id: msg.id,
          sticker: stickerId,
          timestamp: msg.timestamp,
        })
        .catch(() => {});
    },
    [connectedPeer, persistMessages],
  );

  const handleTyping = useCallback(() => {
    const now = Date.now();
    if (now - typingThrottleRef.current < 2000) return;
    typingThrottleRef.current = now;
    const name = profile?.displayName ?? "Peer";
    bridge.current
      .sendSignal({ type: "typing", displayName: name })
      .catch(() => {});
  }, [profile]);

  const handleToggleChat = useCallback(() => {
    setChatOpen((prev) => {
      if (!prev) setUnreadChat(0);
      return !prev;
    });
  }, []);

  const handleStartCallFromPeer = useCallback(
    (peer: DiscoveredPeer, audioOnly: boolean) => {
      initiateCall(peer.callingCode, peer.displayName, audioOnly);
    },
    [initiateCall],
  );

  const handleOpenChat = useCallback(
    async (peer: DiscoveredPeer) => {
      if (connectedPeer?.code === peer.callingCode) {
        setChatOpen(true);
        setUnreadChat(0);
        return;
      }
      try {
        const result = await bridge.current.connectToPeer(peer.callingCode);
        if (!result.success) {
          Toast.warning({ content: result.message });
          return;
        }
        setConnectedPeer({ name: peer.displayName, code: peer.callingCode });

        const stored = await bridge.current.loadMessages(peer.callingCode);
        const loaded: ChatMessage[] = stored.map((s) => ({
          id: s.id,
          from: s.from as "local" | "remote",
          text: s.text,
          sticker: s.sticker,
          fileName: s.fileName,
          timestamp: s.timestamp,
        }));
        setChatMessages(loaded);
        setChatOpen(true);
        setUnreadChat(0);
        const prof = profile ?? (await bridge.current.getProfile());
        const knownFriend = getFriendByCode(peer.callingCode);
        bridge.current
          .sendSignal({
            type: "hello",
            callingCode: prof.callingCode,
            displayName: prof.displayName,
            avatarId: knownFriend ? prof.avatarId : undefined,
            publicKey: knownFriend?.publicKey ?? undefined,
          })
          .catch(() => {});
      } catch (err) {
        Toast.error({ content: `${t("chat.connectFailed")}: ${err}` });
      }
    },
    [connectedPeer, getFriendByCode, profile, t],
  );

  const handleDisconnectChat = useCallback(() => {
    if (connectedPeer) {
      persistMessages(connectedPeer.code, chatMessages);
    }
    bridge.current.disconnectSignal().catch(() => {});
    setConnectedPeer(null);
    setChatMessages([]);
    setChatOpen(false);
    setUnreadChat(0);
    setRemoteTyping(null);
  }, [connectedPeer, chatMessages, persistMessages]);

  const handleApproveFriend = useCallback(async () => {
    if (!pendingFriendRequest) return;
    const req = pendingFriendRequest;
    setPendingFriendRequest(null);
    try {
      const friendKey = req.publicKey ?? generateFriendKey();
      const prof = profile ?? (await bridge.current.getProfile());
      const friend = await bridge.current.addFriend(
        req.callingCode,
        req.displayName,
        req.avatarId,
        friendKey,
      );
      setFriends((prev) => [
        ...prev.filter((f) => f.callingCode !== friend.callingCode),
        friend,
      ]);
      bridge.current
        .sendSignal({
          type: "friend-accept",
          callingCode: prof.callingCode,
          displayName: prof.displayName,
          avatarId: prof.avatarId,
          publicKey: friendKey,
        })
        .catch(() => {});
      setConnectedPeer({ name: req.displayName, code: req.callingCode });
      setChatOpen(true);
      Toast.success({ content: t("friends.approved") });
    } catch (err) {
      Toast.error({ content: `${err}` });
    }
  }, [pendingFriendRequest, profile, t]);

  const handleDenyFriend = useCallback(() => {
    setPendingFriendRequest(null);
    bridge.current.sendSignal({ type: "decline" }).catch(() => {});
  }, []);

  const sendTransfer = useCallback(
    async (opts: {
      fileName: string;
      bytes: Uint8Array;
      kind?: "file" | "voicemail";
      voicemailAuth?: string;
    }): Promise<boolean> => {
      const kind = opts.kind ?? "file";
      const fileId = `f-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      const checksum = await sha256Hex(opts.bytes);
      const b64 = bytesToBase64(opts.bytes);

      const CHUNK_SIZE = 49152;
      const totalChunks = Math.ceil(b64.length / CHUNK_SIZE);
      const prof = profile ?? (await bridge.current.getProfile());

      await bridge.current.sendSignal({
        type: "file-offer",
        fileId,
        fileName: opts.fileName,
        fileSize: opts.bytes.byteLength,
        fromCode: prof.callingCode,
        kind,
        voicemailAuth: opts.voicemailAuth,
        totalChunks,
        sha256: checksum,
      });

      const transfer: FileTransferProgress = {
        fileId,
        fileName: opts.fileName,
        byteSize: opts.bytes.byteLength,
        totalSize: totalChunks,
        received: 0,
        kind,
        sha256: checksum,
        verified: false,
        direction: "send",
        status: "offering",
      };
      setFileTransfers((prev) => new Map(prev).set(fileId, transfer));

      const sentMsg: ChatMessage = {
        id: `file-send-${fileId}`,
        from: "local",
        text:
          kind === "voicemail"
            ? `🎙️ ${opts.fileName} (${formatFileSize(opts.bytes.byteLength)})`
            : `📎 ${opts.fileName} (${formatFileSize(opts.bytes.byteLength)})`,
        fileName: opts.fileName,
        timestamp: Date.now(),
      };
      setChatMessages((prev) => [...prev, sentMsg]);

      const waitForAccept = (): Promise<boolean> =>
        new Promise((resolve) => {
          const check = () => {
            const ft = fileTransfersRef.current.get(fileId);
            if (!ft || ft.status === "offering") {
              setTimeout(check, 200);
              return;
            }
            resolve(ft.status === "transferring");
          };
          setTimeout(check, 200);
          setTimeout(() => resolve(false), 30000);
        });

      const accepted = await waitForAccept();
      if (!accepted) {
        setFileTransfers((prev) => {
          const next = new Map(prev);
          const existing = next.get(fileId);
          if (existing) next.set(fileId, { ...existing, status: "rejected" });
          return next;
        });
        return false;
      }

      for (let i = 0; i < totalChunks; i++) {
        const data = b64.slice(i * CHUNK_SIZE, (i + 1) * CHUNK_SIZE);
        await bridge.current.sendSignal({
          type: "file-chunk",
          fileId,
          offset: i,
          data,
          total: totalChunks,
        });
        setFileTransfers((prev) => {
          const next = new Map(prev);
          const t = next.get(fileId);
          if (t) next.set(fileId, { ...t, received: i + 1 });
          return next;
        });
      }

      await bridge.current.sendSignal({
        type: "file-complete",
        fileId,
        sha256: checksum,
      });
      setFileTransfers((prev) => {
        const next = new Map(prev);
        const existing = next.get(fileId);
        if (existing) {
          next.set(fileId, {
            ...existing,
            status: "complete",
            verified: true,
          });
        }
        return next;
      });
      return true;
    },
    [profile],
  );

  const handleSendFile = useCallback(
    async (file: File) => {
      try {
        const buffer = await file.arrayBuffer();
        const sent = await sendTransfer({
          fileName: file.name,
          bytes: new Uint8Array(buffer),
          kind: "file",
        });
        if (sent) {
          Toast.success({ content: `${t("chat.fileSent")}: ${file.name}` });
        }
      } catch (err) {
        Toast.error({ content: String(err) });
      }
    },
    [sendTransfer, t],
  );

  const handleStopVoicemail = useCallback(async () => {
    const recorder = voicemailRecorderRef.current;
    if (!recorder) {
      setVoicemailRecording(false);
      await handleEndCall();
      return;
    }

    const blob = await new Promise<Blob>((resolve, reject) => {
      try {
        recorder.onstop = () => {
          const parts = voicemailChunksRef.current;
          resolve(
            new Blob(parts, {
              type: recorder.mimeType || "video/webm",
            }),
          );
        };
        recorder.onerror = () => reject(new Error("Recorder error"));
        recorder.stop();
      } catch (err) {
        reject(err);
      }
    }).catch((err) => {
      Toast.error({ content: `${t("toast.recordFailed")}: ${err}` });
      return null;
    });

    voicemailRecorderRef.current = null;
    setVoicemailRecording(false);

    try {
      await bridge.current.stopVoicemail();
    } catch {
      /* ignore */
    }

    if (!blob || blob.size === 0) {
      await handleEndCall();
      return;
    }

    const peerCode = connectedPeerRef.current?.code;
    const friend = getFriendByCode(peerCode);
    if (!friend?.publicKey) {
      Toast.warning({ content: t("voicemail.onlyFriends") });
      await handleEndCall();
      return;
    }

    const buffer = await blob.arrayBuffer();
    const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
    const ext = blob.type.includes("mp4") ? "mp4" : "webm";
    const fileName = `voicemail_${peerCode ?? "peer"}_${timestamp}.${ext}`;
    const sent = await sendTransfer({
      fileName,
      bytes: new Uint8Array(buffer),
      kind: "voicemail",
      voicemailAuth: friend.publicKey,
    });
    if (sent) {
      Toast.success({ content: t("voicemail.sent") });
    } else {
      Toast.warning({ content: t("voicemail.sendFailed") });
    }
    await handleEndCall();
  }, [getFriendByCode, handleEndCall, sendTransfer, t]);

  const handleAcceptFile = useCallback(
    (fileId: string) => {
      bridge.current
        .sendSignal({ type: "file-accept", fileId })
        .catch(() => {});
      setFileTransfers((prev) => {
        const next = new Map(prev);
        const t = next.get(fileId);
        if (t)
          next.set(fileId, { ...t, status: "transferring" });
        return next;
      });
    },
    [],
  );

  const handleRejectFile = useCallback(
    (fileId: string) => {
      bridge.current
        .sendSignal({ type: "file-reject", fileId })
        .catch(() => {});
      setFileTransfers((prev) => {
        const next = new Map(prev);
        const t = next.get(fileId);
        if (t) next.set(fileId, { ...t, status: "rejected" });
        return next;
      });
    },
    [],
  );

  const handleOpenDownloadedFile = useCallback(async (path: string) => {
    try {
      await shellOpen(path);
    } catch (err) {
      console.error("[XDX] open downloaded file failed", err);
      Toast.error({ content: `${err}` });
    }
  }, []);

  const handleDialCallStarted = useCallback(
    async (result: import("./types/call").CallResult, audioOnly: boolean) => {
      if (!result.success) return;
      const name = result.message.split("(")[0]?.trim() || "Peer";
      const code =
        result.message.match(/\(([^)]+)\)/)?.[1] ?? result.sessionId ?? "";

      setConnectedPeer({ name, code });

      const stream = await media.startCamera(!audioOnly, true);
      if (!stream) {
        Toast.error({ content: t("toast.mediaFailed") });
        return;
      }

      const rtc = new WebRTCService();
      rtc.init(stream, true);
      rtcRef.current = rtc;

      rtc.onRemoteStream = (rs) => setRemoteStream(rs);
      rtc.onIceCandidate = (candidate) => {
        bridge.current
          .sendSignal({ type: "ice", candidate })
          .catch(() => {});
      };

      const offerSdp = await rtc.createOffer();
      const prof = profile ?? (await bridge.current.getProfile());
      await bridge.current.sendSignal({
        type: "offer",
        callingCode: prof.callingCode,
        displayName: prof.displayName,
        audioOnly,
        sdp: offerSdp,
      });

      setActiveCall({
        peerId: code,
        peerName: name,
        audioOnly,
        sessionId: result.sessionId ?? undefined,
        startTime: Date.now(),
      });
      setCallState(
        audioOnly ? CallState.InCallAudio : CallState.InCallVideo,
      );
    },
    [media, profile, t],
  );

  useEffect(() => {
    const isLiveCall =
      callState === CallState.InCallVideo ||
      callState === CallState.InCallAudio;
    if (!isLiveCall || !rtcRef.current) {
      if (metricsTimerRef.current) {
        clearInterval(metricsTimerRef.current);
        metricsTimerRef.current = null;
      }
      metricsLastBytesRef.current = null;
      setCallMetrics(null);
      return;
    }

    const sample = async () => {
      const snapshot = await rtcRef.current?.getMetricsSnapshot();
      if (!snapshot) return;

      const now = Date.now();
      const prev = metricsLastBytesRef.current;
      let bitrateBps = 0;
      if (prev && now > prev.ts && snapshot.bytesReceived >= prev.bytes) {
        bitrateBps = Math.round(
          ((snapshot.bytesReceived - prev.bytes) * 8 * 1000) / (now - prev.ts),
        );
      }
      metricsLastBytesRef.current = {
        bytes: snapshot.bytesReceived,
        ts: now,
      };

      const packetsTotal = snapshot.packetsReceived + snapshot.packetsLost;
      const packetLossPercent =
        packetsTotal > 0
          ? (snapshot.packetsLost / packetsTotal) * 100
          : 0;

      setCallMetrics({
        rttMs: snapshot.rttMs,
        bitrateBps,
        packetLossPercent,
        packetsDropped: snapshot.packetsDropped,
        fps: snapshot.fps,
      });
    };

    void sample();
    metricsTimerRef.current = setInterval(() => {
      void sample();
    }, 1000);

    return () => {
      if (metricsTimerRef.current) {
        clearInterval(metricsTimerRef.current);
        metricsTimerRef.current = null;
      }
    };
  }, [callState]);

  const friendCodeSet = new Set(friends.map((f) => f.callingCode));

  const filteredPeers = (searchQuery
    ? peers.filter(
        (p) =>
          p.displayName.toLowerCase().includes(searchQuery.toLowerCase()) ||
          p.callingCode.includes(searchQuery),
      )
    : peers
  )
    .slice()
    .sort((a, b) => {
      const aFriend = friendCodeSet.has(a.callingCode) ? 0 : 1;
      const bFriend = friendCodeSet.has(b.callingCode) ? 0 : 1;
      if (aFriend !== bFriend) return aFriend - bFriend;
      return a.displayName.localeCompare(b.displayName);
    });

  // -- Render sidebar --
  const renderSidebar = () => {
    const ownAvatar = getAvatarSrc(profile?.avatarId) ?? foxBandana;
    const voicemailEntries = downloads.filter((entry) =>
      entry.fileName.startsWith("voicemail_"),
    );
    const sidebarTabs: Array<{
      key: SidebarTab;
      icon: React.ReactNode;
      label: string;
    }> = [
      {
        key: "peers",
        icon: <IconUser size="small" />,
        label: t("sidebar.peers"),
      },
      {
        key: "voicemail",
        icon: <IconComment size="small" />,
        label: t("sidebar.voicemail"),
      },
      {
        key: "downloads",
        icon: <IconDownload size="small" />,
        label: t("sidebar.downloads"),
      },
    ];

    return (
      <div className="xdx-sidebar">
        <div
          className="xdx-titlebar"
          data-tauri-drag-region
          onMouseDown={handleTitlebarMouseDown}
        >
          <div className="xdx-titlebar-spacer" />
          <div className="xdx-titlebar-title">
            <img
              src={ownAvatar}
              alt=""
              className="xdx-titlebar-icon"
            />
            <span>{t("app.name")}</span>
          </div>
        </div>

        <div className="xdx-sidebar-header">
          <div className="xdx-app-brand">
            <div className="xdx-brand-icon">
              <img src={ownAvatar} alt="" className="xdx-brand-img" />
            </div>
            <div className="xdx-brand-text">
              <Title heading={5} style={{ color: "#fff", margin: 0 }}>
                {t("app.name")}
              </Title>
              <Text
                size="small"
                style={{
                  color: "rgba(255,255,255,0.52)",
                  fontSize: 11,
                }}
              >
                {t("app.nameCn")} · {t("app.subtitle")}
              </Text>
            </div>
          </div>
          <div className="xdx-sidebar-search">
            <IconSearch style={{ color: "rgba(255,255,255,0.32)" }} size="small" />
            <input
              className="xdx-search-input"
              placeholder={t("sidebar.search")}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
            />
          </div>
        </div>

        <div className="xdx-contact-list">
          {sidebarTab === "peers" && (
            <>
              {!coreReady ? (
                <div className="xdx-loading">
                  <Spin size="large" />
                  <Text
                    style={{
                      color: "rgba(255,255,255,0.52)",
                      marginTop: 12,
                      display: "block",
                    }}
                  >
                    {t("sidebar.connecting")}
                  </Text>
                </div>
              ) : filteredPeers.length === 0 && searchQuery ? (
                <div className="xdx-no-peers">
                  <Text
                    style={{
                      color: "rgba(255,255,255,0.45)",
                      textAlign: "center",
                      padding: "20px 12px",
                      display: "block",
                    }}
                  >
                    No matches for "{searchQuery}"
                  </Text>
                </div>
              ) : filteredPeers.length === 0 ? (
                <div className="xdx-no-peers">
                  <EventCard
                    image={foxSleeping}
                    title={t("sidebar.noPeers")}
                    description={t("sidebar.noPeersHint")}
                  />
                </div>
              ) : (
                filteredPeers.map((peer) => (
                  <div key={peer.callingCode} className="xdx-contact-item">
                    <div className="xdx-contact-avatar">
                      <Avatar
                        src={getAvatarSrc(getFriendByCode(peer.callingCode)?.avatarId)}
                        size="small"
                        style={{
                          background:
                            "linear-gradient(135deg, #667eea 0%, #764ba2 100%)",
                        }}
                      >
                        {peer.displayName.charAt(0)}
                      </Avatar>
                      <span className="xdx-status-dot online" />
                    </div>
                    <div className="xdx-contact-main">
                      <div className="xdx-contact-info">
                        <Text className="xdx-contact-name">{peer.displayName}</Text>
                        <Text size="small" className="xdx-contact-status">
                          {peer.callingCode} · {t("sidebar.online")}
                        </Text>
                      </div>
                      {callState === CallState.Idle && (
                        <div className="xdx-contact-actions">
                          <Tooltip content={t("chat.title")} position="top">
                            <button
                              className="xdx-action-btn"
                              onClick={() => handleOpenChat(peer)}
                            >
                              <IconComment size="small" />
                            </button>
                          </Tooltip>
                          <Tooltip content={t("dialpad.callVideo")} position="top">
                            <button
                              className="xdx-action-btn"
                              onClick={() => handleStartCallFromPeer(peer, false)}
                            >
                              <IconCamera size="small" />
                            </button>
                          </Tooltip>
                          <Tooltip content={t("dialpad.callAudio")} position="top">
                            <button
                              className="xdx-action-btn"
                              onClick={() => handleStartCallFromPeer(peer, true)}
                            >
                              <IconMicrophone size="small" />
                            </button>
                          </Tooltip>
                        </div>
                      )}
                    </div>
                  </div>
                ))
              )}
            </>
          )}

          {sidebarTab === "voicemail" && (
            <div className="xdx-downloads">
              {voicemailEntries.length === 0 ? (
                <div className="xdx-no-peers" style={{ padding: "24px 12px" }}>
                  <EventCard
                    image={foxSleeping}
                    title={t("sidebar.voicemail")}
                    description={t("voicemail.empty")}
                  />
                </div>
              ) : (
                voicemailEntries.map((entry) => (
                  <div key={entry.path} className="xdx-download-item voicemail">
                    <div className="xdx-download-main">
                      <div className="xdx-download-name" title={entry.fileName}>
                        {entry.fileName}
                      </div>
                      <div className="xdx-download-meta">
                        {formatFileSize(entry.sizeBytes)} ·{" "}
                        {formatShortTime(entry.modifiedAt)}
                      </div>
                      <div className="xdx-download-hash">
                        SHA-256: {entry.sha256.slice(0, 16)}...
                      </div>
                    </div>
                    <button
                      className="xdx-download-open"
                      onClick={() => handleOpenDownloadedFile(entry.path)}
                    >
                      {t("voicemail.play")}
                    </button>
                  </div>
                ))
              )}
            </div>
          )}

          {sidebarTab === "downloads" && (
            <div className="xdx-downloads">
              {downloads.length === 0 ? (
                <div className="xdx-no-peers" style={{ padding: "24px 12px" }}>
                  <EventCard
                    image={foxSleeping}
                    title={t("downloads.title")}
                    description={t("downloads.empty")}
                  />
                </div>
              ) : (
                downloads.map((entry) => (
                  <div key={entry.path} className="xdx-download-item">
                    <div className="xdx-download-main">
                      <div className="xdx-download-name" title={entry.fileName}>
                        {entry.fileName}
                      </div>
                      <div className="xdx-download-meta">
                        {formatFileSize(entry.sizeBytes)} ·{" "}
                        {formatShortTime(entry.modifiedAt)}
                      </div>
                      <div className="xdx-download-hash">
                        SHA-256: {entry.sha256.slice(0, 16)}...
                      </div>
                    </div>
                    <button
                      className="xdx-download-open"
                      onClick={() => handleOpenDownloadedFile(entry.path)}
                    >
                      {t("downloads.open")}
                    </button>
                  </div>
                ))
              )}
            </div>
          )}
        </div>

        <div className="xdx-sidebar-footer">
          <Tooltip content={t("sidebar.settings")} position="top">
            <button
              className="xdx-footer-btn"
              onClick={() => setSettingsOpen(true)}
            >
              <IconSetting size="small" />
            </button>
          </Tooltip>
          <div className="xdx-core-status">
            <span className={`xdx-engine-dot ${coreReady ? "ready" : ""}`} />
            <Text
              size="small"
              style={{ color: "rgba(255,255,255,0.45)", fontSize: 11 }}
            >
              {coreReady ? t("sidebar.engineReady") : t("sidebar.engineOffline")}
            </Text>
          </div>
        </div>

        <div className="xdx-sidebar-tabs">
          {sidebarTabs.map((tab, index) => (
            <React.Fragment key={tab.key}>
              {index === 1 && (
                <button
                  type="button"
                  className="xdx-tab xdx-tab-dial-action"
                  onClick={() => setDialPadOpen(true)}
                >
                  <IconPhone size="small" />
                  <span>{t("sidebar.dial")}</span>
                </button>
              )}
              <button
                type="button"
                className={`xdx-tab ${sidebarTab === tab.key ? "active" : ""}`}
                onClick={() => setSidebarTab(tab.key)}
              >
                {tab.icon}
                <span>{tab.label}</span>
              </button>
            </React.Fragment>
          ))}
        </div>
      </div>
    );
  };

  // -- Idle content with mascot gallery --
  const renderIdleContent = () => (
    <div className="xdx-idle-content">
      {/* Floating ambient orbs */}
      <div className="xdx-ambient-orb xdx-orb-1" />
      <div className="xdx-ambient-orb xdx-orb-2" />
      <div className="xdx-ambient-orb xdx-orb-3" />
      <div className="xdx-idle-glass xdx-idle-glass-1" />
      <div className="xdx-idle-glass xdx-idle-glass-2" />
      <div className="xdx-idle-glass xdx-idle-glass-3" />

      <div className="xdx-idle-hero">
        <EventCard
          image={foxCelebrate}
          title={t("card.welcome")}
          description={t("card.welcomeDesc")}
        />

        {/* Mascot gallery */}
        <div className="xdx-mascot-gallery">
          {IDLE_ART.map((art, i) => (
            <div key={i} className="xdx-mascot-thumb" style={{ animationDelay: `${i * 0.08}s` }}>
              <img src={art.src} alt={art.alt} draggable={false} />
            </div>
          ))}
        </div>

        <div style={{ marginTop: 16, textAlign: "center" }}>
          <Text style={{ color: "rgba(255,255,255,0.4)" }}>
            {t("app.selectContact")}
          </Text>
        </div>

        <div className="xdx-idle-tags">
          <Tag color="blue" size="large">
            {t("idle.tagQuic")}
          </Tag>
          <Tag color="violet" size="large">
            {t("idle.tagFec")}
          </Tag>
          <Tag color="cyan" size="large">
            {t("idle.tagE2e")}
          </Tag>
        </div>

        {profile && (
          <div className="xdx-idle-code-card">
            <Text
              size="small"
              style={{ color: "rgba(255,255,255,0.38)" }}
            >
              {t("idle.yourCode")}
            </Text>
            <span className="xdx-idle-code">{profile.callingCode}</span>
          </div>
        )}
      </div>
    </div>
  );

  // -- Connecting --
  const renderConnecting = () => (
    <div className="xdx-connecting">
      <div className="xdx-connecting-inner">
        <div className="xdx-connecting-pulse">
          <Avatar
            size="extra-large"
            style={{
              background:
                "linear-gradient(135deg, #667eea 0%, #764ba2 100%)",
            }}
          >
            {activeCall?.peerName.charAt(0) ?? "?"}
          </Avatar>
        </div>
        <Title heading={4} style={{ color: "#e0e0e0", marginTop: 24 }}>
          {t("call.connecting")}
        </Title>
        <Text style={{ color: "rgba(255,255,255,0.42)" }}>
          {t("call.quicConnecting")} {activeCall?.peerName}
        </Text>
        <Spin style={{ marginTop: 16 }} />
      </div>
    </div>
  );

  const inCall =
    callState === CallState.InCallVideo ||
    callState === CallState.InCallAudio ||
    callState === CallState.Voicemail;
  const voicemailEnabled = Boolean(
    getFriendByCode(activeCall?.peerId ?? connectedPeer?.code)?.publicKey,
  );
  const mobileEngaged = isMobile && (chatOpen || callState !== CallState.Idle);

  return (
    <>
      {!isMobile && (
        <div
          className="xdx-window-drag-strip"
          data-tauri-drag-region=""
          onMouseDown={handleTitlebarMouseDown}
        />
      )}
      <Layout
        className={`xdx-layout ${isMobile ? "xdx-mobile" : ""} ${
          mobileEngaged ? "mobile-engaged" : "mobile-sidebar"
        }`}
      >
        <Sider className="xdx-sider">{renderSidebar()}</Sider>
        <Content className="xdx-content">
          <div className={`xdx-content-main ${chatOpen && (connectedPeer || inCall) ? "with-chat" : ""}`}>
            {callState === CallState.Idle && renderIdleContent()}
            {callState === CallState.Connecting && renderConnecting()}
            {inCall && activeCall && (
              <CallView
                callState={callState}
                activeCall={activeCall}
                localStream={media.localStream}
                remoteStream={remoteStream}
                cameraOn={media.cameraOn}
                micOn={media.micOn}
                metrics={callMetrics}
                chatOpen={chatOpen}
                unreadChat={unreadChat}
                voicemailRecording={voicemailRecording}
                voicemailEnabled={voicemailEnabled}
                onToggleCamera={media.toggleCamera}
                onToggleMic={media.toggleMic}
                onEndCall={handleEndCall}
                onRecordVoicemail={handleRecordVoicemail}
                onStopVoicemail={handleStopVoicemail}
                onToggleChat={handleToggleChat}
              />
            )}
          </div>
          {chatOpen && (connectedPeer || inCall) && (
            <ChatPanel
              messages={chatMessages}
              onSendText={handleSendText}
              onSendSticker={handleSendSticker}
              onSendFile={handleSendFile}
              onAcceptFile={handleAcceptFile}
              onRejectFile={handleRejectFile}
              fileTransfers={fileTransfers}
              onClose={() => {
                setChatOpen(false);
                if (!inCall && connectedPeer) handleDisconnectChat();
              }}
              remoteTyping={remoteTyping}
              onTyping={handleTyping}
            />
          )}
        </Content>
      </Layout>

      {/* Incoming Call Modal */}
      <Modal
        visible={callState === CallState.IncomingCall && !!pendingOffer}
        closable={false}
        footer={null}
        centered
        maskClosable={false}
        className="xdx-incoming-modal"
        width={400}
        maskStyle={{
          backdropFilter: "blur(16px)",
          background: "rgba(0,0,0,0.6)",
        }}
      >
        {pendingOffer && (
          <div className="xdx-incoming-content">
            <div className="xdx-incoming-ring">
              <div className="xdx-ring-wave xdx-ring-1" />
              <div className="xdx-ring-wave xdx-ring-2" />
              <div className="xdx-ring-wave xdx-ring-3" />
              <Avatar
                size="extra-large"
                className="xdx-incoming-avatar"
                style={{
                  background:
                    "linear-gradient(135deg, #667eea 0%, #764ba2 100%)",
                }}
              >
                {pendingOffer.displayName.charAt(0)}
              </Avatar>
            </div>
            <Text
              style={{
                color: "rgba(255,255,255,0.45)",
                marginTop: 28,
                fontSize: 13,
                letterSpacing: 2,
                textTransform: "uppercase",
              }}
            >
              {t("incoming.title")}
            </Text>
            <Title
              heading={3}
              style={{ color: "#fff", margin: "6px 0 4px" }}
            >
              {pendingOffer.displayName}
            </Title>
            <Text
              style={{ color: "rgba(255,255,255,0.38)", fontSize: 13 }}
            >
              {pendingOffer.audioOnly
                ? t("incoming.audioCall")
                : t("incoming.videoCall")}{" "}
              · {t("incoming.viaSankaku")}
            </Text>
            <div className="xdx-incoming-actions">
              <div className="xdx-call-action">
                <button
                  className="xdx-btn-decline"
                  onClick={handleDeclineCall}
                >
                  <IconClose size="extra-large" />
                </button>
                <Text
                  size="small"
                  style={{
                    color: "rgba(255,255,255,0.45)",
                    marginTop: 8,
                  }}
                >
                  {t("incoming.decline")}
                </Text>
              </div>
              <div className="xdx-call-action">
                <button
                  className="xdx-btn-audio"
                  onClick={() => handleAcceptCall(true)}
                >
                  <IconMicrophone size="extra-large" />
                </button>
                <Text
                  size="small"
                  style={{
                    color: "rgba(255,255,255,0.45)",
                    marginTop: 8,
                  }}
                >
                  {t("incoming.audio")}
                </Text>
              </div>
              <div className="xdx-call-action">
                <button
                  className="xdx-btn-accept"
                  onClick={() => handleAcceptCall(false)}
                >
                  <IconCamera size="extra-large" />
                </button>
                <Text
                  size="small"
                  style={{
                    color: "rgba(255,255,255,0.45)",
                    marginTop: 8,
                  }}
                >
                  {t("incoming.video")}
                </Text>
              </div>
            </div>
          </div>
        )}
      </Modal>

      {/* Friend Request Modal */}
      <Modal
        visible={!!pendingFriendRequest}
        closable={false}
        footer={null}
        centered
        maskClosable={false}
        className="xdx-incoming-modal"
        width={360}
        maskStyle={{
          backdropFilter: "blur(12px)",
          background: "rgba(0,0,0,0.5)",
        }}
      >
        {pendingFriendRequest && (
          <div className="xdx-friend-request-modal">
            <Avatar
              size="extra-large"
              style={{
                background:
                  "linear-gradient(135deg, #667eea 0%, #764ba2 100%)",
                marginBottom: 16,
              }}
            >
              {pendingFriendRequest.displayName.charAt(0)}
            </Avatar>
            <Title heading={4} style={{ color: "#fff", margin: "8px 0 4px" }}>
              {pendingFriendRequest.displayName}
            </Title>
            <Text style={{ color: "rgba(255,255,255,0.45)", fontSize: 13 }}>
              {t("friends.requestDesc")}
            </Text>
            <Text
              size="small"
              style={{
                color: "rgba(255,255,255,0.3)",
                display: "block",
                margin: "4px 0 20px",
              }}
            >
              {pendingFriendRequest.callingCode}
            </Text>
            <div style={{ display: "flex", gap: 12, justifyContent: "center" }}>
              <button className="xdx-btn-decline" onClick={handleDenyFriend}>
                <IconClose size="large" />
              </button>
              <button className="xdx-btn-accept" onClick={handleApproveFriend}>
                <IconUserAdd size="large" />
              </button>
            </div>
          </div>
        )}
      </Modal>

      <Modal
        visible={dialPadOpen}
        title={t("dialpad.title")}
        footer={null}
        centered
        onCancel={() => setDialPadOpen(false)}
        className="xdx-incoming-modal xdx-dial-modal"
        width={420}
        maskStyle={{
          backdropFilter: "blur(14px)",
          background: "rgba(0,0,0,0.52)",
        }}
      >
        {profile && (
          <DialPad
            callingCode={profile.callingCode}
            onCallStarted={(result, audioOnly) => {
              setDialPadOpen(false);
              void handleDialCallStarted(result, audioOnly);
            }}
          />
        )}
      </Modal>

      <SettingsPanel
        open={settingsOpen}
        profile={profile}
        onClose={() => setSettingsOpen(false)}
        onProfileChanged={handleProfileChanged}
        uiScale={uiScale}
        onUiScaleChange={setUiScale}
      />
    </>
  );
};

// ---------------------------------------------------------------------------
// Root wrapper with I18nProvider
// ---------------------------------------------------------------------------

const App: React.FC = () => {
  const [locale, setLocale] = useState<LocaleCode>("en");

  useEffect(() => {
    SankakuBridge.getInstance()
      .getProfile()
      .then((p) => setLocale(p.language as LocaleCode))
      .catch(() => {});
  }, []);

  useEffect(() => {
    const handler = () => {
      SankakuBridge.getInstance()
        .getProfile()
        .then((p) => setLocale(p.language as LocaleCode))
        .catch(() => {});
    };
    window.addEventListener("xdx-profile-changed", handler);
    return () => window.removeEventListener("xdx-profile-changed", handler);
  }, []);

  return (
    <I18nProvider locale={locale}>
      <AppInner />
    </I18nProvider>
  );
};

export default App;
