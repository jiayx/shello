import { t, locale } from "./i18n";
import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./app";
import { ReplayDebug } from "./replay-debug";
import "./styles.css";

function Router() {
  if (window.location.pathname === "/debug/replay") {
    return <ReplayDebug />;
  }

  if (!isKnownRoute(window.location.pathname)) {
    return <NotFound />;
  }

  return <App />;
}

function isKnownRoute(pathname: string) {
  return pathname === "/" || /^\/s\/[23456789abcdefghjkmnpqrstuvwxyz]{3}-[23456789abcdefghjkmnpqrstuvwxyz]{3}$/.test(pathname);
}

function NotFound() {
  return (
    <main className="flex min-h-screen items-center justify-center bg-stone-950 px-6 text-stone-100">
      <section className="w-full max-w-md rounded-3xl border border-white/10 bg-black/30 p-6">
        <p className="text-xs uppercase tracking-[0.32em] text-amber-400">Shello</p>
        <h1 className="mt-4 text-2xl font-medium text-stone-100">{t("Page not found")}</h1>
        <p className="mt-3 text-sm leading-6 text-stone-400">
          {t("This link does not match an active Shello route.")}
        </p>
        <a
          href="/"
          className="mt-6 inline-flex rounded-xl border border-white/10 bg-white px-4 py-2 text-sm font-medium text-stone-950 transition hover:bg-stone-200"
        >
          {t("Go home")}
        </a>
      </section>
    </main>
  );
}

document.documentElement.lang = locale;
document.title = t("Shello — Live terminal sharing");
document.querySelector('meta[name="description"]')?.setAttribute("content", t("Share your local shell with one command. No manual installation. Viewers join in a browser and request control with host approval."));

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Router />
  </React.StrictMode>,
);
