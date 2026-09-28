type EventParameters = Record<string, string | number | boolean>;
type Gtag = (...args: unknown[]) => void;

declare global {
  interface Window {
    dataLayer?: IArguments[];
    gtag?: Gtag;
  }
}

let initialized = false;

export function initAnalytics() {
  const measurementId = import.meta.env.VITE_GA_MEASUREMENT_ID?.trim();
  if (initialized || !import.meta.env.PROD || !measurementId) return;

  window.dataLayer = window.dataLayer || [];
  window.gtag = function () { window.dataLayer!.push(arguments); };
  initialized = true;
  window.gtag("js", new Date());
  // GA owns initial and history-based pageviews; do not also send them manually.
  window.gtag("config", measurementId);

  const script = document.createElement("script");
  script.async = true;
  script.src = `https://www.googletagmanager.com/gtag/js?id=${encodeURIComponent(measurementId)}`;
  document.head.appendChild(script);
}

// Callers should only pass aggregate metadata, never terminal content or codes.
export function trackEvent(name: string, parameters: EventParameters = {}) {
  if (!initialized) return;
  window.gtag!("event", name, parameters);
}
