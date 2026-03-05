import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  CallResult,
  DiscoveredPeer,
  DownloadedFileEntry,
  Friend,
  IncomingCallPayload,
  SignalMessage,
  StoredMessage,
  UserProfile,
  VoicemailResult,
} from "../types/call";

export type IncomingCallHandler = (payload: IncomingCallPayload) => void;
export type PeerDiscoveredHandler = (peer: DiscoveredPeer) => void;
export type PeerLostHandler = (code: string) => void;
export type SignalingMessageHandler = (msg: SignalMessage) => void;
export type SignalingDisconnectHandler = () => void;

class SankakuBridge {
  private static instance: SankakuBridge;

  private incomingListeners: IncomingCallHandler[] = [];
  private discoveredListeners: PeerDiscoveredHandler[] = [];
  private lostListeners: PeerLostHandler[] = [];
  private signalingListeners: SignalingMessageHandler[] = [];
  private disconnectListeners: SignalingDisconnectHandler[] = [];
  private unlistenFns: UnlistenFn[] = [];
  private ready = false;

  private constructor() {}

  static getInstance(): SankakuBridge {
    if (!SankakuBridge.instance) {
      SankakuBridge.instance = new SankakuBridge();
    }
    return SankakuBridge.instance;
  }

  get isReady(): boolean {
    return this.ready;
  }

  // -----------------------------------------------------------------------
  // Lifecycle
  // -----------------------------------------------------------------------

  async initialize(): Promise<CallResult> {
    if (this.ready) {
      return { success: true, message: "Already initialised", sessionId: null };
    }

    const u1 = await listen<IncomingCallPayload>("incoming-call", (e) => {
      for (const h of this.incomingListeners) {
        try {
          h(e.payload);
        } catch (err) {
          console.error("[Bridge] incoming handler error:", err);
        }
      }
    });

    const u2 = await listen<DiscoveredPeer>("peer-discovered", (e) => {
      for (const h of this.discoveredListeners) {
        try {
          h(e.payload);
        } catch (err) {
          console.error("[Bridge] discovered handler error:", err);
        }
      }
    });

    const u3 = await listen<string>("peer-lost", (e) => {
      for (const h of this.lostListeners) {
        try {
          h(e.payload);
        } catch (err) {
          console.error("[Bridge] lost handler error:", err);
        }
      }
    });

    const u4 = await listen<string>("signaling-message", (e) => {
      try {
        const msg = JSON.parse(e.payload) as SignalMessage;
        for (const h of this.signalingListeners) {
          try {
            h(msg);
          } catch (err) {
            console.error("[Bridge] signaling handler error:", err);
          }
        }
      } catch {
        console.warn("[Bridge] unparseable signaling message:", e.payload);
      }
    });

    const u5 = await listen<void>("signaling-disconnected", () => {
      for (const h of this.disconnectListeners) {
        try {
          h();
        } catch (err) {
          console.error("[Bridge] disconnect handler error:", err);
        }
      }
    });

    this.unlistenFns = [u1, u2, u3, u4, u5];

    const result = await invoke<CallResult>("init_sankaku_core");
    this.ready = result.success;
    return result;
  }

  async destroy(): Promise<void> {
    for (const fn of this.unlistenFns) fn();
    this.unlistenFns = [];
    this.incomingListeners = [];
    this.discoveredListeners = [];
    this.lostListeners = [];
    this.signalingListeners = [];
    this.disconnectListeners = [];
    this.ready = false;
  }

  // -----------------------------------------------------------------------
  // Event subscriptions
  // -----------------------------------------------------------------------

  onIncomingCall(handler: IncomingCallHandler): () => void {
    this.incomingListeners.push(handler);
    return () => {
      this.incomingListeners = this.incomingListeners.filter(
        (h) => h !== handler,
      );
    };
  }

  onPeerDiscovered(handler: PeerDiscoveredHandler): () => void {
    this.discoveredListeners.push(handler);
    return () => {
      this.discoveredListeners = this.discoveredListeners.filter(
        (h) => h !== handler,
      );
    };
  }

  onPeerLost(handler: PeerLostHandler): () => void {
    this.lostListeners.push(handler);
    return () => {
      this.lostListeners = this.lostListeners.filter((h) => h !== handler);
    };
  }

  onSignalingMessage(handler: SignalingMessageHandler): () => void {
    this.signalingListeners.push(handler);
    return () => {
      this.signalingListeners = this.signalingListeners.filter(
        (h) => h !== handler,
      );
    };
  }

  onSignalingDisconnect(handler: SignalingDisconnectHandler): () => void {
    this.disconnectListeners.push(handler);
    return () => {
      this.disconnectListeners = this.disconnectListeners.filter(
        (h) => h !== handler,
      );
    };
  }

  // -----------------------------------------------------------------------
  // Profile
  // -----------------------------------------------------------------------

  async getProfile(): Promise<UserProfile> {
    return invoke<UserProfile>("get_profile");
  }

  async updateProfile(fields: {
    displayName?: string;
    avatarId?: string;
    language?: string;
  }): Promise<UserProfile> {
    return invoke<UserProfile>("update_profile", fields);
  }

  // -----------------------------------------------------------------------
  // Peers & Calling
  // -----------------------------------------------------------------------

  async getDiscoveredPeers(): Promise<DiscoveredPeer[]> {
    return invoke<DiscoveredPeer[]>("get_discovered_peers");
  }

  async connectToPeer(code: string): Promise<CallResult> {
    return invoke<CallResult>("connect_to_peer", { code });
  }

  async sendSignal(message: SignalMessage): Promise<void> {
    return invoke<void>("send_signal", {
      message: JSON.stringify(message),
    });
  }

  async disconnectSignal(): Promise<void> {
    return invoke<void>("disconnect_signal");
  }

  async dialCode(
    code: string,
    audioOnly: boolean = false,
  ): Promise<CallResult> {
    return invoke<CallResult>("dial_code", { code, audioOnly });
  }

  async startCall(
    peerId: string,
    audioOnly: boolean = false,
  ): Promise<CallResult> {
    return invoke<CallResult>("start_call", { peerId, audioOnly });
  }

  async acceptCall(): Promise<CallResult> {
    return invoke<CallResult>("accept_call");
  }

  async endCall(): Promise<CallResult> {
    return invoke<CallResult>("end_call");
  }

  async recordVoicemail(): Promise<VoicemailResult> {
    return invoke<VoicemailResult>("record_voicemail");
  }

  async stopVoicemail(): Promise<VoicemailResult> {
    return invoke<VoicemailResult>("stop_voicemail");
  }

  // -----------------------------------------------------------------------
  // Friends
  // -----------------------------------------------------------------------

  async getFriends(): Promise<Friend[]> {
    return invoke<Friend[]>("get_friends");
  }

  async addFriend(
    callingCode: string,
    displayName: string,
    avatarId?: string,
    publicKey?: string,
  ): Promise<Friend> {
    return invoke<Friend>("add_friend", {
      callingCode,
      displayName,
      avatarId,
      publicKey,
    });
  }

  async removeFriend(callingCode: string): Promise<void> {
    return invoke<void>("remove_friend", { callingCode });
  }

  async isFriend(callingCode: string): Promise<boolean> {
    return invoke<boolean>("is_friend", { callingCode });
  }

  // -----------------------------------------------------------------------
  // Conversations
  // -----------------------------------------------------------------------

  async saveMessages(
    peerCode: string,
    messages: StoredMessage[],
  ): Promise<void> {
    return invoke<void>("save_messages", { peerCode, messages });
  }

  async loadMessages(peerCode: string): Promise<StoredMessage[]> {
    return invoke<StoredMessage[]>("load_messages", { peerCode });
  }

  async listConversations(): Promise<string[]> {
    return invoke<string[]>("list_conversations");
  }

  // -----------------------------------------------------------------------
  // File Transfer
  // -----------------------------------------------------------------------

  async saveReceivedFile(
    filename: string,
    dataB64: string,
    expectedSha256?: string,
  ): Promise<DownloadedFileEntry> {
    return invoke<DownloadedFileEntry>("save_received_file", {
      filename,
      dataB64,
      expectedSha256,
    });
  }

  async listReceivedFiles(): Promise<DownloadedFileEntry[]> {
    return invoke<DownloadedFileEntry[]>("list_received_files");
  }

  async setDownloadDirectory(path: string): Promise<void> {
    return invoke<void>("set_download_directory", { path });
  }

  // -----------------------------------------------------------------------
  // Custom Avatar
  // -----------------------------------------------------------------------

  async saveCustomAvatar(dataB64: string): Promise<void> {
    return invoke<void>("save_custom_avatar", { dataB64 });
  }

  async loadCustomAvatar(): Promise<string | null> {
    return invoke<string | null>("load_custom_avatar");
  }
}

export default SankakuBridge;
