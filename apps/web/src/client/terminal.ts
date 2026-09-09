import { t } from "./i18n";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

export type TerminalSize = {
  cols: number;
  rows: number;
};

export type HostTerminalProfile = {
  platform: "unix" | "windows";
  pty?: "conpty";
};

export type TerminalController = {
  dispose: () => void;
  focus: () => void;
  onData: (handler: (value: string) => void) => () => void;
  reset: () => void;
  resize: (size: TerminalSize) => void;
  setCursorVisible: (visible: boolean) => void;
  setHostTerminalProfile: (profile: HostTerminalProfile | null) => void;
  write: (value: string | Uint8Array) => void;
  writeln: (value: string) => void;
};

Terminal.strings.promptLabel = t("Terminal input");
Terminal.strings.tooMuchOutput = t("Too much output to announce, navigate to rows manually to read");

const terminalWriteFlushDelayMs = 8;
const terminalWriteMaxBatchBytes = 64 * 1024;
const terminalWriteMaxPendingBytes = 4 * 1024 * 1024;
const textEncoder = new TextEncoder();

export function mountTerminal(container: HTMLElement, initialSize?: TerminalSize): TerminalController {
  const viewport = document.createElement("div");
  const surface = document.createElement("div");
  viewport.className = "shello-terminal-viewport";
  surface.className = "shello-terminal-surface";
  viewport.dataset.cursorHidden = "true";
  viewport.style.width = "100%";
  viewport.style.height = "100%";
  viewport.style.overflow = "hidden";
  viewport.style.position = "relative";
  surface.style.width = "100%";
  surface.style.height = "100%";
  container.appendChild(viewport);
  viewport.appendChild(surface);
  surface.style.transformOrigin = "center";
  surface.style.position = "absolute";
  surface.style.left = "50%";
  surface.style.top = "50%";

  const baseFontSize = 14;
  const terminal = new Terminal({
    scrollOnEraseInDisplay: true,
    cursorBlink: true,
    fontFamily: '"SFMono-Regular", ui-monospace, monospace',
    fontSize: baseFontSize,
    theme: {
      background: "#111111",
      foreground: "#f5f5f4",
      cursor: "#fbbf24",
      selectionBackground: "#44403c",
      // xterm accepts hex alpha colors, but not the CSS keyword "transparent".
      scrollbarSliderBackground: "#00000000",
      scrollbarSliderHoverBackground: "#00000000",
      scrollbarSliderActiveBackground: "#00000000",
    },
  });
  terminal.open(surface);
  const renderSubscription = terminal.onRender(() => updateScale());
  if (initialSize) {
    terminal.resize(initialSize.cols, initialSize.rows);
  }

  const writeBatcher = new TerminalWriteBatcher((value) => terminal.write(value));
  let resizeTimer = 0;
  let initialResizeFrame = 0;
  let lastWidth = -1;
  let lastHeight = -1;
  let currentSize: TerminalSize = initialSize ?? { cols: terminal.cols, rows: terminal.rows };

  function scheduleScale(width: number, height: number) {
    if (width === lastWidth && height === lastHeight) {
      return;
    }

    lastWidth = width;
    lastHeight = height;

    if (resizeTimer) {
      window.clearTimeout(resizeTimer);
    }

    resizeTimer = window.setTimeout(() => {
      resizeTimer = 0;
      updateScale();
    }, 160);
  }

  function handleWindowResize() {
    const bounds = viewport.getBoundingClientRect();
    scheduleScale(bounds.width, bounds.height);
  }

  function updateScale() {
    if (currentSize.cols <= 0 || currentSize.rows <= 0) {
      return;
    }

    // Measure the rendered screen, not individual text rows. This also works
    // with alternate-screen TUIs and renderers that do not expose .xterm-rows.
    const screen = terminal.element?.querySelector<HTMLElement>(".xterm-screen");
    if (!screen || !screen.offsetWidth || !screen.offsetHeight) return;
    const width = screen.offsetWidth + 16;
    const height = screen.offsetHeight;
    // clientWidth/clientHeight round fractional CSS pixels and can overshoot the
    // available area by a pixel, producing an extra scrollbar or clipping a row.
    const available = viewport.getBoundingClientRect();
    if (available.width <= 0 || available.height <= 0) return;
    // Keep an 8px visual gutter without changing the host terminal dimensions.
    const scale = Math.min(
      Math.max(1, available.width - 16) / width,
      Math.max(1, available.height - 16) / height,
    );
    viewport.style.setProperty("--terminal-scale", String(scale));
    surface.style.width = `${width}px`;
    surface.style.height = `${height}px`;
    surface.style.transform = `translate(-50%, -50%) scale(${scale})`;
  }

  window.addEventListener("resize", handleWindowResize);
  window.visualViewport?.addEventListener("resize", handleWindowResize);
  const resizeObserver =
    typeof ResizeObserver === "undefined"
      ? null
      : new ResizeObserver(() => {
          handleWindowResize();
        });
  resizeObserver?.observe(viewport);
  initialResizeFrame = window.requestAnimationFrame(handleWindowResize);

  return {
    dispose() {
      window.removeEventListener("resize", handleWindowResize);
      window.visualViewport?.removeEventListener("resize", handleWindowResize);
      resizeObserver?.disconnect();
      if (initialResizeFrame) {
        window.cancelAnimationFrame(initialResizeFrame);
      }
      if (resizeTimer) {
        window.clearTimeout(resizeTimer);
      }
      writeBatcher.dispose();
      renderSubscription.dispose();
      terminal.dispose();
      viewport.remove();
    },
    focus() {
      terminal.focus();
    },
    onData(handler) {
      const disposable = terminal.onData(handler);
      return () => {
        disposable.dispose();
      };
    },
    reset() {
      writeBatcher.flush();
      terminal.reset();
      updateScale();
    },
    resize(size) {
      currentSize = size;
      if (terminal.cols !== size.cols || terminal.rows !== size.rows) {
        terminal.resize(size.cols, size.rows);
      }
      updateScale();
    },
    setCursorVisible(visible) {
      // xterm resolves theme colors to opaque colors; CSS suppresses the actual
      // DOM cursor decoration while leaving terminal text and host modes intact.
      viewport.dataset.cursorHidden = String(!visible);
    },
    setHostTerminalProfile(profile) {
      terminal.options.windowsPty =
        profile?.platform === "windows" && profile.pty === "conpty"
          ? { backend: "conpty" }
          : undefined;
    },
    write(value) {
      writeBatcher.write(value);
    },
    writeln(value) {
      writeBatcher.flush();
      terminal.writeln(value);
    },
  };
}

class TerminalWriteBatcher {
  private pending: Array<string | Uint8Array> = [];
  private pendingBytes = 0;
  private timer: number | null = null;
  private lastWrite = 0;

  constructor(private readonly writeDirect: (value: string | Uint8Array) => void) {}

  write(value: string | Uint8Array) {
    const size = byteLength(value);
    if (size === 0) {
      return;
    }

    const now = performance.now();
    if (this.pending.length === 0 && shouldWriteImmediately(this.lastWrite, now)) {
      this.lastWrite = now;
      this.writeDirect(value);
      return;
    }

    if (this.pendingBytes + size > terminalWriteMaxPendingBytes) {
      this.flush();
    }

    this.pending.push(value);
    this.pendingBytes += size;
    if (this.pendingBytes >= terminalWriteMaxBatchBytes) {
      this.flush();
      return;
    }

    this.scheduleFlush();
  }

  flush() {
    if (this.timer !== null) {
      window.clearTimeout(this.timer);
      this.timer = null;
    }
    if (this.pending.length === 0) {
      return;
    }

    for (const value of coalesceWrites(this.pending, this.pendingBytes)) {
      this.writeDirect(value);
    }

    this.pending = [];
    this.pendingBytes = 0;
    this.lastWrite = performance.now();
  }

  dispose() {
    this.flush();
  }

  private scheduleFlush() {
    if (this.timer !== null) {
      return;
    }

    this.timer = window.setTimeout(() => {
      this.timer = null;
      this.flush();
    }, terminalWriteFlushDelayMs);
  }
}

function shouldWriteImmediately(lastWrite: number, now: number) {
  return lastWrite === 0 || now - lastWrite >= terminalWriteFlushDelayMs;
}

function byteLength(value: string | Uint8Array) {
  return typeof value === "string" ? textEncoder.encode(value).byteLength : value.byteLength;
}

function coalesceWrites(
  values: Array<string | Uint8Array>,
  totalBytes: number,
): Array<string | Uint8Array> {
  if (values.every((value) => value instanceof Uint8Array)) {
    const bytes = new Uint8Array(totalBytes);
    let offset = 0;
    for (const value of values) {
      bytes.set(value, offset);
      offset += value.byteLength;
    }
    return [bytes];
  }

  return values;
}
