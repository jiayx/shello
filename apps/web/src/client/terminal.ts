import { t } from "./i18n";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
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
  fit: () => { cols: number; rows: number };
  onData: (handler: (value: string) => void) => () => void;
  reset: () => void;
  resize: (size: TerminalSize) => void;
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
  viewport.style.width = "100%";
  viewport.style.height = "100%";
  viewport.style.overflow = "auto";
  surface.style.width = "100%";
  surface.style.height = "100%";
  container.appendChild(viewport);
  viewport.appendChild(surface);

  const baseFontSize = 14;
  const terminal = new Terminal({
    cursorBlink: true,
    fontFamily: '"SFMono-Regular", ui-monospace, monospace',
    fontSize: baseFontSize,
    theme: {
      background: "#111111",
      foreground: "#f5f5f4",
      cursor: "#fbbf24",
      selectionBackground: "#44403c",
    },
  });
  const fit = new FitAddon();

  terminal.loadAddon(fit);
  terminal.open(surface);
  // Keep host dimensions intact; scroll when the minimum readable font no longer fits.
  function syncSurfaceSize() {
    const screen = terminal.element?.querySelector<HTMLElement>(".xterm-screen");
    if (!screen) return;
    surface.style.minWidth = `${screen.offsetWidth + 16}px`;
    surface.style.minHeight = `${screen.offsetHeight}px`;
  }
  const renderSubscription = terminal.onRender(syncSurfaceSize);
  if (initialSize) {
    terminal.resize(initialSize.cols, initialSize.rows);
  }

  const writeBatcher = new TerminalWriteBatcher((value) => terminal.write(value));
  let resizeTimer = 0;
  let initialResizeFrame = 0;
  let lastWidth = -1;
  let lastHeight = -1;
  let currentSize: TerminalSize = initialSize ?? { cols: terminal.cols, rows: terminal.rows };

  function scheduleFit(width: number, height: number) {
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
    scheduleFit(Math.round(viewport.clientWidth), Math.round(viewport.clientHeight));
  }

  function updateScale() {
    if (currentSize.cols <= 0 || currentSize.rows <= 0) {
      return;
    }

    terminal.options.fontSize = baseFontSize;
    const cellSize = measureCellSize();
    if (!cellSize) {
      return;
    }

    const scale = Math.min(
      viewport.clientWidth / (currentSize.cols * cellSize.width),
      viewport.clientHeight / (currentSize.rows * cellSize.height),
      1,
    );
    terminal.options.fontSize = Math.max(8, Math.floor(baseFontSize * scale * 100) / 100);
  }

  function measureCellSize() {
    const rowsElement = terminal.element?.querySelector<HTMLElement>(".xterm-rows");
    const rowElement = rowsElement?.firstElementChild as HTMLElement | null;
    if (!rowsElement || !rowElement || terminal.cols <= 0 || terminal.rows <= 0) {
      return null;
    }

    const width = rowsElement.scrollWidth / terminal.cols;
    const height = rowElement.getBoundingClientRect().height;
    if (width <= 0 || height <= 0) {
      return null;
    }
    return { width, height };
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
    fit() {
      fit.fit();
      updateScale();
      return {
        cols: terminal.cols,
        rows: terminal.rows,
      };
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
