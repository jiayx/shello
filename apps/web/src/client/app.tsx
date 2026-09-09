import { t, locale } from "./i18n";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  mountTerminal,
  type HostTerminalProfile,
  type TerminalController,
  type TerminalSize,
} from "./terminal";
import {
  BinaryMessageType,
  binarySocketDataToArrayBuffer,
  encodeBinaryMessage,
} from "../protocol";

type SessionInfo = {
  sessionId: string;
  viewerUrl: string;
  hostWebSocketUrl: string;
  viewerWebSocketUrl: string;
};

type SessionStatus = {
  role: "host" | "viewer" | null;
  state: "idle" | "ready" | "active" | "closed";
  hostState: HostState;
  hostConnected: boolean;
  viewerCount: number;
  viewerId: string | null;
  viewerToken: string | null;
  canWrite: boolean;
  controllerViewerId: string | null;
  controlLeaseExpiresAt: number | null;
  pendingControlRequest: {
    viewerId: string;
    leaseSeconds: number;
  } | null;
  hasPendingControlRequest: boolean;
  sessionExpiresAt: number | null;
  hostDisconnectDeadline: number | null;
  pendingRequestExpiresAt: number | null;
  terminalSize: TerminalSize | null;
  terminalProfile: HostTerminalProfile | null;
};

type PlatformTab = "macos" | "linux" | "windows";
type TransportState = "idle" | "connecting" | "connected" | "reconnecting" | "closed" | "error";
type HostState = "waiting" | "online" | "reconnecting" | "offline";
type CopyLabel = "Copy" | "Copied" | "Copy failed";
type FlashPalette = {
  first: string;
  firstGlow: string;
  second: string;
  secondGlow: string;
};

const OFFLINE_STATUS_POLL_MS = 3000;
const TERMINAL_SIZE_FALLBACK_MS = 750;
const MAX_PENDING_TERMINAL_OUTPUT_BYTES = 4 * 1024 * 1024;
const MAX_STDIN_FRAME_BYTES = 32 * 1024;
const MAX_WEBSOCKET_BUFFERED_INPUT_BYTES = 512 * 1024;
const textEncoder = new TextEncoder();

class SessionEndedError extends Error {}

export function App() {
  const terminalRef = useRef<HTMLDivElement | null>(null);
  const detailsDialogRef = useRef<HTMLDialogElement | null>(null);
  const terminal = useRef<TerminalController | null>(null);
  const requestControlButtonRef = useRef<HTMLButtonElement | null>(null);
  const socket = useRef<WebSocket | null>(null);
  const inputCleanup = useRef<(() => void) | null>(null);
  const canWriteRef = useRef(false);
  const previousStatusRef = useRef<SessionStatus | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(readSessionId());
  const [sessionStatus, setSessionStatus] = useState<SessionStatus | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [creating, setCreating] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [requestingControl, setRequestingControl] = useState(false);
  const [selectedPlatform, setSelectedPlatform] = useState<PlatformTab>(() => detectPlatformTab());
  const [statusNote, setStatusNote] = useState<string | null>(null);
  const [shareCopyLabel, setShareCopyLabel] = useState<CopyLabel>("Copy");
  const [platformCopyLabel, setPlatformCopyLabel] = useState<CopyLabel>("Copy");
  const [transportState, setTransportState] = useState<TransportState>("idle");
  const [now, setNow] = useState(() => Date.now());
  const reconnectAttempts = useRef(0);
  const reconnectTimer = useRef<number | null>(null);
  const suppressReconnectRef = useRef(false);
  const shareCopyTimerRef = useRef<number | null>(null);
  const platformCopyTimerRef = useRef<number | null>(null);
  const sessionInfo = useMemo(() => buildSessionInfo(sessionId), [sessionId]);

  const shareUrl = useMemo(() => {
    return sessionInfo?.viewerUrl ?? "";
  }, [sessionInfo]);
  const shellBootstrapCommand = useMemo(() => {
    if (!sessionId || typeof window === "undefined") {
      return "";
    }

    return `curl -fsSL '${window.location.origin}/start?session=${sessionId}' | sh`;
  }, [sessionId]);
  const windowsBootstrapCommand = useMemo(() => {
    if (!sessionId || typeof window === "undefined") {
      return "";
    }

    return `irm '${window.location.origin}/start.ps1?session=${sessionId}' | iex`;
  }, [sessionId]);
  const platformCommand = useMemo(() => {
    if (selectedPlatform === "windows") {
      return windowsBootstrapCommand;
    }
    return shellBootstrapCommand;
  }, [selectedPlatform, shellBootstrapCommand, windowsBootstrapCommand]);

  useEffect(() => {
    const dialog = detailsDialogRef.current;
    if (!dialog) return;
    if (detailsOpen && sessionId) {
      if (!dialog.open) dialog.showModal();
    } else if (dialog.open) {
      dialog.close();
    }
  }, [detailsOpen, sessionId]);

  useEffect(() => {
    if (!statusNote) return;
    const timer = window.setTimeout(() => setStatusNote(null), 6000);
    return () => window.clearTimeout(timer);
  }, [statusNote]);

  useEffect(() => {
    setStatusNote(null);
  }, [sessionStatus?.hostState]);

  useEffect(() => {
    canWriteRef.current = Boolean(sessionStatus?.canWrite);
  }, [sessionStatus?.canWrite]);

  useEffect(() => {
    return () => {
      if (shareCopyTimerRef.current !== null) {
        window.clearTimeout(shareCopyTimerRef.current);
      }
      if (platformCopyTimerRef.current !== null) {
        window.clearTimeout(platformCopyTimerRef.current);
      }
    };
  }, []);

  useEffect(() => {
    if (!sessionId) {
      return;
    }

    const timer = window.setInterval(() => {
      setNow(Date.now());
    }, 1000);

    return () => {
      window.clearInterval(timer);
    };
  }, [sessionId]);

  useEffect(() => {
    if (!terminalRef.current) {
      return;
    }

    terminal.current = mountTerminal(terminalRef.current);
    return () => {
      if (reconnectTimer.current !== null) {
        window.clearTimeout(reconnectTimer.current);
      }
      inputCleanup.current?.();
      socket.current?.close();
      terminal.current?.dispose();
    };
  }, []);

  useEffect(() => {
    if (!sessionId || !terminal.current) {
      return;
    }

    const currentSessionId = sessionId;
    let cancelled = false;
    let activeSocket: WebSocket | null = null;
    let connectionGeneration = 0;
    let terminalReady = false;
    let pendingOutput: Uint8Array[] = [];
    let pendingOutputBytes = 0;
    let terminalReadyTimer: number | null = null;
    let inputBackpressureNotified = false;
    type ConnectionAttempt = {
      isStale: () => boolean;
      ownsSocket: (ws: WebSocket) => boolean;
      scheduleRetry: (delay: number, retry: () => void) => void;
    };

    function clearReconnectTimer() {
      if (reconnectTimer.current !== null) {
        window.clearTimeout(reconnectTimer.current);
        reconnectTimer.current = null;
      }
    }

    function closeActiveSocket() {
      const currentSocket = activeSocket;
      if (currentSocket) {
        currentSocket.close();
      }
      if (socket.current === currentSocket) {
        socket.current = null;
      }
      activeSocket = null;
    }

    function clearTerminalReadyTimer() {
      if (terminalReadyTimer !== null) {
        window.clearTimeout(terminalReadyTimer);
        terminalReadyTimer = null;
      }
    }

    function stageTtyOutput(payload: Uint8Array) {
      if (terminalReady) {
        terminal.current?.write(payload);
        return;
      }

      const copy = payload.slice();
      pendingOutput.push(copy);
      pendingOutputBytes += copy.byteLength;

      if (pendingOutputBytes >= MAX_PENDING_TERMINAL_OUTPUT_BYTES) {
        setStatusNote("Host size is delayed; rendering with the compatible fallback size.");
        flushPendingOutput();
        return;
      }

      if (terminalReadyTimer === null) {
        terminalReadyTimer = window.setTimeout(() => {
          terminalReadyTimer = null;
          if (!terminalReady) {
            setStatusNote("Host size is delayed; rendering with the compatible fallback size.");
            flushPendingOutput();
          }
        }, TERMINAL_SIZE_FALLBACK_MS);
      }
    }

    async function fetchSessionStatus(): Promise<SessionStatus | null> {
      const response = await fetch(sessionStatusURL(currentSessionId));
      if (!response.ok) {
        if (shouldRetrySessionStatus(response)) {
          return null;
        }
        throw new SessionEndedError();
      }
      return (await response.json()) as SessionStatus;
    }

    function startAttempt(): ConnectionAttempt {
      const generation = ++connectionGeneration;
      return {
        isStale() {
          return cancelled || generation !== connectionGeneration;
        },
        ownsSocket(ws: WebSocket) {
          return !this.isStale() && activeSocket === ws;
        },
        scheduleRetry(delay: number, retry: () => void) {
          reconnectTimer.current = window.setTimeout(() => {
            if (this.isStale()) {
              return;
            }
            retry();
          }, delay);
        },
      };
    }

    async function connectViewer() {
      const attempt = startAttempt();
      clearReconnectTimer();
      setConnecting(true);
      setTransportState("connecting");
      let status: SessionStatus | null;

      try {
        status = await fetchSessionStatus();
      } catch (error) {
        if (attempt.isStale()) {
          return;
        }
        if (error instanceof SessionEndedError) {
          setStatusNote("Session ended. Refresh or create a new session.");
          setTransportState("closed");
          setConnecting(false);
          return;
        }
        setStatusNote("Unable to reach the server. Retrying...");
        setTransportState("reconnecting");
        setConnecting(false);
        void scheduleReconnect(attempt);
        return;
      }

      if (attempt.isStale()) {
        return;
      }

      if (!status) {
        setStatusNote("Unable to reach the server. Retrying...");
        setTransportState("reconnecting");
        setConnecting(false);
        void scheduleReconnect(attempt);
        return;
      }

      if (status.state === "closed") {
        applySessionStatus(status);
        setTransportState("closed");
        setConnecting(false);
        void scheduleOfflineStatusPoll(attempt);
        return;
      }

      applySessionStatus(status);

      const ws = new WebSocket(viewerSocketURL(currentSessionId));
      ws.binaryType = "arraybuffer";
      activeSocket = ws;
      socket.current = ws;

      ws.addEventListener("open", () => {
        if (!attempt.ownsSocket(ws)) {
          return;
        }
        clearReconnectTimer();
        setTransportState("connected");
        setStatusNote(null);
        setConnecting(false);
        reconnectAttempts.current = 0;
      });

      ws.addEventListener("message", (event: MessageEvent<unknown>) => {
        if (!attempt.ownsSocket(ws)) {
          return;
        }
        const data = event.data;
        if (typeof data !== "string") {
          void handleBinarySocketMessage(data, ws, (payload) => {
            stageTtyOutput(payload);
          });
          return;
        }

        const parsed = parseControlFrame(data);
        if (parsed) {
          handleControlFrame(parsed);
          return;
        }
      });

      ws.addEventListener("close", () => {
        if (!attempt.ownsSocket(ws)) {
          return;
        }
        activeSocket = null;
        if (socket.current === ws) {
          socket.current = null;
        }
        if (suppressReconnectRef.current) {
          return;
        }
        setTransportState("reconnecting");
        setConnecting(false);
        void scheduleReconnect(attempt);
      });

      ws.addEventListener("error", () => {
        if (!attempt.ownsSocket(ws)) {
          return;
        }
        setTransportState("error");
      });

      inputCleanup.current?.();
      inputCleanup.current = terminal.current?.onData((value) => {
        if (!canWriteRef.current) {
          flashRequestControlButton();
          return;
        }
        if (sendTerminalInput(ws, value)) {
          inputBackpressureNotified = false;
          return;
        }
        if (!inputBackpressureNotified) {
          inputBackpressureNotified = true;
          setStatusNote("Input paused while the connection catches up. Please try again shortly.");
        }
      }) ?? null;
    }

    async function scheduleReconnect(attempt: ConnectionAttempt) {
      if (attempt.isStale()) {
        return;
      }
      clearReconnectTimer();
      reconnectAttempts.current += 1;
      const attemptNumber = reconnectAttempts.current;
      const delay = Math.min(1000 * attemptNumber, 5000);

      try {
        const status = await fetchSessionStatus();
        if (attempt.isStale()) {
          return;
        }

        if (!status) {
          attempt.scheduleRetry(delay, () => void scheduleReconnect(attempt));
          return;
        }

        applySessionStatus(status);
        if (status.state === "closed") {
          setTransportState("closed");
          void scheduleOfflineStatusPoll(attempt);
          return;
        }

        attempt.scheduleRetry(delay, () => void connectViewer());
      } catch (error) {
        if (attempt.isStale()) {
          return;
        }
        if (error instanceof SessionEndedError) {
          setTransportState("closed");
          setStatusNote("Session ended. Refresh or create a new session.");
          return;
        }
        attempt.scheduleRetry(delay, () => void scheduleReconnect(attempt));
      }
    }

    async function scheduleOfflineStatusPoll(attempt: ConnectionAttempt) {
      if (attempt.isStale()) {
        return;
      }
      clearReconnectTimer();
      setConnecting(false);
      setTransportState("closed");

      reconnectTimer.current = window.setTimeout(async () => {
        if (attempt.isStale()) {
          return;
        }

        try {
          const status = await fetchSessionStatus();
          if (attempt.isStale()) {
            return;
          }

          if (!status) {
            void scheduleOfflineStatusPoll(attempt);
            return;
          }

          applySessionStatus(status);
          if (status.state !== "closed" || status.hostConnected) {
            void connectViewer();
            return;
          }

          void scheduleOfflineStatusPoll(attempt);
        } catch (error) {
          if (error instanceof SessionEndedError) {
            setStatusNote("Session ended. Refresh or create a new session.");
            return;
          }
          if (!attempt.isStale()) {
            void scheduleOfflineStatusPoll(attempt);
          }
        }
      }, OFFLINE_STATUS_POLL_MS);
    }

    function handleControlFrame(frame: Record<string, unknown>) {
      if (frame.type === "session.status") {
        const payload = frame.payload as SessionStatus;
        const previous = previousStatusRef.current;
        if (previous) {
          if (!previous.canWrite && payload.canWrite) {
            setStatusNote(null);
            terminal.current?.focus();
          } else if (previous.canWrite && !payload.canWrite) {
            setStatusNote(
              payload.controllerViewerId && payload.controllerViewerId !== payload.viewerId
                ? "Control moved to another viewer."
                : "Control was revoked or the lease expired.",
            );
          } else if (
            previous.pendingControlRequest &&
            !payload.pendingControlRequest &&
            !payload.canWrite
          ) {
            setStatusNote("Control request was declined or cleared.");
          } else if (!previous.pendingControlRequest && payload.pendingControlRequest) {
            setStatusNote("Control request sent. Waiting for host approval.");
          }
        }
        previousStatusRef.current = payload;
        storeViewerToken(currentSessionId, payload.viewerToken);
        applySessionStatus(payload);
        setRequestingControl(Boolean(payload.pendingControlRequest));
        return;
      }
    }

    function applySessionStatus(status: SessionStatus) {
      setSessionStatus(status);
      terminal.current?.setHostTerminalProfile(status.terminalProfile);
      if (status.terminalSize) {
        terminal.current?.resize(status.terminalSize);
        flushPendingOutput();
      }
    }

    function flushPendingOutput() {
      terminalReady = true;
      clearTerminalReadyTimer();
      if (pendingOutput.length === 0) {
        return;
      }
      for (const payload of pendingOutput) {
        terminal.current?.write(payload);
      }
      pendingOutput = [];
      pendingOutputBytes = 0;
    }

    void connectViewer();

    return () => {
      cancelled = true;
      clearReconnectTimer();
      clearTerminalReadyTimer();
      inputCleanup.current?.();
      inputCleanup.current = null;
      closeActiveSocket();
    };
  }, [sessionId]);

  async function createSession(options: { openInNewTab?: boolean } = {}) {
    if (creating) return;
    const openInNewTab = Boolean(sessionId) || options.openInNewTab;
    // Reserve the tab during the click so popup blockers do not reject it after fetch.
    const newTab = openInNewTab ? window.open("about:blank", "_blank") : null;
    if (openInNewTab && !newTab) {
      setStatusNote("Allow pop-ups to open a new session. Your current session is still connected.");
      return;
    }
    if (newTab) newTab.opener = null;
    setCreating(true);
    try {
      const response = await fetch("/api/session", { method: "POST" });
      if (!response.ok) {
        throw new Error("failed to create session");
      }

      const created = (await response.json()) as SessionInfo;
      const createdInfo = normalizeSessionInfo(created);

      if (newTab) {
        if (newTab.closed) {
          setStatusNote("The new tab was closed. Your current session is unchanged.");
          return;
        }
        newTab.location.replace(createdInfo.viewerUrl);
        return;
      }

      suppressReconnectRef.current = true;
      if (reconnectTimer.current !== null) {
        window.clearTimeout(reconnectTimer.current);
        reconnectTimer.current = null;
      }
      inputCleanup.current?.();
      inputCleanup.current = null;
      socket.current?.close();
      socket.current = null;
      setSessionId(created.sessionId);
      reconnectAttempts.current = 0;
      window.history.replaceState({}, "", createdInfo.viewerUrl);
      previousStatusRef.current = null;
      setStatusNote(null);
      setSessionStatus(null);
      setTransportState("idle");
      setConnecting(false);
      terminal.current?.reset();
      terminal.current?.setHostTerminalProfile(null);
    } catch {
      newTab?.close();
      setStatusNote("Could not create a new session. Please try again.");
    } finally {
      suppressReconnectRef.current = false;
      setCreating(false);
    }
  }

  function handleCreateSessionClick(event: React.MouseEvent<HTMLButtonElement>) {
    void createSession({
      openInNewTab: event.metaKey || event.ctrlKey || event.shiftKey,
    });
  }

  async function handleCopy(
    value: string,
    setState: (value: CopyLabel) => void,
    timerRef: { current: number | null },
  ) {
    if (!value) {
      return;
    }

    setState((await copyText(value)) ? "Copied" : "Copy failed");
    if (timerRef.current !== null) {
      window.clearTimeout(timerRef.current);
    }
    timerRef.current = window.setTimeout(() => {
      timerRef.current = null;
      setState("Copy");
    }, 2000);
  }

  async function copyText(value: string) {
    if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
      try {
        await navigator.clipboard.writeText(value);
        return true;
      } catch {
        // Fall back to execCommand below.
      }
    }

    if (typeof document === "undefined") {
      return false;
    }

    const focusedElement = document.activeElement;
    const textarea = document.createElement("textarea");
    try {
      textarea.value = value;
      textarea.setAttribute("readonly", "");
      textarea.style.position = "fixed";
      textarea.style.opacity = "0";
      textarea.style.pointerEvents = "none";
      const dialog = detailsDialogRef.current;
      const copyContainer = dialog?.open ? dialog : document.body;
      copyContainer.appendChild(textarea);
      textarea.focus({ preventScroll: true });
      textarea.select();
      textarea.setSelectionRange(0, textarea.value.length);
      return document.execCommand("copy");
    } catch {
      return false;
    } finally {
      textarea.remove();
      if (focusedElement instanceof HTMLElement && focusedElement.isConnected) {
        focusedElement.focus({ preventScroll: true });
      }
    }
  }

  function requestControl() {
    if (socket.current?.readyState !== WebSocket.OPEN || requestingControl) {
      return;
    }

    socket.current.send(
      JSON.stringify({
        type: "control.request",
        payload: { leaseSeconds: 30 * 60 },
      }),
    );
    setRequestingControl(true);
  }

  function flashRequestControlButton() {
    const button = requestControlButtonRef.current;
    if (!button) {
      return;
    }

    const palette = pickFlashPalette();
    button.animate(
      [
        {
          transform: "scale(1)",
          borderColor: "rgba(56, 189, 248, 0.3)",
          boxShadow: "0 0 0 rgba(56, 189, 248, 0)",
        },
        {
          transform: "scale(1.01)",
          borderColor: palette.first,
          boxShadow: `0 0 0 2px ${palette.firstGlow}`,
        },
        {
          transform: "scale(1)",
          borderColor: palette.second,
          boxShadow: `0 0 0 3px ${palette.secondGlow}`,
        },
        {
          transform: "scale(1)",
          borderColor: "rgba(56, 189, 248, 0.3)",
          boxShadow: "0 0 0 rgba(56, 189, 248, 0)",
        },
      ],
      {
        duration: 520,
        easing: "ease-out",
      },
    );
  }

  const modeLabel = sessionStatus?.canWrite ? "Control granted" : "Read-only";
  const leaseLabel = formatDeadline(sessionStatus?.controlLeaseExpiresAt ?? null, now);
  const sessionExpiryLabel = formatDeadline(sessionStatus?.sessionExpiresAt ?? null, now);
  const connectionLabel = transportLabel(transportState);
  const canRequestControl =
    Boolean(sessionId) &&
    sessionStatus?.hostState === "online" &&
    !sessionStatus?.canWrite &&
    !sessionStatus?.hasPendingControlRequest &&
    sessionStatus?.controllerViewerId === null;

  let requestControlLabel = "Request control";
  if (sessionStatus?.canWrite) {
    requestControlLabel = "Control active";
  } else if (requestingControl) {
    requestControlLabel = "Request pending...";
  } else if (sessionStatus?.hasPendingControlRequest) {
    requestControlLabel = "Another request is pending";
  } else if (sessionStatus?.controllerViewerId) {
    requestControlLabel = "Another viewer is controlling";
  }

  let createSessionLabel = "Create session";
  if (creating) {
    createSessionLabel = "Creating...";
  } else if (sessionId) {
    createSessionLabel = "New session";
  }

  let accessDescription =
    "Viewers are read-only by default. Request control to type into the host shell.";
  if (transportState === "closed" || sessionStatus?.hostState === "offline") {
    accessDescription = "Session ended. Create a new session to continue.";
  } else if (transportState === "error" || transportState === "reconnecting") {
    accessDescription = "Connection lost. Reconnecting...";
  } else if (transportState === "connecting") {
    accessDescription = "Connecting to the session...";
  } else if (sessionStatus?.hostState === "reconnecting") {
    accessDescription = "Host disconnected. Waiting for the host to reconnect.";
  } else if (statusNote) {
    accessDescription = statusNote;
  } else if (sessionStatus?.canWrite) {
    accessDescription = leaseLabel
      ? t("Control is active. Lease valid until {time}.", { time: leaseLabel })
      : "Control is active.";
  } else if (requestingControl || sessionStatus?.pendingControlRequest) {
    accessDescription = "Control request sent. Waiting for host approval.";
  } else if (sessionStatus?.controllerViewerId) {
    accessDescription = "Another viewer currently controls the host shell.";
  } else if (sessionStatus?.hostState === "waiting") {
    accessDescription = "Waiting for the host to connect.";
  }

  const showSetup = !sessionId || sessionStatus?.hostState === "waiting";

  const setupFields = (
    <div className="session-fields">
      <div className="min-w-0">
        <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
          <h2 className="text-sm font-medium text-stone-200">{t("1. Run on your computer")}</h2>
          <div className="flex rounded-lg bg-white/5 p-0.5">
            <PlatformButton active={selectedPlatform === "macos"} label="macOS" onClick={() => setSelectedPlatform("macos")} />
            <PlatformButton active={selectedPlatform === "linux"} label="Linux" onClick={() => setSelectedPlatform("linux")} />
            <PlatformButton active={selectedPlatform === "windows"} label="Windows" onClick={() => setSelectedPlatform("windows")} />
          </div>
        </div>
        <div className="copy-field">
          <code>{platformCommand || t("Create a session to get your command.")}</code>
          <button className="workspace-button" disabled={!platformCommand} onClick={() => void handleCopy(platformCommand, setPlatformCopyLabel, platformCopyTimerRef)}>{t(platformCopyLabel)}</button>
        </div>
      </div>
      <div className="min-w-0">
        <h2 className="mb-2 text-sm font-medium text-stone-200">{t("2. Send this link")}</h2>
        <div className="copy-field">
          <code>{shareUrl || t("Your sharing link will appear here.")}</code>
          <button className="workspace-button" disabled={!shareUrl} onClick={() => void handleCopy(shareUrl, setShareCopyLabel, shareCopyTimerRef)}>{t(shareCopyLabel)}</button>
        </div>
      </div>
    </div>
  );

  return (
    <main className="terminal-workspace">
      <header className="workspace-toolbar">
        <a href="/" aria-label={`Shello · ${t("Go home")}`} title={t("Go home")} className="flex shrink-0 items-center gap-2 rounded-md transition-opacity hover:opacity-80 focus-visible:outline-2 focus-visible:outline-offset-4 focus-visible:outline-amber-400">
          <img src="/logo.svg" alt="" className="h-7 w-7" />
          <h1 className="text-base font-semibold tracking-tight text-amber-400">Shello</h1>
        </a>
        <span className="text-xs text-stone-400" role="status">{t(connectionLabel)}</span>
        <span className="hidden text-xs text-stone-500 sm:inline">{sessionId || t("One command. Share your shell.")}</span>
        <div className="workspace-actions">
          <button className="workspace-button" title={sessionId ? t("Create a session in a new tab") : t("Create session")} onClick={handleCreateSessionClick} onAuxClick={(event) => {
            if (event.button === 1) { event.preventDefault(); void createSession({ openInNewTab: true }); }
          }} disabled={creating}>{t(createSessionLabel)}</button>
          {sessionId && <>
            <button className="workspace-button" onClick={() => void handleCopy(shareUrl, setShareCopyLabel, shareCopyTimerRef)}>{shareCopyLabel === "Copy" ? t("Copy link") : t(shareCopyLabel)}</button>
            <button ref={requestControlButtonRef} className="workspace-button control-button" onClick={requestControl} disabled={!canRequestControl || requestingControl}>{t(requestControlLabel)}</button>
            <button className="workspace-button" aria-expanded={detailsOpen} aria-controls="session-details" aria-haspopup="dialog" onClick={() => setDetailsOpen(!detailsOpen)}>{detailsOpen ? t("Hide details") : t("Session details")}</button>
          </>}
        </div>
      </header>

      <dialog ref={detailsDialogRef} id="session-details" className="workspace-details" aria-labelledby="session-details-title"
        onCancel={() => setDetailsOpen(false)} onClose={() => setDetailsOpen(false)}
        onClick={(event) => {
          if (event.target !== event.currentTarget) return;
          const bounds = event.currentTarget.getBoundingClientRect();
          if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) setDetailsOpen(false);
        }}>
        <div className="drawer-heading">
          <div><h2 id="session-details-title" className="text-lg font-semibold">{t("Session details")}</h2><p className="mt-1 font-mono text-xs text-stone-500">{sessionId}</p></div>
          <button type="button" className="workspace-button" onClick={() => setDetailsOpen(false)} autoFocus aria-label={t("Close session details")}>{t("Close \u00d7")}</button>
        </div>
        {setupFields}
        <dl className="drawer-metadata">
          <div><dt className="inline text-stone-500">{t("Host")}</dt><dd className="inline">{t(sessionStatus?.hostState ?? "waiting")}</dd></div>
          <div><dt className="inline text-stone-500">{t("Viewers")}</dt><dd className="inline">{sessionStatus?.viewerCount ?? 0}</dd></div>
          <div><dt className="inline text-stone-500">{t("Control expires")}</dt><dd className="inline">{leaseLabel ?? t("Not granted")}</dd></div>
          <div><dt className="inline text-stone-500">{t("Session expires")}</dt><dd className="inline">{sessionExpiryLabel ?? t("Unknown")}</dd></div>
        </dl>
      </dialog>

      <section className="workspace-terminal" aria-label={t("Shared terminal")}>
        <div ref={terminalRef} className="absolute inset-0 overflow-hidden" />
        {showSetup && <div className="workspace-setup">
          <div className="setup-content">
            <p className="mb-3 text-xs font-medium uppercase tracking-[0.18em] text-amber-400">{t("Your shell, shared.")}</p>
            <h2 className="text-2xl font-semibold tracking-tight sm:text-3xl">{sessionId ? t("Run once. You're connected.") : t("One command. Share your shell.")}</h2>
            <p className="mt-3 text-sm leading-6 text-stone-400">{sessionId ? t("Run the command in your local terminal, then send the link to your collaborator.") : t("Share your local command line through a browser link. No manual installation.")}</p>
            {sessionId ? <div className="mt-7">{setupFields}</div> : <button className="workspace-button start-button" disabled={creating} onClick={handleCreateSessionClick}>{creating ? t("Creating...") : t("Start sharing")}<span aria-hidden="true"> →</span></button>}
            <p className="mt-5 text-xs leading-5 text-stone-500">{t("Viewers join in their browser. You approve who can type.")}</p>
          </div>
        </div>}
      </section>

      <footer className="workspace-status">
        <span className="shrink-0 text-stone-300">{connecting ? t("Connecting") : t(modeLabel)}</span>
        <p className="min-w-0 flex-1" role="status">{t(accessDescription)}</p>
        <span className="hidden shrink-0 sm:inline">{t("{count} viewers", { count: sessionStatus?.viewerCount ?? 0 })}</span>
      </footer>
    </main>
  );
}

async function handleBinarySocketMessage(
  data: unknown,
  ws: WebSocket,
  handleTtyOutput: (payload: Uint8Array) => void,
) {
  const buffer = await binarySocketDataToArrayBuffer(data);
  if (!buffer) {
    ws.close(1003, "unsupported binary message container");
    return;
  }

  handleBinaryMessage(buffer, handleTtyOutput);
}

function handleBinaryMessage(buffer: ArrayBuffer, handleTtyOutput: (payload: Uint8Array) => void) {
  const bytes = new Uint8Array(buffer);
  if (bytes.length === 0) {
    return;
  }

  switch (bytes[0]) {
    case BinaryMessageType.ttyOutput:
      handleTtyOutput(bytes.subarray(1));
      return;
    default:
      return;
  }
}

function sendTerminalInput(ws: WebSocket, value: string) {
  if (ws.readyState !== WebSocket.OPEN) {
    return false;
  }

  const bytes = textEncoder.encode(value);
  const frames = Math.ceil(bytes.byteLength / MAX_STDIN_FRAME_BYTES);
  if (
    ws.bufferedAmount + bytes.byteLength + frames >
    MAX_WEBSOCKET_BUFFERED_INPUT_BYTES
  ) {
    return false;
  }

  try {
    for (let offset = 0; offset < bytes.byteLength; offset += MAX_STDIN_FRAME_BYTES) {
      const chunk = bytes.subarray(offset, offset + MAX_STDIN_FRAME_BYTES);
      ws.send(encodeBinaryMessage(BinaryMessageType.stdin, chunk));
    }
    return true;
  } catch {
    return false;
  }
}

function readSessionId() {
  if (typeof window === "undefined") {
    return null;
  }

  const match = window.location.pathname.match(/^\/s\/([23456789abcdefghjkmnpqrstuvwxyz]{3}-[23456789abcdefghjkmnpqrstuvwxyz]{3})$/);
  return match?.[1] ?? null;
}

function PlatformButton({
  active,
  label,
  onClick,
}: {
  active: boolean;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`rounded-md px-2 py-1.5 text-xs transition ${
        active
          ? "bg-white text-stone-950 shadow-sm"
          : "text-stone-400 hover:text-stone-200"
      }`}
    >
      {label}
    </button>
  );
}

function detectPlatformTab(): PlatformTab {
  if (typeof window === "undefined") {
    return "macos";
  }

  const platform = window.navigator.userAgent.toLowerCase();
  if (platform.includes("win")) {
    return "windows";
  }
  if (platform.includes("linux")) {
    return "linux";
  }
  return "macos";
}

function viewerSocketURL(sessionId: string) {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const url = new URL(`${protocol}//${window.location.host}/api/session/${sessionId}/viewer`);
  const viewerToken = readViewerToken(sessionId);
  if (viewerToken) {
    url.searchParams.set("viewerToken", viewerToken);
  }
  return url.toString();
}

function sessionStatusURL(sessionId: string) {
  const url = new URL(`/api/session/${sessionId}`, window.location.origin);
  const viewerToken = readViewerToken(sessionId);
  if (viewerToken) {
    url.searchParams.set("viewerToken", viewerToken);
  }
  return url.toString();
}

function buildSessionInfo(sessionId: string | null): SessionInfo | null {
  if (!sessionId || typeof window === "undefined") {
    return null;
  }

  return normalizeSessionInfo({
    sessionId,
    viewerUrl: `/s/${sessionId}`,
    hostWebSocketUrl: `/api/session/${sessionId}/host`,
    viewerWebSocketUrl: `/api/session/${sessionId}/viewer`,
  });
}

function normalizeSessionInfo(info: SessionInfo): SessionInfo {
  if (typeof window === "undefined") {
    return info;
  }

  const viewerUrl = new URL(info.viewerUrl, window.location.origin).toString();
  const origin = new URL(viewerUrl).origin;
  const viewerProtocol = origin.startsWith("https:") ? "wss:" : "ws:";
  const hostProtocol = viewerProtocol;

  return {
    sessionId: info.sessionId,
    viewerUrl,
    hostWebSocketUrl: normalizeWebSocketURL(info.hostWebSocketUrl, hostProtocol),
    viewerWebSocketUrl: normalizeWebSocketURL(info.viewerWebSocketUrl, viewerProtocol),
  };
}

function normalizeWebSocketURL(value: string, protocol: string) {
  if (value.startsWith("ws://") || value.startsWith("wss://")) {
    return value;
  }

  if (typeof window === "undefined") {
    return value;
  }

  const resolved = new URL(value, window.location.origin);
  resolved.protocol = protocol;
  return resolved.toString();
}

function viewerTokenStorageKey(sessionId: string) {
  return `shello.viewerToken.${sessionId}`;
}

function readViewerToken(sessionId: string) {
  if (typeof window === "undefined") {
    return null;
  }
  const key = viewerTokenStorageKey(sessionId);
  const token = window.sessionStorage.getItem(key);
  if (token) return token;
  // Preserve viewer identity and control when upgrading an existing browser session.
  const legacyToken = window.sessionStorage.getItem(`ttys.viewerToken.${sessionId}`);
  if (legacyToken) window.sessionStorage.setItem(key, legacyToken);
  return legacyToken;
}

function storeViewerToken(sessionId: string, token: string | null) {
  if (!token || typeof window === "undefined") {
    return;
  }
  window.sessionStorage.setItem(viewerTokenStorageKey(sessionId), token);
}

function formatDeadline(timestamp: number | null, now: number) {
  if (!timestamp) {
    return null;
  }

  const remainingMs = timestamp - now;
  if (remainingMs <= 0) {
    return t("Expired");
  }

  const remainingSeconds = Math.ceil(remainingMs / 1000);
  const minutes = Math.floor(remainingSeconds / 60);
  const seconds = remainingSeconds % 60;
  const relative =
    minutes > 0 ? t("{minutes}m {seconds}s left", { minutes, seconds: String(seconds).padStart(2, "0") }) : t("{seconds}s left", { seconds });

  return `${new Date(timestamp).toLocaleTimeString(locale)} (${relative})`;
}

function transportLabel(value: string) {
  switch (value) {
    case "connected":
      return "Live";
    case "connecting":
      return "Connecting";
    case "reconnecting":
      return "Reconnecting";
    case "closed":
      return "Offline";
    case "error":
      return "Connection issue";
    default:
      return "Idle";
  }
}

function shouldRetrySessionStatus(response: Response) {
  return response.status === 429 || response.status >= 500;
}

function pickFlashPalette(): FlashPalette {
  const firstHue = Math.floor(Math.random() * 360);
  const firstSaturation = Math.floor(Math.random() * 101);
  const firstLightness = 35 + Math.floor(Math.random() * 41);
  const secondHue = Math.floor(Math.random() * 360);
  const secondSaturation = Math.floor(Math.random() * 101);
  const secondLightness = 35 + Math.floor(Math.random() * 41);

  return {
    first: `hsl(${firstHue} ${firstSaturation}% ${firstLightness}%)`,
    firstGlow: `hsl(${firstHue} ${firstSaturation}% ${firstLightness}% / 0.22)`,
    second: `hsl(${secondHue} ${secondSaturation}% ${secondLightness}%)`,
    secondGlow: `hsl(${secondHue} ${secondSaturation}% ${secondLightness}% / 0.18)`,
  };
}

function parseControlFrame(value: string): Record<string, unknown> | null {
  try {
    const parsed = JSON.parse(value) as Record<string, unknown>;
    if (typeof parsed.type === "string") {
      return parsed;
    }
  } catch {
    return null;
  }

  return null;
}
