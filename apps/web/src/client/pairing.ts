const codePattern = /^[23456789abcdefghjkmnpqrstuvwxyz]{6}$/;

export function parsePairingInput(value: string, origin: string): string | null {
  let code = value.trim();
  if (/^https?:\/\//i.test(code)) {
    try {
      const url = new URL(code);
      if (url.origin !== origin) return null;
      const match = url.pathname.match(/^\/s\/([^/]+)\/?$/);
      if (!match) return null;
      code = match[1];
    } catch {
      return null;
    }
  }
  code = code.toLowerCase();
  if (code.length === 7 && code[3] === "-") code = code.slice(0, 3) + code.slice(4);
  if (!codePattern.test(code)) return null;
  return `${code.slice(0, 3)}-${code.slice(3)}`;
}
