import { t } from "./i18n";
import { useEffect, useRef, useState } from "react";
import { mountTerminal, type TerminalController, type TerminalSize } from "./terminal";

const defaultSize: TerminalSize = { cols: 120, rows: 30 };

export function ReplayDebug() {
  const terminalRef = useRef<HTMLDivElement | null>(null);
  const terminal = useRef<TerminalController | null>(null);
  const replayTimer = useRef<number | null>(null);
  const [size, setSize] = useState(defaultSize);
  const [bytes, setBytes] = useState<Uint8Array | null>(null);
  const [status, setStatus] = useState(t("Load a SHELLO_TRACE file to replay raw PTY output."));

  useEffect(() => {
    if (!terminalRef.current) {
      return;
    }

    terminal.current = mountTerminal(terminalRef.current, size);
    return () => {
      if (replayTimer.current !== null) {
        window.clearInterval(replayTimer.current);
      }
      terminal.current?.dispose();
      terminal.current = null;
    };
  }, []);

  useEffect(() => {
    terminal.current?.resize(size);
  }, [size]);

  async function handleFile(event: React.ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    if (!file) {
      return;
    }

    const loaded = new Uint8Array(await file.arrayBuffer());
    setBytes(loaded);
    setStatus(t("Loaded {name} ({count} bytes).", { name: file.name, count: loaded.byteLength }));
  }

  function replayAll() {
    if (!bytes) {
      setStatus(t("Load a trace first."));
      return;
    }
    stopReplay();
    terminal.current?.reset();
    terminal.current?.resize(size);
    terminal.current?.write(bytes);
    setStatus(t("Replayed {count} bytes at {cols}x{rows}.", { count: bytes.byteLength, ...size }));
  }

  function replaySlow() {
    if (!bytes) {
      setStatus(t("Load a trace first."));
      return;
    }
    stopReplay();
    terminal.current?.reset();
    terminal.current?.resize(size);

    let offset = 0;
    replayTimer.current = window.setInterval(() => {
      const chunk = bytes.subarray(offset, offset + 256);
      terminal.current?.write(chunk);
      offset += chunk.byteLength;
      setStatus(t("Replaying {current} / {total} bytes.", { current: Math.min(offset, bytes.byteLength), total: bytes.byteLength }));
      if (offset >= bytes.byteLength) {
        stopReplay();
        setStatus(t("Finished slow replay at {cols}x{rows}.", size));
      }
    }, 16);
  }

  function stopReplay() {
    if (replayTimer.current !== null) {
      window.clearInterval(replayTimer.current);
      replayTimer.current = null;
    }
  }

  return (
    <main className="min-h-screen bg-stone-950 p-6 text-stone-100">
      <section className="mx-auto flex max-w-7xl flex-col gap-4">
        <div className="flex flex-wrap items-center gap-3 rounded-2xl border border-white/10 bg-black/30 p-4">
          <a href="/" className="text-sm text-amber-300 hover:text-amber-200">
            Shello
          </a>
          <span className="text-sm text-stone-500">{t("Raw PTY replay")}</span>
          <label className="text-sm text-stone-300">
            {t("Trace file")}
            <input className="ml-2 text-sm" type="file" onChange={handleFile} />
          </label>
          <NumberField label={t("Cols")} value={size.cols} onChange={(cols) => setSize({ ...size, cols })} />
          <NumberField label={t("Rows")} value={size.rows} onChange={(rows) => setSize({ ...size, rows })} />
          <button className="rounded-lg bg-white px-3 py-1.5 text-sm text-stone-950" onClick={replayAll}>
            {t("Replay")}
          </button>
          <button className="rounded-lg border border-white/10 px-3 py-1.5 text-sm" onClick={replaySlow}>
            {t("Slow replay")}
          </button>
          <button className="rounded-lg border border-white/10 px-3 py-1.5 text-sm" onClick={stopReplay}>
            {t("Stop")}
          </button>
          <p className="basis-full text-sm text-stone-400">{status}</p>
        </div>
        <div
          ref={terminalRef}
          className="h-[calc(100vh-9rem)] min-h-96 overflow-hidden rounded-2xl border border-white/10 bg-[#111111]"
        />
      </section>
    </main>
  );
}

function NumberField({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number;
  onChange: (value: number) => void;
}) {
  return (
    <label className="text-sm text-stone-300">
      {label}
      <input
        className="ml-2 w-20 rounded-lg border border-white/10 bg-black/30 px-2 py-1 text-stone-100"
        type="number"
        min="1"
        max="1000"
        value={value}
        onChange={(event) => onChange(clampSize(Number(event.target.value)))}
      />
    </label>
  );
}

function clampSize(value: number) {
  if (!Number.isFinite(value)) {
    return 1;
  }
  return Math.max(1, Math.min(Math.trunc(value), 1000));
}
